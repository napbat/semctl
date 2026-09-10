//! Verified local application and undo for server-produced workspace edit plans.
//!
//! The server is plan-only. This module is the single filesystem mutation
//! boundary: it re-authorizes the plan against the current codebase metadata,
//! verifies the opaque checkout identity and every preimage hash, stages every
//! postimage, then swaps the files with rollback backups. Preimages are retained
//! in semctl's private config directory for hash-guarded undo.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};

use crate::client::{Client, api};

mod formatter;
mod paths;
mod transaction;

const PLAN_SCHEMA_VERSION: u32 = 1;
const MAX_FILES: usize = 256;
const MAX_EDITS: usize = 4096;
const MAX_REPLACEMENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFile {
    pub path: String,
    pub content_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    pub plan_id: String,
    pub operation: String,
    pub changed_files: Vec<AppliedFile>,
    pub already_applied: bool,
    pub already_undone: bool,
    pub watcher_active: bool,
    pub sync_state: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditHistory {
    schema_version: u32,
    plan_id: String,
    operation: String,
    codebase_id: String,
    source_identity: String,
    files: Vec<HistoryFile>,
    undone: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    path: String,
    preimage_hash: String,
    preimage_base64: String,
    postimage_hash: String,
}

struct PreparedFile {
    path: String,
    // This path is a label for formatters and diagnostics. Mutations use the
    // directory capability in `location`.
    target: PathBuf,
    location: paths::Target,
    preimage: Vec<u8>,
    postimage: Vec<u8>,
    postimage_hash: String,
    temporary: PathBuf,
    backup: PathBuf,
}

impl PreparedFile {
    fn recovery_path(&self) -> PathBuf {
        self.backup.with_extension("edit")
    }

    fn verify_requested_path(&self) -> Result<()> {
        self.location
            .verify_requested_path(&self.path, &self.target)
    }
}

/// Apply one plan to the client's bound checkout. A formatter step is executed
/// only when the caller explicitly approves it through `run_formatter`.
pub async fn apply(
    client: &Client,
    plan: &api::WorkspaceEditPlan,
    run_formatter: bool,
    watcher_active: bool,
) -> Result<ApplyOutcome> {
    validate_plan_contract(plan, run_formatter)?;
    let root = checkout_root(client)?;
    let checkout = paths::Checkout::open(&root)?;
    validate_server_context(client, &root, plan).await?;

    let prepare_plan = plan.clone();
    let preparation = tokio::task::spawn_blocking(move || {
        prepare_apply(&checkout, &prepare_plan, watcher_active)
    })
    .await
    .context("prepare edit task failed")??;
    let mut prepared = match preparation {
        ApplyPreparation::Complete(outcome) => return Ok(outcome),
        ApplyPreparation::Pending(files) => files,
    };
    if let Some(step) = &plan.formatter {
        formatter::format(&root, step, &mut prepared).await?;
        validate_server_context(client, &root, plan).await?;
    }
    let plan = plan.clone();
    // Once commit starts, the blocking task owns the complete transaction. A
    // cancelled MCP request cannot interrupt it between filesystem replacements.
    tokio::task::spawn_blocking(move || commit_apply(&root, &plan, &mut prepared, watcher_active))
        .await
        .context("commit edit task failed")?
}

enum ApplyPreparation {
    Complete(ApplyOutcome),
    Pending(Vec<PreparedFile>),
}

fn prepare_apply(
    checkout: &Arc<paths::Checkout>,
    plan: &api::WorkspaceEditPlan,
    watcher_active: bool,
) -> Result<ApplyPreparation> {
    let path = history_path(&plan.plan_id)?;
    let directory = path.parent().context("edit history path has no parent")?;
    let _lock = lock_checkout(directory, &plan.source_identity)?;
    if let Some(outcome) = existing_apply(&checkout.path, plan, &path, watcher_active)? {
        return Ok(ApplyPreparation::Complete(outcome));
    }
    Ok(ApplyPreparation::Pending(prepare_plan(checkout, plan)?))
}

fn existing_apply(
    root: &Path,
    plan: &api::WorkspaceEditPlan,
    path: &Path,
    watcher_active: bool,
) -> Result<Option<ApplyOutcome>> {
    if path.exists() {
        let history = read_history(path)?;
        ensure!(
            !history.undone,
            "plan {} was already undone; request a fresh plan",
            plan.plan_id
        );
        ensure_no_retained_recovery(root, &history)?;
        ensure!(
            history_matches(root, &history, false)?,
            "edit history for plan {} exists but the checkout no longer matches its postimages",
            plan.plan_id
        );
        return Ok(Some(outcome_from_history(
            &history,
            true,
            false,
            watcher_active,
        )));
    }
    Ok(None)
}

fn commit_apply(
    root: &Path,
    plan: &api::WorkspaceEditPlan,
    prepared: &mut [PreparedFile],
    watcher_active: bool,
) -> Result<ApplyOutcome> {
    let path = history_path(&plan.plan_id)?;
    let directory = path.parent().context("edit history path has no parent")?;
    let _lock = lock_checkout(directory, &plan.source_identity)?;
    // Another request can complete the same plan while formatting or waiting
    // for the checkout lock. Replay must still return the recorded outcome.
    if let Some(outcome) = existing_apply(root, plan, &path, watcher_active)? {
        return Ok(outcome);
    }
    let history = history_from(plan, prepared);
    create_history(&path, &history)?;
    let transaction = match transaction::Transaction::commit(prepared) {
        Ok(transaction) => transaction,
        Err(error) => {
            // Keep the history when conflicts retain recovery data. A retry
            // must not treat a partially recovered transaction as a fresh plan.
            if !transaction::recovery_required(&error) {
                let _ = fs::remove_file(&path);
            }
            return Err(error);
        }
    };
    transaction.finish()?;
    Ok(outcome_from_history(&history, false, false, watcher_active))
}

/// Restore retained preimages while every current file still matches the
/// postimage recorded by [`apply`]. The filesystem transaction completes even
/// when the awaiting request is cancelled.
pub async fn undo(client: &Client, plan_id: &str, watcher_active: bool) -> Result<ApplyOutcome> {
    validate_plan_id(plan_id)?;
    let root = checkout_root(client)?;
    let client = client.clone();
    let plan_id = plan_id.to_string();
    tokio::task::spawn_blocking(move || undo_local(&client, &root, &plan_id, watcher_active))
        .await
        .context("undo edit task failed")?
}

fn undo_local(
    client: &Client,
    root: &Path,
    plan_id: &str,
    watcher_active: bool,
) -> Result<ApplyOutcome> {
    let directory = crate::config::edit_history_dir()?;
    let source = crate::codebase::checkout_source_id(root)?;
    let _lock = lock_checkout(&directory, &source)?;
    let path = history_path(plan_id)?;
    let mut history = read_history(&path)
        .with_context(|| format!("no retained edit history for plan {plan_id}"))?;
    validate_history_context(client, root, &history)?;
    ensure_no_retained_recovery(root, &history)?;
    if history.undone {
        ensure!(
            history_matches(root, &history, true)?,
            "plan {plan_id} is marked undone but its files no longer match the retained preimages"
        );
        return Ok(outcome_from_history(&history, false, true, watcher_active));
    }
    let checkout = paths::Checkout::open(root)?;
    let prepared = prepare_undo(&checkout, &history)?;
    let transaction = transaction::Transaction::commit(&prepared)?;
    history.undone = true;
    if let Err(error) = write_history(&path, &history) {
        return Err(transaction.rollback_error(error));
    }
    transaction.finish()?;
    Ok(outcome_from_history(&history, false, false, watcher_active))
}

fn validate_plan_contract(plan: &api::WorkspaceEditPlan, run_formatter: bool) -> Result<()> {
    ensure!(
        plan.schema_version == PLAN_SCHEMA_VERSION,
        "unsupported workspace edit plan schema {}",
        plan.schema_version
    );
    validate_plan_id(&plan.plan_id)?;
    ensure!(
        plan.applicable,
        "plan is not applicable: {}",
        plan.refusal_reasons.join("; ")
    );
    ensure!(plan.graph_complete, "plan graph is partial");
    ensure!(
        plan.provider_generations_current,
        "plan provider generations are stale"
    );
    ensure!(!plan.files.is_empty(), "plan contains no file edits");
    ensure!(
        plan.files.len() <= MAX_FILES,
        "plan exceeds {MAX_FILES} files"
    );
    ensure!(
        plan.files
            .iter()
            .map(|file| file.edits.len())
            .sum::<usize>()
            <= MAX_EDITS,
        "plan exceeds {MAX_EDITS} edits"
    );
    if plan.formatter.is_some() && !run_formatter {
        bail!("plan includes a formatter step; explicit runFormatter approval is required");
    }
    Ok(())
}

async fn validate_server_context(
    client: &Client,
    root: &Path,
    plan: &api::WorkspaceEditPlan,
) -> Result<()> {
    ensure!(
        client.codebase()? == plan.codebase_id,
        "plan codebase does not match the bound codebase"
    );
    let source = crate::codebase::checkout_source_id(root)?;
    ensure!(
        source == plan.source_identity,
        "plan belongs to a different local checkout source"
    );
    let summary: api::CodebaseSummary = client
        .get(&format!("/v1/codebases/{}", plan.codebase_id))
        .await
        .context("refresh codebase state before apply")?;
    ensure!(
        u64::try_from(summary.graph_generation).ok() == Some(plan.graph_generation),
        "the server graph advanced after this plan was created"
    );
    ensure!(summary.graph_fresh, "the server graph is no longer fresh");
    Ok(())
}

fn validate_history_context(client: &Client, root: &Path, history: &EditHistory) -> Result<()> {
    ensure!(
        client.codebase()? == history.codebase_id,
        "undo history belongs to another codebase"
    );
    let source = crate::codebase::checkout_source_id(root)?;
    ensure!(
        source == history.source_identity,
        "undo history belongs to another checkout source"
    );
    // The checkout identity above is the whole check: undo rewrites files on
    // THIS disk, and the server holds no claim over them to re-read.
    Ok(())
}

fn checkout_root(client: &Client) -> Result<PathBuf> {
    let raw = client
        .local_root()
        .ok_or_else(|| anyhow!("the selected codebase has no bound local checkout root"))?;
    fs::canonicalize(raw).with_context(|| format!("canonicalize checkout {}", raw.display()))
}

fn prepare_plan(
    checkout: &Arc<paths::Checkout>,
    plan: &api::WorkspaceEditPlan,
) -> Result<Vec<PreparedFile>> {
    let mut seen = HashSet::new();
    plan.files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let (path, target) = resolve_target(&checkout.path, &file.path)?;
            ensure!(seen.insert(target.clone()), "duplicate edit file {path}");
            let location = paths::Target::bind(checkout, &target)?;
            let preimage = location
                .read()
                .with_context(|| format!("read preimage {path}"))?;
            ensure!(
                hash(&preimage).eq_ignore_ascii_case(&file.preimage_hash),
                "stale preimage for {path}"
            );
            let postimage = apply_byte_edits(&preimage, &file.edits, &path)?;
            let postimage_hash = hash(&postimage);
            ensure!(
                postimage_hash.eq_ignore_ascii_case(&file.expected_postimage_hash),
                "computed postimage hash for {path} does not match the plan"
            );
            let (temporary, backup) = sidecars(&target, &plan.plan_id, index);
            ensure!(
                !temporary.exists() && !backup.exists(),
                "edit sidecar already exists for {path}"
            );
            Ok(PreparedFile {
                path,
                target,
                location,
                preimage,
                postimage,
                postimage_hash,
                temporary,
                backup,
            })
        })
        .collect()
}

fn prepare_undo(
    checkout: &Arc<paths::Checkout>,
    history: &EditHistory,
) -> Result<Vec<PreparedFile>> {
    history
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let (path, target) = resolve_target(&checkout.path, &file.path)?;
            let location = paths::Target::bind(checkout, &target)?;
            let current = location
                .read()
                .with_context(|| format!("read current postimage {path}"))?;
            ensure!(
                hash(&current).eq_ignore_ascii_case(&file.postimage_hash),
                "cannot undo {path}: current file does not match the recorded postimage"
            );
            let preimage = BASE64
                .decode(&file.preimage_base64)
                .with_context(|| format!("decode retained preimage for {path}"))?;
            ensure!(
                hash(&preimage).eq_ignore_ascii_case(&file.preimage_hash),
                "retained preimage hash is corrupt for {path}"
            );
            let (temporary, backup) = sidecars(&target, &history.plan_id, index);
            ensure!(
                !temporary.exists() && !backup.exists(),
                "edit sidecar already exists for {path}"
            );
            Ok(PreparedFile {
                path,
                target,
                location,
                preimage: current,
                postimage: preimage,
                postimage_hash: file.preimage_hash.clone(),
                temporary,
                backup,
            })
        })
        .collect()
}

fn apply_byte_edits(preimage: &[u8], edits: &[api::ByteEdit], path: &str) -> Result<Vec<u8>> {
    let mut previous_end = 0_u64;
    for edit in edits {
        ensure!(
            edit.start >= previous_end,
            "overlapping or unordered edits in {path}"
        );
        ensure!(edit.end >= edit.start, "reversed edit range in {path}");
        ensure!(
            edit.end <= u64::try_from(preimage.len()).unwrap_or(u64::MAX),
            "edit range exceeds {path}"
        );
        ensure!(
            edit.replacement.len() <= MAX_REPLACEMENT_BYTES,
            "replacement exceeds the per-edit size limit in {path}"
        );
        previous_end = edit.end;
    }

    let mut output = preimage.to_vec();
    for edit in edits.iter().rev() {
        let start = usize::try_from(edit.start).context("edit start exceeds platform size")?;
        let end = usize::try_from(edit.end).context("edit end exceeds platform size")?;
        output.splice(start..end, edit.replacement.as_bytes().iter().copied());
    }
    Ok(output)
}

fn resolve_target(root: &Path, relative: &str) -> Result<(String, PathBuf)> {
    ensure!(!relative.is_empty(), "edit path is empty");
    let path = Path::new(relative);
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            _ => bail!("edit path must be relative and may not traverse parents: {relative}"),
        }
    }
    let target = fs::canonicalize(root.join(&normalized))
        .with_context(|| format!("resolve edit target {relative}"))?;
    ensure!(
        target.starts_with(root),
        "edit target escapes the checkout: {relative}"
    );
    ensure!(
        target.is_file(),
        "edit target is not a regular file: {relative}"
    );
    let normalized = normalized.to_str().context("edit path is not UTF-8")?;
    #[cfg(windows)]
    let normalized = normalized.replace('\\', "/");
    #[cfg(not(windows))]
    let normalized = normalized.to_owned();
    Ok((normalized, target))
}

fn history_from(plan: &api::WorkspaceEditPlan, prepared: &[PreparedFile]) -> EditHistory {
    EditHistory {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_id: plan.plan_id.clone(),
        operation: plan.operation.clone(),
        codebase_id: plan.codebase_id.clone(),
        source_identity: plan.source_identity.clone(),
        files: prepared
            .iter()
            .map(|file| HistoryFile {
                path: file.path.clone(),
                preimage_hash: hash(&file.preimage),
                preimage_base64: BASE64.encode(&file.preimage),
                postimage_hash: file.postimage_hash.clone(),
            })
            .collect(),
        undone: false,
    }
}

fn history_matches(root: &Path, history: &EditHistory, preimages: bool) -> Result<bool> {
    for file in &history.files {
        let (_, target) = resolve_target(root, &file.path)?;
        let bytes = fs::read(target)?;
        let expected = if preimages {
            &file.preimage_hash
        } else {
            &file.postimage_hash
        };
        if !hash(&bytes).eq_ignore_ascii_case(expected) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn retained_recovery_paths(root: &Path, history: &EditHistory) -> Result<Vec<PathBuf>> {
    let mut retained = Vec::new();
    for (index, file) in history.files.iter().enumerate() {
        let (_, target) = resolve_target(root, &file.path)?;
        let (temporary, backup) = sidecars(&target, &history.plan_id, index);
        for path in [backup.with_extension("edit"), temporary, backup] {
            match fs::symlink_metadata(&path) {
                Ok(_) => retained.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("check retained edit recovery files"),
            }
        }
    }
    Ok(retained)
}

fn ensure_no_retained_recovery(root: &Path, history: &EditHistory) -> Result<()> {
    let paths = retained_recovery_paths(root, history)?;
    ensure!(
        paths.is_empty(),
        "plan {} has retained edit recovery files; inspect them before retrying: {}",
        history.plan_id,
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

fn outcome_from_history(
    history: &EditHistory,
    already_applied: bool,
    already_undone: bool,
    watcher_active: bool,
) -> ApplyOutcome {
    let use_preimages = history.undone || already_undone;
    ApplyOutcome {
        plan_id: history.plan_id.clone(),
        operation: if use_preimages {
            format!("undo:{}", history.operation)
        } else {
            history.operation.clone()
        },
        changed_files: history
            .files
            .iter()
            .map(|file| AppliedFile {
                path: file.path.clone(),
                content_hash: if use_preimages {
                    file.preimage_hash.clone()
                } else {
                    file.postimage_hash.clone()
                },
            })
            .collect(),
        already_applied,
        already_undone,
        watcher_active,
        sync_state: if watcher_active {
            "the active checkout watcher will enqueue an incremental sync".into()
        } else {
            "no active watcher was detected; run `semctl index` to sync the edits".into()
        },
    }
}

fn validate_plan_id(plan_id: &str) -> Result<()> {
    ensure!(
        plan_id.len() == 64
            && plan_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "plan id must be 64 lowercase hexadecimal characters"
    );
    Ok(())
}

fn history_path(plan_id: &str) -> Result<PathBuf> {
    validate_plan_id(plan_id)?;
    Ok(crate::config::edit_history_dir()?.join(format!("{plan_id}.json")))
}

fn create_history(path: &Path, history: &EditHistory) -> Result<()> {
    let parent = path.parent().context("edit history path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let bytes = serde_json::to_vec(history).context("serialize edit history")?;
    let mut file = crate::config::create_private_new(path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error).with_context(|| format!("write {}", path.display()));
    }
    Ok(())
}

fn write_history(path: &Path, history: &EditHistory) -> Result<()> {
    let bytes = serde_json::to_vec(history).context("serialize edit history")?;
    crate::config::atomic_write_private(path, &bytes)
}

fn read_history(path: &Path) -> Result<EditHistory> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let history: EditHistory = serde_json::from_slice(&bytes).context("parse edit history")?;
    ensure!(
        history.schema_version == PLAN_SCHEMA_VERSION,
        "unsupported edit history schema"
    );
    validate_plan_id(&history.plan_id)?;
    Ok(history)
}

/// Apply and undo share this lock across processes. Hash the opaque source
/// identity so no plan field can introduce a path component into the lock name.
fn lock_checkout(directory: &Path, source_identity: &str) -> Result<File> {
    let path = directory.join(format!(
        "checkout-{}.lock",
        hash(source_identity.as_bytes())
    ));
    crate::config::lock_file(&path)
}

fn sidecars(target: &Path, plan_id: &str, index: usize) -> (PathBuf, PathBuf) {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let stem = &plan_id[..12];
    (
        parent.join(format!(".semctl-{stem}-{index}.tmp")),
        parent.join(format!(".semctl-{stem}-{index}.bak")),
    )
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests;

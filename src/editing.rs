//! Verified local application and undo for server-produced workspace edit plans.
//!
//! The server is plan-only. This module is the single filesystem mutation
//! boundary: it re-authorizes the plan against the current codebase metadata,
//! verifies the opaque checkout identity and every preimage hash, stages every
//! postimage, then swaps the files with rollback backups. Preimages are retained
//! in semctl's private config directory for hash-guarded undo.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};

use crate::client::{Client, api};

const PLAN_SCHEMA_VERSION: u32 = 1;
const MAX_FILES: usize = 256;
const MAX_EDITS: usize = 4096;
const MAX_REPLACEMENT_BYTES: usize = 4 * 1024 * 1024;
const FORMATTER_TIMEOUT: Duration = Duration::from_secs(30);

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
    target: PathBuf,
    preimage: Vec<u8>,
    postimage: Vec<u8>,
    postimage_hash: String,
    temporary: PathBuf,
    backup: PathBuf,
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
    validate_server_context(client, &root, plan).await?;

    let history_path = history_path(&plan.plan_id)?;
    if history_path.exists() {
        let history = read_history(&history_path)?;
        if history.undone {
            bail!(
                "plan {} was already undone; request a fresh plan",
                plan.plan_id
            );
        }
        if history_matches(&root, &history, false)? {
            return Ok(outcome_from_history(&history, true, false, watcher_active));
        }
        bail!(
            "edit history for plan {} exists but the checkout no longer matches its postimages",
            plan.plan_id
        );
    }

    let mut prepared = prepare_plan(&root, plan)?;
    let mut history = history_from(plan, &prepared);
    create_history(&history_path, &history)?;

    if let Err(error) = stage_and_commit(&mut prepared) {
        let _ = fs::remove_file(&history_path);
        return Err(error);
    }

    if let Some(formatter) = &plan.formatter
        && let Err(error) = run_formatter_step(&root, formatter, &prepared).await
    {
        rollback(&prepared);
        let _ = fs::remove_file(&history_path);
        return Err(error);
    }

    // A formatter may intentionally change the planned postimages. Record the
    // exact bytes now on disk; undo verifies these hashes before restoring.
    for (state, record) in prepared.iter_mut().zip(&mut history.files) {
        let bytes = match fs::read(&state.target)
            .with_context(|| format!("read formatted postimage {}", state.target.display()))
        {
            Ok(bytes) => bytes,
            Err(error) => {
                rollback(&prepared);
                let _ = fs::remove_file(&history_path);
                return Err(error);
            }
        };
        state.postimage_hash = hash(&bytes);
        record.postimage_hash.clone_from(&state.postimage_hash);
    }
    if let Err(error) = write_history(&history_path, &history) {
        rollback(&prepared);
        let _ = fs::remove_file(&history_path);
        return Err(error);
    }

    cleanup_sidecars(&prepared);
    Ok(outcome_from_history(&history, false, false, watcher_active))
}

/// Restore retained preimages while every current file still matches the
/// postimage recorded by [`apply`].
// Undo restores backups on THIS disk. It no longer asks the server anything —
// the checkout owns its own files — but stays async: it is half of the
// apply/undo pair the command and MCP surfaces both await, and a signature that
// disagrees with its twin is a papercut for every caller.
#[allow(
    clippy::unused_async,
    reason = "pairs with apply on the command surface"
)]
pub async fn undo(client: &Client, plan_id: &str, watcher_active: bool) -> Result<ApplyOutcome> {
    validate_plan_id(plan_id)?;
    let root = checkout_root(client)?;
    let path = history_path(plan_id)?;
    let mut history = read_history(&path)
        .with_context(|| format!("no retained edit history for plan {plan_id}"))?;
    validate_history_context(client, &root, &history)?;

    if history.undone {
        ensure!(
            history_matches(&root, &history, true)?,
            "plan {plan_id} is marked undone but its files no longer match the retained preimages"
        );
        return Ok(outcome_from_history(&history, false, true, watcher_active));
    }

    let mut prepared = prepare_undo(&root, &history)?;
    stage_and_commit(&mut prepared)?;
    history.undone = true;
    if let Err(error) = write_history(&path, &history) {
        rollback(&prepared);
        history.undone = false;
        return Err(error);
    }
    cleanup_sidecars(&prepared);
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

fn prepare_plan(root: &Path, plan: &api::WorkspaceEditPlan) -> Result<Vec<PreparedFile>> {
    let mut seen = HashSet::new();
    plan.files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let (path, target) = resolve_target(root, &file.path)?;
            ensure!(seen.insert(target.clone()), "duplicate edit file {path}");
            let preimage =
                fs::read(&target).with_context(|| format!("read preimage {}", target.display()))?;
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
                preimage,
                postimage,
                postimage_hash,
                temporary,
                backup,
            })
        })
        .collect()
}

fn prepare_undo(root: &Path, history: &EditHistory) -> Result<Vec<PreparedFile>> {
    history
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let (path, target) = resolve_target(root, &file.path)?;
            let current = fs::read(&target)
                .with_context(|| format!("read current postimage {}", target.display()))?;
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
    Ok((normalized.to_string_lossy().replace('\\', "/"), target))
}

fn stage_and_commit(files: &mut [PreparedFile]) -> Result<()> {
    stage_postimages(files)?;
    install_staged(files, None)
}

fn stage_postimages(files: &[PreparedFile]) -> Result<()> {
    for file in files {
        let result = (|| -> Result<()> {
            let mut staged = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file.temporary)
                .with_context(|| format!("create staged postimage {}", file.temporary.display()))?;
            staged
                .write_all(&file.postimage)
                .and_then(|()| staged.sync_all())
                .with_context(|| format!("write staged postimage {}", file.temporary.display()))?;
            let permissions = fs::metadata(&file.target)?.permissions();
            fs::set_permissions(&file.temporary, permissions)?;
            Ok(())
        })();
        if let Err(error) = result {
            cleanup_sidecars(files);
            return Err(error);
        }
    }
    Ok(())
}

fn install_staged(files: &[PreparedFile], fail_before: Option<usize>) -> Result<()> {
    for (index, file) in files.iter().enumerate() {
        if fail_before == Some(index) {
            rollback(&files[..index]);
            cleanup_sidecars(files);
            bail!("injected atomic-install failure before {}", file.path);
        }
        if let Err(error) = fs::rename(&file.target, &file.backup)
            .and_then(|()| fs::rename(&file.temporary, &file.target))
        {
            rollback(&files[..index.saturating_add(1)]);
            cleanup_sidecars(files);
            return Err(error).with_context(|| format!("atomically replace {}", file.path));
        }
    }
    Ok(())
}

fn rollback(files: &[PreparedFile]) {
    for file in files.iter().rev() {
        if !file.backup.exists() {
            continue;
        }
        if file.target.exists() {
            let _ = fs::rename(&file.target, &file.temporary);
        }
        let _ = fs::rename(&file.backup, &file.target);
        let _ = fs::remove_file(&file.temporary);
    }
}

fn cleanup_sidecars(files: &[PreparedFile]) {
    for file in files {
        let _ = fs::remove_file(&file.temporary);
        let _ = fs::remove_file(&file.backup);
    }
}

async fn run_formatter_step(
    root: &Path,
    step: &api::FormatterStep,
    planned: &[PreparedFile],
) -> Result<()> {
    let program = Path::new(&step.program);
    ensure!(
        program.components().count() == 1,
        "formatter program must be a bare executable name"
    );
    let program_name = program
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        matches!(program_name.as_str(), "rustfmt" | "gofmt" | "prettier"),
        "formatter program '{}' is not in semctl's bounded allowlist",
        step.program
    );
    validate_formatter_arguments(&program_name, &step.arguments)?;
    ensure!(
        !step.paths.is_empty() && step.paths.len() <= MAX_FILES,
        "formatter must name between 1 and {MAX_FILES} planned paths"
    );
    let planned_targets: HashSet<&Path> =
        planned.iter().map(|file| file.target.as_path()).collect();
    let mut formatter_targets = HashSet::new();
    let mut normalized_paths = Vec::with_capacity(step.paths.len());
    for path in &step.paths {
        let (normalized, target) = resolve_target(root, path)?;
        ensure!(
            planned_targets.contains(target.as_path()),
            "formatter path {normalized} is not part of the edit plan"
        );
        ensure!(
            formatter_targets.insert(target),
            "duplicate formatter path {normalized}"
        );
        normalized_paths.push(normalized);
    }
    let mut command = tokio::process::Command::new(&step.program);
    command
        .current_dir(root)
        .kill_on_drop(true)
        .args(&step.arguments)
        .args(normalized_paths);
    let status = tokio::time::timeout(FORMATTER_TIMEOUT, command.status())
        .await
        .context("formatter timed out after 30 seconds")??;
    ensure!(status.success(), "formatter exited with {status}");
    Ok(())
}

fn validate_formatter_arguments(program: &str, arguments: &[String]) -> Result<()> {
    let valid = match program {
        "rustfmt" => match arguments {
            [] => true,
            [flag, edition]
                if flag == "--edition"
                    && matches!(edition.as_str(), "2015" | "2018" | "2021" | "2024") =>
            {
                true
            }
            _ => false,
        },
        "gofmt" => matches!(arguments, [write] if write == "-w"),
        "prettier" => {
            matches!(arguments, [write] if write == "--write")
                || matches!(arguments, [write, unknown] if write == "--write" && unknown == "--ignore-unknown")
        }
        _ => false,
    };
    ensure!(
        valid,
        "formatter arguments are outside semctl's bounded allowlist"
    );
    Ok(())
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
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error).with_context(|| format!("write {}", path.display()));
    }
    restrict_history_permissions(path);
    Ok(())
}

fn write_history(path: &Path, history: &EditHistory) -> Result<()> {
    let bytes = serde_json::to_vec(history).context("serialize edit history")?;
    let temporary = path.with_extension("json.new");
    let backup = path.with_extension("json.old");
    ensure!(
        !temporary.exists() && !backup.exists(),
        "edit-history update sidecar already exists"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("create {}", temporary.display()))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("write {}", temporary.display()));
    }
    drop(file);
    if let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("backup {}", path.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }
    let _ = fs::remove_file(&backup);
    restrict_history_permissions(path);
    Ok(())
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

#[cfg(unix)]
fn restrict_history_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_history_permissions(_path: &Path) {}

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
mod tests {
    use std::fs;

    use super::{
        EditHistory, HistoryFile, PLAN_SCHEMA_VERSION, PreparedFile, apply_byte_edits,
        cleanup_sidecars, hash, history_matches, install_staged, outcome_from_history,
        resolve_target, rollback, sidecars, stage_postimages, validate_formatter_arguments,
    };
    use crate::client::api::ByteEdit;

    #[test]
    fn byte_edits_apply_in_reverse_without_offset_drift() {
        let edits = vec![
            ByteEdit {
                start: 0,
                end: 1,
                replacement: "AA".into(),
            },
            ByteEdit {
                start: 4,
                end: 6,
                replacement: "Z".into(),
            },
        ];
        assert_eq!(apply_byte_edits(b"abcdef", &edits, "x").unwrap(), b"AAbcdZ");
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let edits = vec![
            ByteEdit {
                start: 1,
                end: 4,
                replacement: String::new(),
            },
            ByteEdit {
                start: 3,
                end: 5,
                replacement: String::new(),
            },
        ];
        assert!(apply_byte_edits(b"abcdef", &edits, "x").is_err());
    }

    #[test]
    fn out_of_checkout_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_target(temp.path(), "../escape.rs").is_err());
        assert!(resolve_target(temp.path(), "/escape.rs").is_err());
    }

    #[test]
    fn formatter_arguments_cannot_select_an_arbitrary_subcommand_or_plugin() {
        assert!(validate_formatter_arguments("rustfmt", &[]).is_ok());
        assert!(validate_formatter_arguments("gofmt", &["-w".to_string()]).is_ok());
        assert!(validate_formatter_arguments("prettier", &["--write".to_string()]).is_ok());
        assert!(validate_formatter_arguments("cargo", &["run".to_string()]).is_err());
        assert!(
            validate_formatter_arguments(
                "prettier",
                &["--plugin".to_string(), "untrusted.js".to_string()]
            )
            .is_err()
        );
    }

    #[test]
    fn staged_multi_file_failure_rolls_back_every_installed_file() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.rs");
        let second = temp.path().join("second.rs");
        fs::write(&first, b"first-before").unwrap();
        fs::write(&second, b"second-before").unwrap();
        let files = vec![
            prepared(&first, "first-after", 0),
            prepared(&second, "second-after", 1),
        ];

        stage_postimages(&files).unwrap();
        assert!(install_staged(&files, Some(1)).is_err());

        assert_eq!(fs::read(&first).unwrap(), b"first-before");
        assert_eq!(fs::read(&second).unwrap(), b"second-before");
        assert!(
            files
                .iter()
                .all(|file| !file.temporary.exists() && !file.backup.exists())
        );
    }

    #[test]
    fn retained_backups_restore_verified_preimages() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let target = root.join("source.rs");
        fs::write(&target, b"before").unwrap();
        let files = vec![prepared(&target, "after", 0)];

        stage_postimages(&files).unwrap();
        install_staged(&files, None).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"after");

        rollback(&files);
        cleanup_sidecars(&files);
        assert_eq!(fs::read(&target).unwrap(), b"before");
    }

    #[test]
    fn retained_hashes_recognize_duplicate_apply_and_undo_delivery() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let target = root.join("source.rs");
        fs::write(&target, b"after").unwrap();
        let mut history = EditHistory {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: "a".repeat(64),
            operation: "rename_symbol".into(),
            codebase_id: "cb".into(),
            source_identity: "checkout".into(),
            files: vec![HistoryFile {
                path: "source.rs".into(),
                preimage_hash: hash(b"before"),
                preimage_base64: String::new(),
                postimage_hash: hash(b"after"),
            }],
            undone: false,
        };

        assert!(history_matches(&root, &history, false).unwrap());
        let applied = outcome_from_history(&history, true, false, true);
        assert!(applied.already_applied);

        fs::write(&target, b"before").unwrap();
        history.undone = true;
        assert!(history_matches(&root, &history, true).unwrap());
        let undone = outcome_from_history(&history, false, true, true);
        assert!(undone.already_undone);
    }

    fn prepared(target: &std::path::Path, postimage: &str, index: usize) -> PreparedFile {
        let preimage = fs::read(target).unwrap();
        let (temporary, backup) = sidecars(target, &"a".repeat(64), index);
        PreparedFile {
            path: target.file_name().unwrap().to_string_lossy().into_owned(),
            target: target.to_path_buf(),
            preimage,
            postimage: postimage.as_bytes().to_vec(),
            postimage_hash: hash(postimage.as_bytes()),
            temporary,
            backup,
        }
    }
}

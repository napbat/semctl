//! File-sync engine shared by the `semctl index` command and the `semctl mcp`
//! auto-index (startup / watch / periodic).
//!
//! Walks a directory with [`walker`] (gitignore-aware, exclude-glob backstop,
//! `.git` skipped), sends a manifest the server diffs against what it holds, then
//! uploads only the changed files. Every candidate is read and hashed on each
//! scan. A [`SyncCache`] reuses content-filter decisions only after an exact
//! content hash match. Restored timestamps cannot hide changed bytes.
//!
//! [`background`] drives the mcp auto-index lifecycle (startup index, realtime
//! [`watcher`], periodic re-sync) on top of this engine; [`spawn_indexing`] is
//! its entry point.

mod background;
mod blocking;
mod cache;
mod policy;
mod scan;
mod source;
mod upload;
mod walker;
mod watcher;

pub use background::{spawn_indexing, spawn_indexing_tracked};
pub use cache::SyncCache;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::debug;

use crate::client::{Client, api};
use scan::ScanResult;

/// What a [`sync`] queued, for the caller to report on.
pub struct SyncOutcome {
    pub codebase_id: String,
    pub job_id: String,
    pub uploaded: usize,
    pub to_delete: usize,
}

/// A user-visible milestone from a one-shot sync. Background indexing uses the
/// quiet [`sync`] wrapper; interactive callers can use [`sync_with_progress`]
/// to show what is happening before the server-side embed job is queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncProgress {
    Preparing,
    Scanning {
        root: PathBuf,
    },
    Planning {
        files: usize,
        cached_files: usize,
    },
    Uploading {
        uploaded_files: usize,
        total_files: usize,
    },
    Finalizing,
}

/// Identifies the index job `sync_status` reports on.
#[derive(Clone)]
pub struct LastJob {
    pub(crate) job_id: String,
}

/// Per-codebase latest jobs queued by this MCP session. Multiple local checkouts
/// can be active at once, so one global "last job" slot would make status for one
/// codebase accidentally report another codebase's sync.
pub type JobRegistry = Mutex<HashMap<String, LastJob>>;

/// Record the job a background sync just queued, so `sync_status` can poll it.
pub(crate) async fn record_job(registry: &JobRegistry, o: &SyncOutcome) {
    registry.lock().await.insert(
        o.codebase_id.clone(),
        LastJob {
            job_id: o.job_id.clone(),
        },
    );
}

/// Register (if new) the Local codebase for `dir`, walk it, diff against the
/// server, and upload the changed files. Returns once the embed job is queued —
/// the caller decides whether to poll it. Shared by the `semctl index` command
/// and the `semctl mcp` startup / watch / periodic auto-index.
///
/// Holds the `cache` lock for the whole call: that gives exclusive cache access
/// *and* serializes overlapping syncs to one codebase into one job at a time.
/// Step-by-step progress is logged at `debug`; callers emit the `info`-level
/// summary so a no-op periodic tick stays quiet.
pub async fn sync(client: &Client, dir: &Path, cache: &Mutex<SyncCache>) -> Result<SyncOutcome> {
    sync_with_progress(client, dir, cache, |_| {}).await
}

/// The interactive form of [`sync`], reporting scan, plan, and completed
/// upload-batch progress through `on_progress`.
pub async fn sync_with_progress<F>(
    client: &Client,
    dir: &Path,
    cache: &Mutex<SyncCache>,
    on_progress: F,
) -> Result<SyncOutcome>
where
    F: Fn(&SyncProgress),
{
    on_progress(&SyncProgress::Preparing);
    // A manifest is complete desired state. Always lift a path inside a Git
    // checkout to its worktree root so launching from `src/` cannot delete
    // everything outside `src/` from the server index.
    let dir = crate::codebase::working_copy_root(dir).await;
    // An explicit --codebase / SEMCTX_CODEBASE indexes into that codebase;
    // otherwise resolve the folder's Local codebase, creating it if new.
    let codebase_id = match client.codebase_raw() {
        Some(id) => id.to_string(),
        None => crate::codebase::ensure(client, &dir)
            .await
            .context("register codebase")?,
    };
    let source_id = crate::codebase::checkout_source_id(&dir).context("identify local checkout")?;
    let cache = cache.lock().await;
    debug!(%codebase_id, dir = %dir.display(), "indexing codebase");
    on_progress(&SyncProgress::Scanning { root: dir.clone() });

    let ScanResult {
        manifest,
        changed,
        cached_files,
        policy,
    } = scan::run(dir.clone(), source_id.clone(), &cache).await?;
    on_progress(&SyncProgress::Planning {
        files: manifest.len(),
        cached_files,
    });

    let request = api::SyncManifestRequest {
        files: manifest,
        source_id: source_id.clone(),
        vcs: crate::codebase::checkout_state(&dir).await,
    };
    let policy = blocking::run(move |cancellation| {
        policy.verify(&cancellation)?;
        Ok(policy)
    })
    .await
    .context("validate source policy before sync")?;
    let plan: api::SyncPlan = client
        .post(&format!("/v1/codebases/{codebase_id}/sync"), &request)
        .await
        .context("sync plan")?;

    // A checkout whose remote was re-pointed belongs to a different project
    // than the one it was filed under, and the server moves it rather than
    // letting it go on writing into a codebase it has left. Everything after
    // this — the uploads, the cache, what gets reported — is about where the
    // checkout actually is now.
    let codebase_id = match plan.codebase_id.as_deref() {
        Some(moved) if moved != codebase_id => {
            let _ = crate::config::cache_codebase(&dir, moved).await;
            moved.to_string()
        }
        _ => codebase_id,
    };

    let files = upload::Files::new(dir, &plan.need_content, request.files, changed, policy)?;
    let uploaded = upload::run(
        client,
        &codebase_id,
        &plan.job_id,
        files,
        &source_id,
        &on_progress,
    )
    .await?;
    Ok(SyncOutcome {
        codebase_id,
        job_id: plan.job_id,
        uploaded,
        to_delete: plan.to_delete.len(),
    })
}

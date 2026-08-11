//! File-sync engine shared by the `semctl index` command and the `semctl mcp`
//! auto-index (startup / watch / periodic).
//!
//! Walks a directory with [`walker`] (gitignore-aware, exclude-glob backstop,
//! `.git` skipped), sends a manifest the server diffs against what it holds, then
//! uploads only the changed files. A [`SyncCache`] of file stamps lets repeat
//! syncs (the mcp periodic / watch passes) skip re-reading and re-hashing files
//! whose mtime+size are unchanged, so an idle re-sync is a cheap stat-walk rather
//! than a full re-read of the tree.
//!
//! [`background`] drives the mcp auto-index lifecycle (startup index, realtime
//! [`watcher`], periodic re-sync) on top of this engine; [`spawn_indexing`] is
//! its entry point.

mod background;
mod walker;
mod watcher;

pub use background::{spawn_indexing, spawn_indexing_tracked};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::client::{Client, api};

/// Cap each upload request's combined content so a big first sync streams in
/// bounded batches instead of one giant body.
const UPLOAD_BATCH_BYTES: usize = 4 * 1024 * 1024;

/// How many upload batches to have in flight at once — an HTTP politeness cap;
/// the embedder queues server-side regardless of request concurrency.
const UPLOAD_PARALLEL_REQUESTS: usize = 4;

/// What a [`sync`] queued, for the caller to report on.
pub struct SyncOutcome {
    pub codebase_id: String,
    pub job_id: String,
    pub uploaded: usize,
    pub to_delete: usize,
}

#[derive(Clone)]
struct FileStamp {
    mtime_ns: u128,
    size: u64,
    hash: String,
}

/// Per-process cache of file stamps. Lets repeat syncs skip re-reading and
/// re-hashing unchanged files. The caller wraps it in a [`Mutex`], which doubles
/// as the lock that serializes concurrent syncs (startup vs watch vs periodic)
/// to a single codebase.
#[derive(Default)]
pub struct SyncCache {
    files: HashMap<String, FileStamp>,
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
    let source_id = sync_source_id(&dir).context("identify local checkout")?;
    let mut cache = cache.lock().await;
    debug!(%codebase_id, dir = %dir.display(), "indexing codebase");

    // Stat-walk the tree, then read+hash only the files whose stamp changed.
    // Content for changed files is kept for the upload phase; unchanged files
    // contribute their cached hash without a read.
    let candidates = walker::walk(&dir, &walker::WalkOptions::default());
    let mut manifest = Vec::with_capacity(candidates.len());
    let mut changed: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(candidates.len());

    for c in candidates {
        seen.insert(c.rel.clone());
        let cached_hash = cache
            .files
            .get(&c.rel)
            .filter(|st| st.mtime_ns == c.mtime_ns && st.size == c.size)
            .map(|st| st.hash.clone());
        if let Some(hash) = cached_hash {
            manifest.push(api::ManifestEntry {
                path: c.rel,
                hash,
                size: i64::try_from(c.size).unwrap_or(i64::MAX),
            });
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&c.path) else {
            debug!(rel = %c.rel, "skip — unreadable / non-UTF-8");
            cache.files.remove(&c.rel);
            continue;
        };
        if !walker::is_indexable(&content) {
            debug!(rel = %c.rel, "skip — empty / generated / minified");
            cache.files.remove(&c.rel);
            continue;
        }
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        cache.files.insert(
            c.rel.clone(),
            FileStamp {
                mtime_ns: c.mtime_ns,
                size: c.size,
                hash: hash.clone(),
            },
        );
        manifest.push(api::ManifestEntry {
            path: c.rel.clone(),
            hash,
            size: i64::try_from(c.size).unwrap_or(i64::MAX),
        });
        changed.insert(c.rel, content);
    }
    // Drop stamps for files that vanished so the cache can't grow unbounded.
    cache.files.retain(|k, _| seen.contains(k));
    debug!(files = manifest.len(), changed = changed.len(), "scanned");

    let plan: api::SyncPlan = client
        .post(
            &format!("/v1/codebases/{codebase_id}/sync"),
            &api::SyncManifestRequest {
                files: manifest,
                source_id: source_id.clone(),
            },
        )
        .await
        .context("sync plan")?;

    let uploaded = upload_needed(client, &codebase_id, &plan, &dir, &changed, &source_id).await?;
    Ok(SyncOutcome {
        codebase_id,
        job_id: plan.job_id,
        uploaded,
        to_delete: plan.to_delete.len(),
    })
}

/// Upload the content the server's plan asked for, at bounded concurrency.
/// Files read during this pass come from `changed`; anything else the server
/// wants (e.g. it was wiped and re-requests unchanged files) is read from disk
/// on demand, so we never hold the whole tree in memory.
async fn upload_needed(
    client: &Client,
    codebase_id: &str,
    plan: &api::SyncPlan,
    dir: &Path,
    changed: &HashMap<String, String>,
    source_id: &str,
) -> Result<usize> {
    if plan.need_content.is_empty() {
        return Ok(0);
    }

    // Group the needed paths into byte-bounded batches, pulling content from the
    // in-memory set or reading it on demand.
    let mut batches: Vec<Vec<api::SyncFileContent>> = Vec::new();
    let mut current: Vec<api::SyncFileContent> = Vec::new();
    let mut bytes = 0usize;
    for path in &plan.need_content {
        let content = match changed.get(path) {
            Some(c) => c.clone(),
            None => match std::fs::read_to_string(dir.join(path)) {
                Ok(c) => c,
                Err(e) => {
                    warn!(%path, error = %e, "upload: skip unreadable file");
                    continue;
                }
            },
        };
        // Never send empty content — the server's Content field is [Required].
        if content.is_empty() {
            continue;
        }
        if bytes + content.len() > UPLOAD_BATCH_BYTES && !current.is_empty() {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += content.len();
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        current.push(api::SyncFileContent {
            path: path.clone(),
            content,
            hash: Some(hash),
        });
    }
    if !current.is_empty() {
        batches.push(current);
    }

    let total = plan.need_content.len();
    let url = format!("/v1/codebases/{codebase_id}/sync/{}", plan.job_id);

    // One batch → one final PUT. Several → upload every batch concurrently marked
    // non-final (the server stages them without queueing the job), then one empty
    // final PUT completes the sync. Without the marker the server queued the job
    // on whichever PUT landed first and 409'd the rest.
    if batches.len() == 1 {
        let batch = batches.pop().expect("one batch");
        let n = batch.len();
        client
            .put::<_, serde_json::Value>(
                &url,
                &api::SyncContentRequest {
                    files: batch,
                    source_id: source_id.to_string(),
                    r#final: None,
                },
            )
            .await
            .with_context(|| format!("upload {n} files"))?;
        return Ok(n);
    }

    let mut set: tokio::task::JoinSet<Result<usize>> = tokio::task::JoinSet::new();
    let mut uploaded = 0usize;
    for batch in batches {
        if set.len() >= UPLOAD_PARALLEL_REQUESTS {
            uploaded += join_one(&mut set).await?;
            debug!(uploaded, total, "uploaded batch");
        }
        let client = client.clone();
        let url = url.clone();
        let source_id = source_id.to_string();
        set.spawn(async move {
            let n = batch.len();
            client
                .put::<_, serde_json::Value>(
                    &url,
                    &api::SyncContentRequest {
                        files: batch,
                        source_id,
                        r#final: Some(false),
                    },
                )
                .await
                .with_context(|| format!("upload {n} files"))?;
            Ok(n)
        });
    }
    while !set.is_empty() {
        uploaded += join_one(&mut set).await?;
        debug!(uploaded, total, "uploaded batch");
    }
    client
        .put::<_, serde_json::Value>(
            &url,
            &api::SyncContentRequest {
                files: Vec::new(),
                source_id: source_id.to_string(),
                r#final: Some(true),
            },
        )
        .await
        .context("complete upload")?;
    Ok(uploaded)
}

/// Opaque stable identity for one local checkout. Different semctl processes
/// in the same checkout intentionally produce the same value; another clone
/// produces a different value even when it points at the same git remote.
fn sync_source_id(dir: &Path) -> Result<String> {
    let installation_id = crate::config::installation_id()?;
    Ok(sync_source_id_for(&installation_id, dir))
}

fn sync_source_id_for(installation_id: &str, dir: &Path) -> String {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut path = canonical.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        path.make_ascii_lowercase();
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"semctl-sync-source-v1\0");
    hasher.update(installation_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Await the next finished upload task, flattening the join error and the task's
/// own result into one `Result`.
async fn join_one(set: &mut tokio::task::JoinSet<Result<usize>>) -> Result<usize> {
    match set.join_next().await {
        Some(joined) => joined.map_err(|e| anyhow::anyhow!("upload task: {e}"))?,
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::sync_source_id_for;
    use std::path::Path;

    #[test]
    fn source_identity_is_stable_for_the_same_checkout() {
        let first = sync_source_id_for("install-a", Path::new("/work/repo"));
        let second = sync_source_id_for("install-a", Path::new("/work/repo"));
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn source_identity_separates_installations_and_checkouts() {
        let baseline = sync_source_id_for("install-a", Path::new("/work/repo"));
        assert_ne!(
            baseline,
            sync_source_id_for("install-a", Path::new("/work/other-clone"))
        );
        assert_ne!(
            baseline,
            sync_source_id_for("install-b", Path::new("/work/repo"))
        );
    }
}

//! File-sync engine shared by the `semctl index` command and the `semctl mcp`
//! auto-index (startup / watch / periodic).
//!
//! Walks a directory with [`walker`] (gitignore-aware, exclude-glob backstop,
//! `.git` skipped), sends a manifest the server diffs against what it holds, then
//! uploads only the changed files. A [`SyncCache`] of file stamps lets repeat
//! syncs (including separate CLI runs when persistence is enabled) skip
//! re-reading and re-hashing files whose mtime+size are unchanged, so an idle
//! re-sync is a cheap stat-walk rather than a full re-read of the tree.
//!
//! [`background`] drives the mcp auto-index lifecycle (startup index, realtime
//! [`watcher`], periodic re-sync) on top of this engine; [`spawn_indexing`] is
//! its entry point.

mod background;
mod walker;
mod watcher;

pub use background::{spawn_indexing, spawn_indexing_tracked};

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::client::{Client, api};

/// Cap each upload request's combined content so a big first sync streams in
/// bounded batches instead of one giant body.
const UPLOAD_BATCH_BYTES: usize = 4 * 1024 * 1024;

/// Tiny source files can number in the thousands without reaching the byte cap.
/// A count cap keeps progress/resume granularity useful and creates enough
/// independent requests to benefit from bounded upload concurrency.
const UPLOAD_BATCH_FILES: usize = 256;

/// How many upload batches to have in flight at once — an HTTP politeness cap;
/// the embedder queues server-side regardless of request concurrency.
const UPLOAD_PARALLEL_REQUESTS: usize = 4;

/// Bump when file hashing or cache semantics change. An unknown version is a
/// harmless cache miss, never an indexing failure.
const SYNC_CACHE_FORMAT_VERSION: u32 = 3;

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct FileStamp {
    mtime_ns: u128,
    size: u64,
    /// `None` means unchanged content was inspected and intentionally excluded
    /// (blank, generated, minified, or non-UTF-8).
    hash: Option<String>,
}

impl FileStamp {
    fn for_candidate(candidate: &walker::Candidate, hash: Option<String>) -> Self {
        Self {
            mtime_ns: candidate.mtime_ns,
            size: candidate.size,
            hash,
        }
    }

    fn matches(&self, candidate: &walker::Candidate) -> bool {
        self.mtime_ns == candidate.mtime_ns && self.size == candidate.size
    }
}

/// Content read during the manifest pass, paired with the hash of those exact
/// bytes so upload batching does not hash the file a second time.
struct PreparedFile {
    content: String,
    hash: String,
}

impl PreparedFile {
    fn new(content: String) -> Self {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Self { content, hash }
    }
}

struct ScanResult {
    manifest: Vec<api::ManifestEntry>,
    changed: HashMap<String, PreparedFile>,
    cached_files: usize,
}

#[derive(Deserialize, Serialize)]
struct StoredSyncCache {
    format_version: u32,
    files: HashMap<String, FileStamp>,
}

#[derive(Serialize)]
struct StoredSyncCacheView<'a> {
    format_version: u32,
    files: &'a HashMap<String, FileStamp>,
}

/// Cache of file stamps. It is in-memory by default for MCP background syncs;
/// [`SyncCache::persistent`] also loads/saves the same stamps across one-shot
/// CLI invocations. Source contents are never persisted.
///
/// The caller wraps it in a [`Mutex`], which doubles as the lock that serializes
/// concurrent syncs (startup vs watch vs periodic) to a single codebase.
#[derive(Default)]
pub struct SyncCache {
    files: HashMap<String, FileStamp>,
    persist: bool,
    persisted_source_id: Option<String>,
    persistence_path: Option<PathBuf>,
}

impl SyncCache {
    /// Create a cache that survives separate CLI processes. Cache failures only
    /// cost performance: indexing falls back to reading and hashing every file.
    #[must_use]
    pub fn persistent() -> Self {
        Self {
            persist: true,
            ..Self::default()
        }
    }

    fn load_persistent(&mut self, source_id: &str) {
        if !self.persist || self.persisted_source_id.as_deref() == Some(source_id) {
            return;
        }
        self.files.clear();
        self.persisted_source_id = Some(source_id.to_string());
        self.persistence_path = None;

        let path = match crate::config::sync_cache_dir() {
            Ok(dir) => dir.join(format!("{source_id}.json")),
            Err(error) => {
                warn!(%error, "sync cache unavailable; hashing all files");
                return;
            }
        };
        self.persistence_path = Some(path.clone());
        match read_persistent_cache(&path) {
            Ok(Some(files)) => {
                debug!(path = %path.display(), files = files.len(), "loaded sync cache");
                self.files = files;
            }
            Ok(None) => {}
            Err(error) => {
                warn!(path = %path.display(), %error, "ignoring unreadable sync cache");
            }
        }
    }

    fn save_persistent(&self) {
        let Some(path) = self.persistence_path.as_deref() else {
            return;
        };
        if let Err(error) = write_persistent_cache(path, &self.files) {
            warn!(path = %path.display(), %error, "couldn't save sync cache");
        }
    }
}

fn read_persistent_cache(path: &Path) -> Result<Option<HashMap<String, FileStamp>>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let stored: StoredSyncCache = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("parse {}", path.display()))?;
    if stored.format_version != SYNC_CACHE_FORMAT_VERSION {
        return Ok(None);
    }
    Ok(Some(stored.files))
}

fn write_persistent_cache(path: &Path, files: &HashMap<String, FileStamp>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = fs::File::create(path).with_context(|| format!("write {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(
        &mut writer,
        &StoredSyncCacheView {
            format_version: SYNC_CACHE_FORMAT_VERSION,
            files,
        },
    )
    .context("serialize sync cache")?;
    writer
        .flush()
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
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
    let mut cache = cache.lock().await;
    cache.load_persistent(&source_id);
    debug!(%codebase_id, dir = %dir.display(), "indexing codebase");
    on_progress(&SyncProgress::Scanning { root: dir.clone() });

    let ScanResult {
        manifest,
        mut changed,
        cached_files,
    } = scan_directory(&dir, &mut cache);
    on_progress(&SyncProgress::Planning {
        files: manifest.len(),
        cached_files,
    });

    debug!("reading checkout metadata");
    let vcs = crate::codebase::checkout_state(&dir).await;
    debug!("requesting sync plan");
    let plan: api::SyncPlan = client
        .post(
            &format!("/v1/codebases/{codebase_id}/sync"),
            &api::SyncManifestRequest {
                files: manifest,
                source_id: source_id.clone(),
                vcs,
            },
        )
        .await
        .context("sync plan")?;
    debug!(
        need_content = plan.need_content.len(),
        to_delete = plan.to_delete.len(),
        job = %plan.job_id,
        "received sync plan"
    );

    // A checkout whose remote was re-pointed belongs to a different project
    // than the one it was filed under, and the server moves it rather than
    // letting it go on writing into a codebase it has left. Everything after
    // this — the uploads, the cache, what gets reported — is about where the
    // checkout actually is now.
    let codebase_id = match plan.codebase_id.as_deref() {
        Some(moved) if moved != codebase_id => {
            let _ = crate::config::cache_codebase(&dir, moved);
            moved.to_string()
        }
        _ => codebase_id,
    };

    let uploaded = upload_needed(
        client,
        &codebase_id,
        &plan,
        &dir,
        &mut changed,
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

/// Build the desired-state manifest and retain newly read content for upload.
/// Cache hits need only the metadata walk; deterministic content exclusions are
/// cached too so blank/generated/minified files are not reread on every sync.
fn scan_directory(dir: &Path, cache: &mut SyncCache) -> ScanResult {
    // Stat-walk the tree, then read+hash only the files whose stamp changed.
    // Content for changed files is kept for the upload phase; unchanged files
    // contribute their cached hash without a read.
    let candidates = walker::walk(dir, &walker::WalkOptions::default());
    let mut manifest = Vec::with_capacity(candidates.len());
    let mut changed: HashMap<String, PreparedFile> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(candidates.len());
    let mut cached_files = 0usize;
    let mut cache_dirty = false;

    for c in candidates {
        seen.insert(c.rel.clone());
        let cached = cache.files.get(&c.rel).filter(|stamp| stamp.matches(&c));
        if let Some(stamp) = cached {
            if let Some(hash) = stamp.hash.clone() {
                cached_files += 1;
                manifest.push(api::ManifestEntry {
                    path: c.rel,
                    hash,
                    size: i64::try_from(c.size).unwrap_or(i64::MAX),
                });
            }
            continue;
        }

        let content = match std::fs::read_to_string(&c.path) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::InvalidData => {
                debug!(rel = %c.rel, "skip — non-UTF-8");
                cache
                    .files
                    .insert(c.rel.clone(), FileStamp::for_candidate(&c, None));
                cache_dirty = true;
                continue;
            }
            Err(error) => {
                debug!(rel = %c.rel, %error, "skip — unreadable");
                cache_dirty |= cache.files.remove(&c.rel).is_some();
                continue;
            }
        };
        if !walker::is_indexable(&content) {
            debug!(rel = %c.rel, "skip — blank / generated / minified");
            cache
                .files
                .insert(c.rel.clone(), FileStamp::for_candidate(&c, None));
            cache_dirty = true;
            continue;
        }
        let prepared = PreparedFile::new(content);
        cache.files.insert(
            c.rel.clone(),
            FileStamp::for_candidate(&c, Some(prepared.hash.clone())),
        );
        cache_dirty = true;
        manifest.push(api::ManifestEntry {
            path: c.rel.clone(),
            hash: prepared.hash.clone(),
            size: i64::try_from(c.size).unwrap_or(i64::MAX),
        });
        changed.insert(c.rel, prepared);
    }
    // Drop stamps for files that vanished so the cache can't grow unbounded.
    persist_scan_cache(cache, &seen, cache_dirty);
    debug!(files = manifest.len(), changed = changed.len(), "scanned");
    ScanResult {
        manifest,
        changed,
        cached_files,
    }
}

fn persist_scan_cache(cache: &mut SyncCache, seen: &HashSet<String>, mut dirty: bool) {
    let cached_before_retain = cache.files.len();
    cache.files.retain(|path, _| seen.contains(path));
    dirty |= cache.files.len() != cached_before_retain;
    if dirty {
        cache.save_persistent();
    }
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
    changed: &mut HashMap<String, PreparedFile>,
    source_id: &str,
    on_progress: &impl Fn(&SyncProgress),
) -> Result<usize> {
    if plan.need_content.is_empty() {
        return Ok(0);
    }

    let mut batches = prepare_upload_batches(&plan.need_content, dir, changed)?;
    let total: usize = batches.iter().map(Vec::len).sum();
    if total > 0 {
        report_upload_progress(on_progress, 0, total);
    }
    let url = format!("/v1/codebases/{codebase_id}/sync/{}", plan.job_id);

    // One batch → one final PUT. Several → upload every batch concurrently marked
    // non-final (the server stages them without queueing the job), then one empty
    // final PUT completes the sync. Without the marker the server queued the job
    // on whichever PUT landed first and 409'd the rest.
    if batches.len() == 1 {
        let batch = batches.pop().expect("one batch");
        let n = upload_batch(client, &url, source_id, batch, None).await?;
        report_upload_progress(on_progress, n, total);
        return Ok(n);
    }

    let mut set: tokio::task::JoinSet<Result<usize>> = tokio::task::JoinSet::new();
    let mut uploaded = 0usize;
    for batch in batches {
        if set.len() >= UPLOAD_PARALLEL_REQUESTS {
            uploaded += join_one(&mut set).await?;
            debug!(uploaded, total, "uploaded batch");
            report_upload_progress(on_progress, uploaded, total);
        }
        let client = client.clone();
        let url = url.clone();
        let source_id = source_id.to_string();
        set.spawn(async move { upload_batch(&client, &url, &source_id, batch, Some(false)).await });
    }
    while !set.is_empty() {
        uploaded += join_one(&mut set).await?;
        debug!(uploaded, total, "uploaded batch");
        report_upload_progress(on_progress, uploaded, total);
    }
    on_progress(&SyncProgress::Finalizing);
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

/// Group requested content into byte-bounded requests. A file retained from
/// the manifest pass carries its already-computed hash; only cache-hit content
/// that the server re-requests must be read and hashed here.
fn prepare_upload_batches(
    needed_paths: &[String],
    dir: &Path,
    changed: &mut HashMap<String, PreparedFile>,
) -> Result<Vec<Vec<api::SyncFileContent>>> {
    let mut batches: Vec<Vec<api::SyncFileContent>> = Vec::new();
    let mut current: Vec<api::SyncFileContent> = Vec::new();
    let mut bytes = 0usize;
    for path in needed_paths {
        let prepared = if let Some(prepared) = changed.remove(path) {
            prepared
        } else {
            let content = std::fs::read_to_string(dir.join(path))
                .with_context(|| format!("read {path} for upload"))?;
            if !walker::has_uploadable_content(&content) {
                anyhow::bail!("{path} became blank after the manifest was created; rerun index");
            }
            PreparedFile::new(content)
        };
        // The manifest pass enforces this too. Keep the upload boundary
        // defensive so stale in-memory state can never trigger a server 400.
        if !walker::has_uploadable_content(&prepared.content) {
            anyhow::bail!("{path} became blank after the manifest was created; rerun index");
        }
        let batch_full = current.len() >= UPLOAD_BATCH_FILES
            || bytes + prepared.content.len() > UPLOAD_BATCH_BYTES;
        if batch_full && !current.is_empty() {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += prepared.content.len();
        current.push(api::SyncFileContent {
            path: path.clone(),
            content: prepared.content,
            hash: Some(prepared.hash),
        });
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

fn report_upload_progress(
    on_progress: &impl Fn(&SyncProgress),
    uploaded_files: usize,
    total_files: usize,
) {
    on_progress(&SyncProgress::Uploading {
        uploaded_files,
        total_files,
    });
}

async fn upload_batch(
    client: &Client,
    url: &str,
    source_id: &str,
    files: Vec<api::SyncFileContent>,
    final_batch: Option<bool>,
) -> Result<usize> {
    let n = files.len();
    client
        .put::<_, serde_json::Value>(
            url,
            &api::SyncContentRequest {
                files,
                source_id: source_id.to_string(),
                r#final: final_batch,
            },
        )
        .await
        .with_context(|| format!("upload {n} files"))?;
    Ok(n)
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
    use super::*;

    fn stamp() -> FileStamp {
        FileStamp {
            mtime_ns: 1_234_567_890,
            size: 42,
            hash: Some("abc123".to_string()),
        }
    }

    #[test]
    fn persistent_cache_round_trips_file_stamps() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sync-cache").join("cache.json");
        let files = HashMap::from([
            ("src/main.rs".to_string(), stamp()),
            (
                "generated.rs".to_string(),
                FileStamp {
                    mtime_ns: 987_654_321,
                    size: 128,
                    hash: None,
                },
            ),
        ]);

        write_persistent_cache(&path, &files).unwrap();

        assert_eq!(read_persistent_cache(&path).unwrap(), Some(files));
    }

    #[test]
    fn unknown_cache_format_is_a_clean_miss() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.json");
        let stale = StoredSyncCache {
            format_version: SYNC_CACHE_FORMAT_VERSION + 1,
            files: HashMap::from([("src/main.rs".to_string(), stamp())]),
        };
        fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();

        assert_eq!(read_persistent_cache(&path).unwrap(), None);
    }

    #[test]
    fn upload_batch_reuses_hash_from_manifest_pass() {
        let temp = tempfile::tempdir().unwrap();
        let path = "src/main.rs".to_string();
        let mut changed = HashMap::from([(
            path.clone(),
            PreparedFile {
                content: "fn main() {}\n".to_string(),
                hash: "already-computed".to_string(),
            },
        )]);

        let batches =
            prepare_upload_batches(std::slice::from_ref(&path), temp.path(), &mut changed).unwrap();

        assert!(changed.is_empty());
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0][0].hash.as_deref(), Some("already-computed"));
    }

    #[test]
    fn upload_batches_reject_blank_content_before_the_request() {
        let temp = tempfile::tempdir().unwrap();
        let path = "blank.txt".to_string();
        let mut changed = HashMap::from([(
            path.clone(),
            PreparedFile {
                content: " \n\t".to_string(),
                hash: "irrelevant".to_string(),
            },
        )]);

        let error = prepare_upload_batches(std::slice::from_ref(&path), temp.path(), &mut changed)
            .unwrap_err();

        assert!(error.to_string().contains("became blank"));
    }

    #[test]
    fn tiny_files_are_split_by_count_for_progress_and_resume() {
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<String> = (0..=UPLOAD_BATCH_FILES)
            .map(|index| format!("src/{index}.rs"))
            .collect();
        let mut changed: HashMap<String, PreparedFile> = paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    PreparedFile::new("fn tiny() {}\n".to_string()),
                )
            })
            .collect();

        let batches = prepare_upload_batches(&paths, temp.path(), &mut changed).unwrap();

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), UPLOAD_BATCH_FILES);
        assert_eq!(batches[1].len(), 1);
    }

    #[test]
    fn repeat_scan_reuses_indexed_and_excluded_file_stamps() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join("blank.txt"), " \n\t").unwrap();
        fs::write(
            temp.path().join("generated.rs"),
            "// @generated by a test\nstruct Generated;\n",
        )
        .unwrap();
        let mut cache = SyncCache::default();

        let first = scan_directory(temp.path(), &mut cache);
        assert_eq!(first.manifest.len(), 1);
        assert_eq!(first.changed.len(), 1);
        assert_eq!(first.cached_files, 0);
        assert!(
            cache
                .files
                .get("blank.txt")
                .is_some_and(|stamp| stamp.hash.is_none())
        );
        assert!(
            cache
                .files
                .get("generated.rs")
                .is_some_and(|stamp| stamp.hash.is_none())
        );

        let second = scan_directory(temp.path(), &mut cache);
        assert_eq!(second.manifest.len(), 1);
        assert!(second.changed.is_empty());
        assert_eq!(second.cached_files, 1);
    }
}

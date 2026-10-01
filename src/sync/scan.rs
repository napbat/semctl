//! Complete filesystem scans and manifest preparation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::debug;

use super::blocking::Cancellation;
use super::cache::{CacheState, CachedContent, SyncCache};
use super::{blocking, source, walker};
use crate::client::api;

/// Content read during the manifest pass, paired with the hash of those exact
/// bytes so upload batching does not hash the file a second time.
pub(super) struct PreparedFile {
    pub(super) content: String,
    pub(super) hash: String,
}

impl PreparedFile {
    pub(super) fn new(content: String) -> Self {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Self { content, hash }
    }
}

pub(super) struct ScanResult {
    pub(super) manifest: Vec<api::ManifestEntry>,
    pub(super) changed: HashMap<String, PreparedFile>,
    pub(super) cached_files: usize,
    pub(super) policy: super::policy::SourcePolicy,
}

// Retain a bounded amount of content to avoid a second read for small syncs.
// Larger syncs reread requested files and verify their manifest hashes.
const RETAINED_CONTENT_BYTES: usize = 4 * 1024 * 1024;

pub(super) async fn run(dir: PathBuf, source_id: String, cache: &SyncCache) -> Result<ScanResult> {
    let mut state = cache.state.clone().lock_owned().await;
    blocking::run(move |cancellation| {
        state.load_persistent(&source_id);
        scan_directory(&dir, &mut state, &cancellation)
    })
    .await
    .context("scan checkout")
}

/// Build the desired-state manifest and retain newly read content for upload.
/// Cached decisions require an exact hash of the current bytes. This includes
/// exclusions, so preserved timestamps cannot hide a newly indexable file.
fn scan_directory(
    dir: &Path,
    cache: &mut CacheState,
    cancellation: &Cancellation,
) -> Result<ScanResult> {
    // Read and hash every candidate. Keep a bounded amount of changed content
    // for upload, and reuse only decisions made on byte-identical content.
    let root = std::fs::canonicalize(dir).context("resolve scan root")?;
    let walk = walker::walk(&root, &walker::WalkOptions::default(), cancellation)?;
    scan_candidates(&root, cache, cancellation, walk.candidates, walk.policy)
}

fn scan_candidates(
    dir: &Path,
    cache: &mut CacheState,
    cancellation: &Cancellation,
    candidates: Vec<walker::Candidate>,
    policy: super::policy::SourcePolicy,
) -> Result<ScanResult> {
    let mut manifest = Vec::with_capacity(candidates.len());
    let mut changed: HashMap<String, PreparedFile> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(candidates.len());
    let mut cached_files = 0usize;
    let mut cache_dirty = false;
    let mut retained_bytes = 0;

    for c in candidates {
        cancellation.check()?;
        seen.insert(c.rel.clone());
        let bytes = source::read_bytes(dir, &c.rel, cancellation)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let cached = cache.files.get(&c.rel).filter(|stamp| stamp.hash == hash);
        if let Some(stamp) = cached {
            if stamp.indexable {
                cached_files += 1;
                manifest.push(api::ManifestEntry {
                    path: c.rel,
                    hash,
                    size: i64::try_from(bytes.len())
                        .context("source file size exceeds manifest limit")?,
                });
            }
            continue;
        }

        let Ok(content) = String::from_utf8(bytes) else {
            debug!(rel = %c.rel, "skip — non-UTF-8");
            cache.files.insert(
                c.rel.clone(),
                CachedContent {
                    hash,
                    indexable: false,
                },
            );
            cache_dirty = true;
            continue;
        };
        if !walker::is_indexable(&content) {
            debug!(rel = %c.rel, "skip — blank / generated / minified");
            cache.files.insert(
                c.rel.clone(),
                CachedContent {
                    hash,
                    indexable: false,
                },
            );
            cache_dirty = true;
            continue;
        }
        let prepared = PreparedFile { content, hash };
        cache.files.insert(
            c.rel.clone(),
            CachedContent {
                hash: prepared.hash.clone(),
                indexable: true,
            },
        );
        cache_dirty = true;
        manifest.push(api::ManifestEntry {
            path: c.rel.clone(),
            hash: prepared.hash.clone(),
            size: i64::try_from(prepared.content.len())
                .context("source file size exceeds manifest limit")?,
        });
        if retained_bytes + prepared.content.len() <= RETAINED_CONTENT_BYTES {
            retained_bytes += prepared.content.len();
            changed.insert(c.rel, prepared);
        }
    }
    // Drop decisions for files that vanished so the cache can't grow unbounded.
    cancellation.check()?;
    policy.verify(cancellation)?;
    persist_scan_cache(cache, &seen, cache_dirty);
    debug!(files = manifest.len(), changed = changed.len(), "scanned");
    Ok(ScanResult {
        manifest,
        changed,
        cached_files,
        policy,
    })
}

fn persist_scan_cache(cache: &mut CacheState, seen: &HashSet<String>, mut dirty: bool) {
    let cached_before_retain = cache.files.len();
    cache.files.retain(|path, _| seen.contains(path));
    dirty |= cache.files.len() != cached_before_retain;
    if dirty {
        cache.save_persistent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn repeat_scan_reuses_byte_verified_content_decisions() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join("blank.txt"), " \n\t").unwrap();
        fs::write(
            temp.path().join("generated.rs"),
            "// @generated by a test\nstruct Generated;\n",
        )
        .unwrap();
        let mut cache = CacheState::default();

        let first = scan_directory(temp.path(), &mut cache, &Cancellation::default()).unwrap();
        assert_eq!(first.manifest.len(), 1);
        assert_eq!(first.changed.len(), 1);
        assert_eq!(first.cached_files, 0);
        assert!(
            cache
                .files
                .get("blank.txt")
                .is_some_and(|stamp| !stamp.indexable)
        );
        assert!(
            cache
                .files
                .get("generated.rs")
                .is_some_and(|stamp| !stamp.indexable)
        );

        let second = scan_directory(temp.path(), &mut cache, &Cancellation::default()).unwrap();
        assert_eq!(second.manifest.len(), 1);
        assert!(second.changed.is_empty());
        assert_eq!(second.cached_files, 1);
    }

    #[test]
    fn a_file_read_failure_aborts_the_complete_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let cancellation = Cancellation::default();
        let walk = walker::walk(&root, &walker::WalkOptions::default(), &cancellation).unwrap();
        fs::remove_file(root.join("main.rs")).unwrap();
        let result = scan_candidates(
            &root,
            &mut CacheState::default(),
            &cancellation,
            walk.candidates,
            walk.policy,
        );
        assert!(result.is_err());
    }

    #[test]
    fn retained_scan_content_has_a_fixed_memory_limit() {
        let temp = tempfile::tempdir().unwrap();
        let content = "x\n".repeat(RETAINED_CONTENT_BYTES / 2 + 1);
        fs::write(temp.path().join("large.txt"), content).unwrap();
        let scan = scan_directory(
            temp.path(),
            &mut CacheState::default(),
            &Cancellation::default(),
        )
        .unwrap();
        assert_eq!(scan.manifest.len(), 1);
        assert!(scan.changed.is_empty());
    }
}

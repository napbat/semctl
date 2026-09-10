//! Content-verified filter decisions for complete checkout scans.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

const SYNC_CACHE_FORMAT_VERSION: u32 = 4;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct CachedContent {
    pub(super) hash: String,
    /// The content policy was evaluated on bytes with this exact hash.
    pub(super) indexable: bool,
}

#[derive(Deserialize, Serialize)]
struct StoredSyncCache {
    format_version: u32,
    files: HashMap<String, CachedContent>,
}

#[derive(Serialize)]
struct StoredSyncCacheView<'a> {
    format_version: u32,
    files: &'a HashMap<String, CachedContent>,
}

/// Cache of content-filter decisions. Every scan reads and hashes current bytes
/// before reusing a decision. File metadata cannot prove content equality after
/// a restore or an edit that preserves timestamps. Source is never persisted.
///
/// The caller wraps it in a [`Mutex`], which doubles as the lock that serializes
/// concurrent syncs (startup vs watch vs periodic) to a single codebase.
#[derive(Default)]
pub struct SyncCache {
    // The blocking scanner owns this lock. Cancelling its async caller cannot
    // let another scan access the cache before that worker stops.
    pub(super) state: Arc<Mutex<CacheState>>,
}

#[derive(Default)]
pub(super) struct CacheState {
    pub(super) files: HashMap<String, CachedContent>,
    persist: bool,
    persisted_source_id: Option<String>,
    persistence_path: Option<PathBuf>,
}

impl SyncCache {
    /// Create a cache that survives separate CLI processes. Cache failures only
    /// cost performance: indexing evaluates the content filters again.
    #[must_use]
    pub fn persistent() -> Self {
        Self {
            state: Arc::new(Mutex::new(CacheState {
                persist: true,
                ..CacheState::default()
            })),
        }
    }
}

impl CacheState {
    pub(super) fn load_persistent(&mut self, source_id: &str) {
        if !self.persist || self.persisted_source_id.as_deref() == Some(source_id) {
            return;
        }
        self.files.clear();
        self.persisted_source_id = Some(source_id.to_string());
        self.persistence_path = None;

        let path = match crate::config::sync_cache_dir() {
            Ok(dir) => dir.join(format!("{source_id}.json")),
            Err(error) => {
                warn!(%error, "sync cache unavailable; evaluating all content filters");
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

    pub(super) fn save_persistent(&self) {
        let Some(path) = self.persistence_path.as_deref() else {
            return;
        };
        if let Err(error) = write_persistent_cache(path, &self.files) {
            warn!(path = %path.display(), %error, "couldn't save sync cache");
        }
    }
}

fn read_persistent_cache(path: &Path) -> Result<Option<HashMap<String, CachedContent>>> {
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

fn write_persistent_cache(path: &Path, files: &HashMap<String, CachedContent>) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decision() -> CachedContent {
        CachedContent {
            hash: "abc123".to_string(),
            indexable: true,
        }
    }

    #[test]
    fn persistent_cache_round_trips_content_decisions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sync-cache").join("cache.json");
        let files = HashMap::from([
            ("src/main.rs".to_string(), decision()),
            (
                "generated.rs".to_string(),
                CachedContent {
                    hash: "excluded".into(),
                    indexable: false,
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
            files: HashMap::from([("src/main.rs".to_string(), decision())]),
        };
        fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();

        assert_eq!(read_persistent_cache(&path).unwrap(), None);
    }
}

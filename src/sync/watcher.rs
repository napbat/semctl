//! Realtime filesystem watcher for `semctl mcp`.
//!
//! The periodic re-sync (`spawn_periodic_resync` in [`super::background`]) is the
//! drift backstop; this is the low-latency complement. `notify` (debounced) wakes us
//! on each burst of edits and we trigger a full manifest re-sync — the same
//! reconcile path as the periodic timer, just event-driven. Re-walking and
//! diffing the whole tree handles creates, edits, deletes and renames uniformly,
//! with no event classification or incremental delete endpoint needed.
//!
//! Events under the VCS dir or matched by the root gitignore are filtered out so
//! a `cargo build` / `git` operation doesn't spin the sync.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::SyncCache;
use crate::client::Client;

/// Debounce window: collapse a burst of saves into a single re-sync.
const DEBOUNCE_MS: u64 = 750;

/// Begin watching `dir`. Returns the watcher guard — dropping it stops the
/// watch, so the caller keeps it alive for the server's lifetime. `None` when
/// the platform watch can't be established, in which case the periodic re-sync
/// still covers drift.
pub(super) fn spawn(
    client: Client,
    dir: PathBuf,
    cache: Arc<Mutex<SyncCache>>,
    last_job: Arc<Mutex<Option<super::LastJob>>>,
) -> Option<Debouncer<RecommendedWatcher, RecommendedCache>> {
    // Wake-only channel: the re-sync re-walks the whole tree, so we forward
    // "something interesting changed", not which paths.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let gi = build_ignore(&dir);
    let mut debouncer = match new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| match result {
            Ok(events) => {
                if events.iter().any(|e| is_interesting(&e.paths, &gi)) {
                    let _ = tx.send(());
                }
            }
            Err(errs) => {
                for e in errs {
                    warn!(error = %e, "watcher error");
                }
            }
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "fs watcher unavailable; relying on periodic re-sync");
            return None;
        }
    };

    if let Err(e) = debouncer.watch(&dir, RecursiveMode::Recursive) {
        warn!(error = %e, "fs watch registration failed; relying on periodic re-sync");
        return None;
    }
    info!(dir = %dir.display(), debounce_ms = DEBOUNCE_MS, "fs watcher active");

    // Single-flight consumer: await each re-sync before taking the next wake,
    // draining wakes that piled up during it so a burst collapses to one pass.
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            while rx.try_recv().is_ok() {}
            match super::sync(&client, &dir, &cache).await {
                Ok(o) if o.uploaded > 0 || o.to_delete > 0 => {
                    info!(
                        uploaded = o.uploaded,
                        to_delete = o.to_delete,
                        job = %o.job_id,
                        "watch re-sync pushed changes",
                    );
                    super::record_job(&last_job, &o).await;
                }
                Ok(_) => debug!("watch re-sync: no changes"),
                Err(e) => warn!(error = %format!("{e:#}"), "watch re-sync failed"),
            }
        }
    });

    Some(debouncer)
}

/// Root gitignore matcher (root `.gitignore` + `.semctlignore`). Best-effort: a
/// missing/unreadable file just yields a matcher that ignores nothing.
fn build_ignore(root: &Path) -> Gitignore {
    let mut b = GitignoreBuilder::new(root);
    let _ = b.add(root.join(".gitignore"));
    let _ = b.add(root.join(".semctlignore"));
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Whether a debounced event touches anything worth re-syncing for: a path
/// outside the VCS dir that the root gitignore doesn't exclude. Coarser than the
/// walker (it doesn't chase nested gitignores), but enough to keep build-artifact
/// churn from spinning the sync — anything that slips through costs only one
/// no-op walk.
fn is_interesting(paths: &[PathBuf], gi: &Gitignore) -> bool {
    paths.iter().any(|p| {
        if p.components().any(|c| c.as_os_str() == ".git") {
            return false;
        }
        let is_dir = p.is_dir();
        !gi.matched_path_or_any_parents(p, is_dir).is_ignore()
    })
}

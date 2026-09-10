//! Realtime filesystem watcher for `semctl mcp`.
//!
//! The periodic re-sync (`spawn_periodic_resync` in [`super::background`]) is the
//! drift backstop; this is the low-latency complement. `notify` (debounced) wakes us
//! on each burst of edits and we trigger a full manifest re-sync — the same
//! reconcile path as the periodic timer, just event-driven. Re-walking and
//! diffing the whole tree handles creates, edits, deletes and renames uniformly,
//! with no incremental delete endpoint needed.
//!
//! Read/open access events are ignored because the re-sync itself walks and opens
//! the watched tree. Letting those events through makes each completed sync queue
//! its successor forever on platforms whose watcher reports file access. Events
//! under the VCS dir or matched by the root gitignore are filtered out too, so a
//! `cargo build` / `git` operation doesn't spin the sync.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::{
    SyncCache,
    blocking::Cancellation,
    policy::{SourcePolicy, event_may_affect_policy},
};
use crate::client::Client;

/// Debounce window: collapse a burst of saves into a single re-sync.
const DEBOUNCE_MS: u64 = 750;

type WatchControl = std::sync::Mutex<Debouncer<RecommendedWatcher, RecommendedCache>>;

/// Begin watching `dir`. Returns the watcher guard — dropping it stops the
/// watch, so the caller keeps it alive for the server's lifetime. `None` when
/// the platform watch can't be established, in which case the periodic re-sync
/// still covers drift.
pub(super) fn spawn(
    client: Client,
    dir: PathBuf,
    cache: Arc<Mutex<SyncCache>>,
    jobs: Arc<super::JobRegistry>,
) -> Option<Arc<WatchControl>> {
    // Wake-only channel: the re-sync re-walks the whole tree, so we forward
    // "something interesting changed", not which paths.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let initial_policy = load_initial_policy(&dir);
    let mut observed: std::collections::HashSet<_> = initial_policy
        .as_ref()
        .into_iter()
        .flat_map(SourcePolicy::observed_sources)
        .collect();

    let ignore_root = dir.clone();
    let mut debouncer = match new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| match result {
            Ok(events) => {
                if !events.iter().any(|event| {
                    can_change_tree(&event.event)
                        && event
                            .paths
                            .iter()
                            .any(|path| event_may_affect_policy(&ignore_root, path, &observed))
                }) {
                    return;
                }
                // Notifications and manifests share the checked policy engine.
                // A load error still wakes the scanner, which reports the error
                // and refuses upload instead of treating unreadable rules as empty.
                let interesting = match SourcePolicy::load(&ignore_root, &Cancellation::default()) {
                    Ok(mut policy) => {
                        let interesting = events
                            .iter()
                            .any(|event| is_interesting(&event.event, &mut policy));
                        observed.extend(policy.observed_sources());
                        interesting
                    }
                    Err(_) => true,
                };
                if interesting {
                    // A full channel has a wake-up; a closed channel has no consumer.
                    let _ = tx.try_send(());
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
    let mut registered = std::collections::HashSet::new();
    if let Some(policy) = initial_policy {
        register_external_watches(&mut debouncer, &policy, &mut registered);
    }
    let debouncer = Arc::new(std::sync::Mutex::new(debouncer));
    let control = Arc::downgrade(&debouncer);
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
                    super::record_job(&jobs, &o).await;
                }
                Ok(_) => debug!("watch re-sync: no changes"),
                Err(e) => warn!(error = %format!("{e:#}"), "watch re-sync failed"),
            }
            // A config change can select a rule in a different directory. Add
            // that directory after reconciliation so later rule edits wake us.
            let Some(control) = control.upgrade() else {
                break;
            };
            let root = dir.clone();
            let mut previous = registered.clone();
            match super::blocking::run(move |cancellation| {
                let policy = SourcePolicy::load(&root, &cancellation)?;
                let mut watcher = control
                    .lock()
                    .map_err(|_| anyhow::anyhow!("watch control lock is poisoned"))?;
                register_external_watches(&mut watcher, &policy, &mut previous);
                Ok(previous)
            })
            .await
            {
                Ok(current) => registered = current,
                Err(error) => {
                    warn!(%error, "source policy watch refresh failed; periodic sync covers changes");
                }
            }
        }
    });

    Some(debouncer)
}

fn load_initial_policy(dir: &std::path::Path) -> Option<SourcePolicy> {
    match SourcePolicy::load(dir, &Cancellation::default()) {
        Ok(policy) => Some(policy),
        Err(error) => {
            warn!(%error, "source policy unavailable at watcher startup; relying on periodic re-sync");
            None
        }
    }
}

/// Watch rule parents so creation and atomic replacement are visible. For a
/// parent that does not exist yet, watch its closest existing ancestor first.
fn register_external_watches(
    watcher: &mut Debouncer<RecommendedWatcher, RecommendedCache>,
    policy: &SourcePolicy,
    registered: &mut std::collections::HashSet<PathBuf>,
) {
    for path in policy.external_sources() {
        let parent = path
            .parent()
            .and_then(|parent| parent.ancestors().find(|ancestor| ancestor.is_dir()));
        let Some(parent) = parent else { continue };
        if registered.contains(parent) {
            continue;
        }
        match watcher.watch(parent, RecursiveMode::NonRecursive) {
            Ok(()) => {
                registered.insert(parent.to_path_buf());
            }
            Err(error) => {
                warn!(%error, "external source policy watch unavailable; periodic sync covers changes");
            }
        }
    }
}

/// Filesystem errors wake the authoritative scanner, which fails closed.
fn is_interesting(event: &Event, policy: &mut SourcePolicy) -> bool {
    can_change_tree(event)
        && event.paths.iter().any(|path| {
            policy
                .event_is_relevant(path, path.is_dir(), &Cancellation::default())
                .unwrap_or(true)
        })
}

fn can_change_tree(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Write))
        )
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    use std::path::Path;

    use super::*;
    use crate::sync::walker::IGNORE_FILES;

    fn test_policy(root: &Path) -> SourcePolicy {
        SourcePolicy::load(root, &Cancellation::default()).unwrap()
    }

    fn event(kind: EventKind) -> Event {
        Event::new(kind).add_path(PathBuf::from("src/lib.rs"))
    }

    #[test]
    fn scan_access_does_not_schedule_another_sync() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = test_policy(temp.path());

        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Open(AccessMode::Write)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
        ] {
            assert!(
                !is_interesting(&event(kind), &mut policy),
                "accepted {kind:?}"
            );
        }
    }

    #[test]
    fn mutations_still_schedule_a_sync() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = test_policy(temp.path());

        for kind in [
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Remove(RemoveKind::File),
        ] {
            assert!(
                is_interesting(&event(kind), &mut policy),
                "rejected {kind:?}"
            );
        }
    }

    #[test]
    fn vcs_events_remain_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = test_policy(temp.path());
        let event =
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(PathBuf::from(".git/index"));

        assert!(!is_interesting(&event, &mut policy));
    }

    #[test]
    fn both_project_ignore_files_filter_watcher_events() {
        for ignore_name in IGNORE_FILES {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join(ignore_name), "private.txt\n").unwrap();
            let mut policy = test_policy(temp.path());
            let event = Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(temp.path().join("private.txt"));
            assert!(!is_interesting(&event, &mut policy));
        }
    }

    #[test]
    fn ignore_file_changes_always_schedule_a_scan() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".gitignore"),
            ".semctxignore\n.semctlignore\n",
        )
        .unwrap();
        let mut policy = test_policy(temp.path());
        for name in IGNORE_FILES {
            let event =
                Event::new(EventKind::Modify(ModifyKind::Any)).add_path(temp.path().join(name));
            assert!(is_interesting(&event, &mut policy));
        }
    }
}

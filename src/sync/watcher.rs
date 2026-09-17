//! Realtime filesystem watching for `semctl mcp`.
//!
//! The periodic re-sync (`spawn_periodic_resync` in [`super::background`]) is the
//! drift backstop; this is the low-latency complement. The process watch hub
//! ([`WatchHub`]) wakes us on each debounced burst of edits and we trigger a
//! full manifest re-sync — the same reconcile path as the periodic timer, just
//! event-driven. Re-walking and diffing the whole tree handles creates, edits,
//! deletes and renames uniformly, with no incremental delete endpoint needed.
//!
//! Read/open access events are ignored because the re-sync itself walks and opens
//! the watched tree. Letting those events through makes each completed sync queue
//! its successor forever on platforms whose watcher reports file access. Events
//! under the VCS dir or matched by the root gitignore are filtered out too, so a
//! `cargo build` / `git` operation doesn't spin the sync.
//!
//! Every decision that needs the source policy runs on the blocking pool. The
//! hub's notify thread serves every watched checkout in the process, so it must
//! never read a file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::{
    SyncCache, SyncLimits, blocking,
    blocking::Cancellation,
    policy::{SourcePolicy, event_may_affect_policy},
};
use crate::client::Client;
use crate::engine::watch_hub::{BATCH_CAPACITY, WatchHub, WatchRegistration, WatchSink};

/// Begin watching `dir`. Returns whether the watch is active; when it is not,
/// the periodic re-sync still covers drift.
///
/// **Blocking.** Registration walks the tree to seed the watcher's file-id
/// cache, so the caller runs this on the blocking pool.
pub(super) fn spawn(
    client: Client,
    dir: PathBuf,
    cache: Arc<Mutex<SyncCache>>,
    jobs: Arc<super::JobRegistry>,
    limits: SyncLimits,
) -> bool {
    let hub = Arc::new(WatchHub::new());
    let (sender, mut batches) = tokio::sync::mpsc::channel(BATCH_CAPACITY);
    let overflow = Arc::new(AtomicBool::new(false));
    // The registration owns this checkout's share of the hub, including the hub
    // itself: dropping it releases the root watch and every external watch it
    // alone held.
    let registration = match hub.register(dir.clone(), WatchSink::new(sender, overflow.clone())) {
        Ok(registration) => Arc::new(registration),
        Err(error) => {
            warn!(
                error = %format!("{error:#}"),
                "fs watch registration failed; relying on periodic re-sync"
            );
            return false;
        }
    };

    let mut observed = HashSet::new();
    if let Ok(policy) = SourcePolicy::load(&dir, &Cancellation::default()) {
        observed.extend(policy.observed_sources());
        for path in policy.external_sources() {
            registration.watch_external(&path);
        }
    }

    // Single-flight consumer: await each re-sync before taking the next batch,
    // draining batches that piled up during it so a burst collapses to one pass.
    tokio::spawn(async move {
        let registration = registration;
        while let Some(batch) = batches.recv().await {
            let mut events = batch.events;
            while let Ok(next) = batches.try_recv() {
                events.extend(next.events);
            }
            // Dropped events can name any path, so an overflow makes this run
            // unconditional instead of judging it by what survived.
            let dropped = overflow.swap(false, Ordering::AcqRel);
            let (relevant, next_observed) = relevance(&dir, events, observed).await;
            observed = next_observed;
            if !dropped && !relevant {
                continue;
            }
            match super::sync(&client, &dir, &cache, &limits).await {
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
            refresh_external_watches(&registration, &dir).await;
        }
    });
    true
}

/// Whether this batch justifies a re-sync, and the policy sources to keep
/// watching for the next one.
///
/// Runs on the blocking pool: the cheap event filter is followed by a policy
/// load, which reads rule files. A load error reports the batch as relevant, so
/// the authoritative scanner runs, reports the error, and refuses to upload
/// rather than treating unreadable rules as empty.
async fn relevance(
    root: &Path,
    events: Vec<Event>,
    observed: HashSet<PathBuf>,
) -> (bool, HashSet<PathBuf>) {
    let root = root.to_path_buf();
    blocking::run(move |cancellation| {
        if !events.iter().any(|event| {
            can_change_tree(event)
                && event
                    .paths
                    .iter()
                    .any(|path| event_may_affect_policy(&root, path, &observed))
        }) {
            return Ok((false, observed));
        }
        // Notifications and manifests share the checked policy engine.
        let Ok(mut policy) = SourcePolicy::load(&root, &cancellation) else {
            return Ok((true, observed));
        };
        let relevant = events
            .iter()
            .any(|event| is_interesting(event, &mut policy));
        let mut observed = observed;
        observed.extend(policy.observed_sources());
        Ok((relevant, observed))
    })
    .await
    // A cancelled or panicking worker leaves the decision to the scanner.
    .unwrap_or_else(|_| (true, HashSet::new()))
}

/// Re-read the policy and subscribe to every rule directory it now names.
///
/// Best effort: a failure is logged and the periodic sync covers the change.
async fn refresh_external_watches(registration: &Arc<WatchRegistration>, root: &Path) {
    let registration = registration.clone();
    let root = root.to_path_buf();
    let refreshed = blocking::run(move |cancellation| {
        let policy = SourcePolicy::load(&root, &cancellation)?;
        for path in policy.external_sources() {
            registration.watch_external(&path);
        }
        Ok(())
    })
    .await;
    if let Err(error) = refreshed {
        warn!(%error, "source policy watch refresh failed; periodic sync covers changes");
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

    /// An event the policy excludes must not schedule a sync, and the rule
    /// files the check read must stay observed for the next batch.
    #[tokio::test]
    async fn the_relevance_check_applies_the_source_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();

        let (relevant, observed) = relevance(
            &root,
            vec![Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("ignored.txt"))],
            HashSet::new(),
        )
        .await;
        assert!(!relevant, "an ignored file must not schedule a sync");
        assert!(
            observed.iter().any(|path| path.ends_with(".gitignore")),
            "the rule files it read must stay observed"
        );

        let (relevant, _) = relevance(
            &root,
            vec![Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("src/lib.rs"))],
            HashSet::new(),
        )
        .await;
        assert!(relevant, "a source edit must schedule a sync");
    }
}

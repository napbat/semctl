//! One filesystem watcher for every checkout in the process.
//!
//! `notify` costs a platform resource per instance — on Linux one inotify
//! instance per watcher, and the per-user instance limit is small. One watcher
//! per checkout therefore stops scaling long before 1,000 checkouts. The hub
//! keeps one debounced watcher and routes each event to every registration that
//! observes the event's path.
//!
//! The hub does no filesystem work and no policy work on the notify thread. It
//! takes a read lock on the route table, copies the matching events into each
//! registration's bounded channel, and returns. Deciding whether an event
//! matters — which needs the source policy, and therefore file reads — belongs
//! to the registration's own task, on the blocking pool.
//!
//! Lock order inside the hub is: watcher state, then routes. The notify
//! callback only ever takes the routes lock, so a registration that holds the
//! watcher state lock cannot deadlock against an arriving event.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Debounce window: collapse a burst of saves into one batch.
const DEBOUNCE: Duration = Duration::from_millis(750);

/// Batches one registration can hold before the hub reports overflow. A
/// registration that is already this far behind learns nothing from another
/// batch: its next run re-walks the whole tree anyway.
pub(crate) const BATCH_CAPACITY: usize = 64;

/// One debounced group of events for one registration.
///
/// The event kind and the paths are both kept. The receiver needs the kind to
/// drop access-only events and the paths to ask the source policy whether the
/// change is relevant.
pub(crate) struct WatchBatch {
    pub(crate) events: Vec<Event>,
}

/// Where one registration's batches go.
///
/// The channel is bounded and the flag is the escape hatch. When the channel is
/// full, or the platform watcher reports an error, the flag says "something
/// changed that you did not see", and the receiver must treat its next run as
/// unconditional.
#[derive(Clone)]
pub(crate) struct WatchSink {
    events: mpsc::Sender<WatchBatch>,
    overflow: Arc<AtomicBool>,
}

impl WatchSink {
    pub(crate) fn new(events: mpsc::Sender<WatchBatch>, overflow: Arc<AtomicBool>) -> Self {
        Self { events, overflow }
    }

    /// Hand one batch over without waiting. The notify thread must never block
    /// on a slow receiver, because it serves every other checkout too.
    fn deliver(&self, batch: WatchBatch) {
        if self.events.try_send(batch).is_err() {
            self.report_overflow();
        }
    }

    /// Record that events were lost, and wake the receiver so it acts on it.
    ///
    /// A failed wake is safe to ignore: the channel is full, so the receiver
    /// already has a batch to wake on, and it reads the flag before it runs.
    fn report_overflow(&self) {
        self.overflow.store(true, Ordering::Release);
        let _ = self.events.try_send(WatchBatch { events: Vec::new() });
    }
}

/// One registration's view of the filesystem.
struct Route {
    /// Canonical working-copy root, watched recursively.
    root: PathBuf,
    /// Directories outside `root` that hold a source-policy rule this
    /// registration depends on, such as a global gitignore file.
    externals: HashSet<PathBuf>,
    /// The externals this registration holds a platform watch share of. A
    /// directory inside a root that is already watched recursively needs no
    /// watch of its own, so it is routed without being counted here.
    watched: HashSet<PathBuf>,
    sink: WatchSink,
}

impl Route {
    /// Whether this registration observes `path`.
    ///
    /// The root matches by ancestor: it is watched recursively, so a
    /// registration observes everything under it. An external directory
    /// matches only itself and its direct children, because an external watch
    /// is not recursive and reports nothing deeper. Matching an external by
    /// ancestor would route far too much: the source policy records an absent
    /// rule file for every ancestor of the root up to the filesystem root, so
    /// every coordinator would observe every event on the machine.
    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
            || self
                .externals
                .iter()
                .any(|external| observes_external(external, path))
    }

    fn observes(&self, event: &Event) -> bool {
        event.paths.iter().any(|path| self.contains(path))
    }
}

/// Whether a non-recursive watch on `external` can report `path`.
fn observes_external(external: &Path, path: &Path) -> bool {
    path == external || path.parent() == Some(external)
}

/// Registrations by id. An id is never reused, so a dropped registration
/// cannot be confused with a later one for the same root.
type Routes = HashMap<u64, Route>;

/// The platform watcher and the watches it holds.
struct WatcherState {
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    /// How many registrations depend on each recursive root watch. Two
    /// registrations share one root when two sessions attach the same checkout
    /// at the same time, and when two credential scopes need two coordinators
    /// for one root. The watch is released when the last of them goes.
    roots: HashMap<PathBuf, usize>,
    /// How many registrations depend on each non-recursive external watch.
    /// The watch is released when the last of them goes.
    externals: HashMap<PathBuf, usize>,
    registrations: usize,
}

/// One debounced watcher for the whole process.
pub(crate) struct WatchHub {
    /// `None` until the first registration and again after the last one, so a
    /// process with no watched checkout holds no platform watcher.
    watcher: Mutex<Option<WatcherState>>,
    routes: Arc<RwLock<Routes>>,
    next_id: AtomicU64,
}

impl WatchHub {
    pub(crate) fn new() -> Self {
        Self {
            watcher: Mutex::new(None),
            routes: Arc::new(RwLock::new(Routes::new())),
            next_id: AtomicU64::new(0),
        }
    }

    /// Watch `root` recursively and send its events to `sink`.
    ///
    /// **Blocking.** The debouncer walks the tree to seed its file-id cache, so
    /// this can take seconds on a large checkout. Callers must run it on the
    /// blocking pool, never on a runtime worker.
    ///
    /// An error means this root has no watcher. The caller keeps serving and
    /// falls back to its periodic re-sync; the reason belongs in its status.
    pub(crate) fn register(
        self: &Arc<Self>,
        root: PathBuf,
        sink: WatchSink,
    ) -> Result<WatchRegistration> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut state = lock(&self.watcher);
        let watcher = match state.as_mut() {
            Some(watcher) => watcher,
            None => state.insert(WatcherState {
                debouncer: self.new_debouncer()?,
                roots: HashMap::new(),
                externals: HashMap::new(),
                registrations: 0,
            }),
        };

        // Publish the route before the watch so an event that arrives during
        // registration is delivered rather than dropped.
        write_routes(&self.routes).insert(
            id,
            Route {
                root: root.clone(),
                externals: HashSet::new(),
                watched: HashSet::new(),
                sink,
            },
        );
        // Another registration may already watch this root. One watch serves
        // both, and the route table decides who hears each event.
        if let Some(holders) = watcher.roots.get_mut(&root) {
            *holders += 1;
        } else if covered_by_a_root(watcher, &root) {
            // A checkout inside another registered checkout. The outer root is
            // watched recursively, so its watch already reports this tree.
            // Adding a second watch for the same directory would overwrite the
            // outer watch's recursive flag.
            watcher.roots.insert(root.clone(), 1);
        } else if let Err(error) = watcher
            .debouncer
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("watch {}", root.display()))
        {
            write_routes(&self.routes).remove(&id);
            if watcher.registrations == 0 {
                // Nothing else needs the watcher this call created.
                *state = None;
            }
            return Err(error);
        } else {
            watcher.roots.insert(root.clone(), 1);
        }
        watcher.registrations += 1;
        info!(root = %root.display(), debounce_ms = DEBOUNCE.as_millis(), "fs watcher active");
        Ok(WatchRegistration {
            hub: self.clone(),
            id,
            root,
        })
    }

    /// The debouncer callback owns a handle to the route table and nothing
    /// else. It must stay free of filesystem access: it runs on the notify
    /// thread, which every watched checkout in the process shares.
    fn new_debouncer(&self) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
        let routes = self.routes.clone();
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let routes = read_routes(&routes);
                match result {
                    Ok(events) => {
                        let events: Vec<Event> =
                            events.into_iter().map(|event| event.event).collect();
                        dispatch(&routes, &events);
                    }
                    Err(errors) => {
                        for error in &errors {
                            warn!(%error, "watcher error");
                        }
                        // A lost event can name any path, so every watched
                        // checkout has to treat its next run as unconditional.
                        for route in routes.values() {
                            route.sink.report_overflow();
                        }
                    }
                }
            },
        )
        .context("start the filesystem watcher")
    }

    /// Add a non-recursive watch for `parent` on behalf of `id`.
    ///
    /// **Blocking.** It touches the platform watcher.
    ///
    /// Best effort: a parent that cannot be watched is logged and left
    /// unwatched, because the periodic re-sync still covers the rule change.
    fn add_external(&self, id: u64, parent: &Path) {
        let mut state = lock(&self.watcher);
        let Some(watcher) = state.as_mut() else {
            return;
        };
        let mut routes = write_routes(&self.routes);
        let Some(route) = routes.get_mut(&id) else {
            return;
        };
        if !route.externals.insert(parent.to_path_buf()) {
            return;
        }
        if covered_by_a_root(watcher, parent) {
            // A recursive root watch already reports this directory. Watching
            // it again without recursion would overwrite that root's flag and
            // leave the whole checkout reporting one directory only.
            return;
        }
        match watcher.externals.get_mut(parent) {
            // Another registration already watches it. One watch serves both.
            Some(holders) => {
                *holders += 1;
                route.watched.insert(parent.to_path_buf());
            }
            None => match watcher.debouncer.watch(parent, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watcher.externals.insert(parent.to_path_buf(), 1);
                    route.watched.insert(parent.to_path_buf());
                }
                Err(error) => {
                    route.externals.remove(parent);
                    warn!(%error, "external source policy watch unavailable; periodic sync covers changes");
                }
            },
        }
    }

    /// Release everything `id` holds: its root watch, its share of every
    /// external watch, and its route. The watcher itself goes when the last
    /// registration does.
    fn release(&self, id: u64, root: &Path) {
        let mut state = lock(&self.watcher);
        let Some(watcher) = state.as_mut() else {
            return;
        };
        let route = write_routes(&self.routes).remove(&id);
        // The root watch is shared, so only the last holder may release it.
        // Releasing it earlier would leave every other registration on this
        // root with a coordinator that believes it is watched and never hears
        // another event.
        if let Some(holders) = watcher.roots.get_mut(root) {
            *holders -= 1;
            if *holders == 0 {
                watcher.roots.remove(root);
                if let Err(error) = watcher.debouncer.unwatch(root) {
                    debug!(%error, root = %root.display(), "unwatch after release");
                }
                rewatch_around(watcher, root);
            }
        }
        for external in route.into_iter().flat_map(|route| route.watched) {
            let Some(holders) = watcher.externals.get_mut(&external) else {
                continue;
            };
            *holders -= 1;
            if *holders == 0 {
                watcher.externals.remove(&external);
                if let Err(error) = watcher.debouncer.unwatch(&external) {
                    debug!(%error, path = %external.display(), "unwatch external source");
                }
            }
        }
        watcher.registrations -= 1;
        if watcher.registrations == 0 {
            // Dropping the debouncer stops its thread and releases the
            // platform resource. Holding an idle watcher would keep an inotify
            // instance for a checkout nothing is serving.
            *state = None;
            debug!("last watch registration released; filesystem watcher stopped");
        }
    }

    /// How many roots the hub watches. Test-only: production code asks each
    /// coordinator for its own watch state.
    #[cfg(test)]
    fn watched_roots(&self) -> usize {
        lock(&self.watcher)
            .as_ref()
            .map_or(0, |watcher| watcher.registrations)
    }

    /// Whether a platform watcher exists at all.
    #[cfg(test)]
    fn has_watcher(&self) -> bool {
        lock(&self.watcher).is_some()
    }

    /// How many registrations share the watch on `root`. Test-only.
    #[cfg(test)]
    fn root_holders(&self, root: &Path) -> usize {
        lock(&self.watcher)
            .as_ref()
            .and_then(|watcher| watcher.roots.get(root).copied())
            .unwrap_or(0)
    }
}

/// One checkout's claim on the hub.
///
/// Dropping it releases the root watch, every external watch it alone held, and
/// its route. The coordinator that owns it must drop it after it cancels its
/// task, so no run can be woken by a registration that is going away.
pub(crate) struct WatchRegistration {
    hub: Arc<WatchHub>,
    id: u64,
    root: PathBuf,
}

impl WatchRegistration {
    /// Watch the directory that holds `path`, so creating or atomically
    /// replacing the rule file is visible.
    ///
    /// A rule file that does not exist yet has no parent to watch, so the
    /// closest existing ancestor is used instead. Reference counted across
    /// registrations: several checkouts share one watch on a global rule file.
    ///
    /// **Blocking.** It probes the filesystem and touches the platform
    /// watcher. Callers must run it on the blocking pool.
    pub(crate) fn watch_external(&self, path: &Path) {
        let parent = path
            .parent()
            .and_then(|parent| parent.ancestors().find(|ancestor| ancestor.is_dir()));
        let Some(parent) = parent else { return };
        self.hub.add_external(self.id, parent);
    }
}

impl Drop for WatchRegistration {
    fn drop(&mut self) {
        self.hub.release(self.id, &self.root);
    }
}

/// Whether some registered root already watches `path` recursively.
///
/// The root itself is not a cover for itself: the caller decides what to
/// do about a watch on the same directory.
fn covered_by_a_root(watcher: &WatcherState, path: &Path) -> bool {
    watcher
        .roots
        .keys()
        .any(|root| root != path && path.starts_with(root))
}

/// Restore the watches that removing the watch on `removed` also took.
///
/// `notify` keeps one entry per directory with a recursive flag, and
/// removing a recursive entry removes every watch whose path starts with
/// it. Two kinds of watch this hub still needs can therefore be gone: a
/// nested root inside `removed`, and the coverage an enclosing root had of
/// the `removed` subtree. Both are re-established here.
///
/// Best effort: a path that cannot be watched again is logged, and that
/// checkout falls back to its periodic re-sync.
fn rewatch_around(watcher: &mut WatcherState, removed: &Path) {
    let affected: Vec<PathBuf> = watcher
        .roots
        .keys()
        .filter(|root| root.starts_with(removed) || removed.starts_with(root))
        .cloned()
        .collect();
    for root in affected {
        if let Err(error) = watcher.debouncer.watch(&root, RecursiveMode::Recursive) {
            warn!(%error, root = %root.display(), "could not watch this root again after a nested root was released");
        }
    }
    let externals: Vec<PathBuf> = watcher
        .externals
        .keys()
        .filter(|external| external.starts_with(removed))
        .cloned()
        .collect();
    for external in externals {
        if let Err(error) = watcher
            .debouncer
            .watch(&external, RecursiveMode::NonRecursive)
        {
            warn!(%error, path = %external.display(), "could not watch this rule directory again after a root was released");
        }
    }
}

/// Copy each event into every registration that observes one of its paths.
///
/// Pure with respect to the filesystem: it reads the route table and the event
/// paths, and nothing else. That is what keeps the notify thread available to
/// the other 999 checkouts.
fn dispatch(routes: &Routes, events: &[Event]) {
    for route in routes.values() {
        let matched: Vec<Event> = events
            .iter()
            .filter(|event| can_change_tree(event) && route.observes(event))
            .cloned()
            .collect();
        if !matched.is_empty() {
            route.sink.deliver(WatchBatch { events: matched });
        }
    }
}

/// Whether an event can have changed the tree it names.
///
/// Read and open events are ignored: a reconcile walks and opens the watched
/// tree itself, so letting them through would make each finished sync queue its
/// successor forever on a platform that reports file access. The filter is
/// applied here, before the event enters a channel, so an access burst costs no
/// channel capacity and cannot overflow a registration that has nothing to do.
pub(crate) fn can_change_tree(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Write))
        )
}

/// A poisoned hub lock means a previous holder panicked while the watcher was
/// consistent enough to describe: the data behind it is a watcher handle and
/// two maps. Recovering keeps watching; refusing would leave the process with
/// no watcher and no way to get one back.
fn lock(watcher: &Mutex<Option<WatcherState>>) -> MutexGuard<'_, Option<WatcherState>> {
    watcher.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read_routes(routes: &RwLock<Routes>) -> RwLockReadGuard<'_, Routes> {
    routes.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_routes(routes: &RwLock<Routes>) -> std::sync::RwLockWriteGuard<'_, Routes> {
    routes.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use notify::EventKind;
    use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
    use tokio::sync::mpsc;

    use super::{
        BATCH_CAPACITY, Event, HashSet, Path, PathBuf, Route, Routes, WatchBatch, WatchHub,
        WatchSink, dispatch,
    };

    struct Receiver {
        batches: mpsc::Receiver<WatchBatch>,
        overflow: Arc<AtomicBool>,
    }

    impl Receiver {
        fn paths(&mut self) -> Vec<PathBuf> {
            let mut paths = Vec::new();
            while let Ok(batch) = self.batches.try_recv() {
                paths.extend(batch.events.into_iter().flat_map(|event| event.paths));
            }
            paths
        }
    }

    fn sink(capacity: usize) -> (WatchSink, Receiver) {
        let (sender, batches) = mpsc::channel(capacity);
        let overflow = Arc::new(AtomicBool::new(false));
        (
            WatchSink::new(sender, overflow.clone()),
            Receiver { batches, overflow },
        )
    }

    fn route(root: &str, externals: &[&str], sink: WatchSink) -> Route {
        let externals = externals.iter().map(PathBuf::from).collect::<HashSet<_>>();
        Route {
            root: PathBuf::from(root),
            watched: externals.clone(),
            externals,
            sink,
        }
    }

    fn event(path: &str) -> Event {
        Event::new(EventKind::Modify(ModifyKind::Any)).add_path(PathBuf::from(path))
    }

    /// Nested checkouts are a real layout: one root inside another must reach
    /// both, and an unrelated root must reach neither.
    #[test]
    fn an_event_reaches_every_root_that_contains_it() {
        let (outer_sink, mut outer) = sink(BATCH_CAPACITY);
        let (inner_sink, mut inner) = sink(BATCH_CAPACITY);
        let (other_sink, mut other) = sink(BATCH_CAPACITY);
        let mut routes = Routes::new();
        routes.insert(0, route("/work/outer", &[], outer_sink));
        routes.insert(1, route("/work/outer/inner", &[], inner_sink));
        routes.insert(2, route("/work/other", &[], other_sink));

        dispatch(&routes, &[event("/work/outer/inner/src/lib.rs")]);

        assert_eq!(
            outer.paths(),
            vec![PathBuf::from("/work/outer/inner/src/lib.rs")]
        );
        assert_eq!(
            inner.paths(),
            vec![PathBuf::from("/work/outer/inner/src/lib.rs")]
        );
        assert!(
            other.paths().is_empty(),
            "an unrelated root must not be woken"
        );
    }

    /// A reconcile walks and opens the tree it watches. Delivering the access
    /// events that walk produces would make every finished sync queue its
    /// successor, so they never enter a channel.
    #[test]
    fn an_access_only_event_is_not_delivered() {
        let (first_sink, mut first) = sink(BATCH_CAPACITY);
        let mut routes = Routes::new();
        routes.insert(0, route("/work/a", &[], first_sink));

        dispatch(
            &routes,
            &[
                Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
                    .add_path(PathBuf::from("/work/a/src/lib.rs")),
                Event::new(EventKind::Access(AccessKind::Read))
                    .add_path(PathBuf::from("/work/a/src/lib.rs")),
            ],
        );
        assert!(first.paths().is_empty(), "an access must not wake a run");

        dispatch(
            &routes,
            &[
                Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
                    .add_path(PathBuf::from("/work/a/src/lib.rs")),
            ],
        );
        assert_eq!(
            first.paths(),
            vec![PathBuf::from("/work/a/src/lib.rs")],
            "a finished write is a change"
        );
    }

    /// An external watch is not recursive, so it reports the directory itself
    /// and its direct children only. The source policy records an absent rule
    /// file for every ancestor of a root, so an ancestor match would route
    /// every event on the machine to every coordinator.
    #[test]
    fn an_external_directory_observes_only_its_own_entries() {
        let (root_sink, mut subscriber) = sink(BATCH_CAPACITY);
        let mut routes = Routes::new();
        routes.insert(0, route("/work/a", &["/"], root_sink));

        dispatch(&routes, &[event("/tmp/other/file.rs")]);
        assert!(
            subscriber.paths().is_empty(),
            "a deep path is not reported by a non-recursive watch"
        );

        dispatch(&routes, &[event("/.gitconfig")]);
        assert_eq!(
            subscriber.paths(),
            vec![PathBuf::from("/.gitconfig")],
            "a direct child of the external directory is reported"
        );

        dispatch(&routes, &[event("/")]);
        assert_eq!(
            subscriber.paths(),
            vec![PathBuf::from("/")],
            "the external directory itself is reported"
        );
    }

    /// A global rule file lives outside every checkout. Only the registrations
    /// that subscribed to its directory hear about it.
    #[test]
    fn an_external_event_reaches_only_its_subscribers() {
        let (subscriber_sink, mut subscriber) = sink(BATCH_CAPACITY);
        let (plain_sink, mut plain) = sink(BATCH_CAPACITY);
        let mut routes = Routes::new();
        routes.insert(
            0,
            route("/work/a", &["/home/user/.config/git"], subscriber_sink),
        );
        routes.insert(1, route("/work/b", &[], plain_sink));

        dispatch(&routes, &[event("/home/user/.config/git/ignore")]);

        assert_eq!(
            subscriber.paths(),
            vec![PathBuf::from("/home/user/.config/git/ignore")]
        );
        assert!(plain.paths().is_empty());
    }

    /// Only the events a registration observes are copied into its channel; a
    /// batch is not broadcast whole.
    #[test]
    fn a_batch_carries_only_the_events_the_registration_observes() {
        let (first_sink, mut first) = sink(BATCH_CAPACITY);
        let mut routes = Routes::new();
        routes.insert(0, route("/work/a", &[], first_sink));

        dispatch(
            &routes,
            &[
                event("/work/a/src/lib.rs"),
                event("/work/b/src/lib.rs"),
                Event::new(EventKind::Create(CreateKind::File))
                    .add_path(PathBuf::from("/work/a/new.rs")),
            ],
        );

        assert_eq!(
            first.paths(),
            vec![
                PathBuf::from("/work/a/src/lib.rs"),
                PathBuf::from("/work/a/new.rs")
            ]
        );
    }

    /// A receiver that stopped draining must not stall the notify thread. The
    /// flag is what tells it to treat the next run as unconditional.
    #[test]
    fn a_full_sink_reports_overflow_instead_of_blocking() {
        let (full_sink, receiver) = sink(1);
        let mut routes = Routes::new();
        routes.insert(0, route("/work/a", &[], full_sink));

        dispatch(&routes, &[event("/work/a/first.rs")]);
        assert!(!receiver.overflow.load(Ordering::Acquire));

        dispatch(&routes, &[event("/work/a/second.rs")]);
        assert!(
            receiver.overflow.load(Ordering::Acquire),
            "a dropped batch must be reported as overflow"
        );
    }

    /// The platform watcher exists exactly while some checkout needs it.
    #[test]
    fn the_watcher_lives_only_while_a_registration_holds_it() {
        let first_root = tempfile::tempdir().expect("temporary checkout");
        let second_root = tempfile::tempdir().expect("temporary checkout");
        let hub = Arc::new(WatchHub::new());
        assert!(!hub.has_watcher(), "an idle hub holds no watcher");

        let (first_sink, _first) = sink(BATCH_CAPACITY);
        let (second_sink, _second) = sink(BATCH_CAPACITY);
        let first = hub
            .register(first_root.path().to_path_buf(), first_sink)
            .expect("watch the first checkout");
        let second = hub
            .register(second_root.path().to_path_buf(), second_sink)
            .expect("watch the second checkout");
        assert_eq!(hub.watched_roots(), 2, "one watcher serves both roots");

        drop(first);
        assert!(hub.has_watcher(), "a remaining checkout keeps the watcher");
        assert_eq!(hub.watched_roots(), 1);

        drop(second);
        assert!(
            !hub.has_watcher(),
            "the last registration must release the platform watcher"
        );
    }

    /// Two sessions can hold two registrations on one root: they attach at the
    /// same time, or they use two credential scopes. The first release must
    /// keep the watch, or the remaining coordinator never hears another event.
    #[test]
    fn a_root_watch_is_shared_and_released_with_its_last_holder() {
        let root = tempfile::tempdir().expect("temporary checkout");
        let hub = Arc::new(WatchHub::new());
        let (first_sink, _first) = sink(BATCH_CAPACITY);
        let (second_sink, _second) = sink(BATCH_CAPACITY);
        let first = hub
            .register(root.path().to_path_buf(), first_sink)
            .expect("watch the checkout once");
        let second = hub
            .register(root.path().to_path_buf(), second_sink)
            .expect("watch the checkout twice");
        assert_eq!(hub.root_holders(root.path()), 2);
        assert_eq!(hub.watched_roots(), 2, "two registrations, one root watch");

        drop(first);
        assert_eq!(
            hub.root_holders(root.path()),
            1,
            "the remaining holder keeps the root watch"
        );
        assert!(hub.has_watcher());

        drop(second);
        assert_eq!(hub.root_holders(root.path()), 0);
        assert!(
            !hub.has_watcher(),
            "the last registration must release the platform watcher"
        );
    }

    /// Wait until the registration reports an event, or give up.
    ///
    /// The debounce window is 750 ms and a platform watcher needs a moment to
    /// arm, so this polls rather than reading once.
    fn await_paths(receiver: &mut Receiver) -> Vec<PathBuf> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let paths = receiver.paths();
            if !paths.is_empty() || std::time::Instant::now() >= deadline {
                return paths;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Write a file under `root` and report whether `receiver` heard about it.
    fn hears_a_write(root: &Path, receiver: &mut Receiver) -> bool {
        let file = root.join("watched.txt");
        std::fs::write(&file, b"an edit\n").expect("write a source file");
        let paths = await_paths(receiver);
        let heard = paths.iter().any(|path| path == &file);
        // Leave nothing behind for the next write in the same test.
        let _ = std::fs::remove_file(&file);
        heard
    }

    /// One checkout inside another is a real layout. `notify` keeps one entry
    /// per directory, so a second watch on a directory replaces the first, and
    /// removing a recursive entry removes every watch below it. Either root
    /// must keep receiving events when the other one goes, in either
    /// registration order.
    #[test]
    fn nested_roots_keep_their_watches_when_the_other_is_released() {
        for outer_first in [true, false] {
            let outer_dir = tempfile::tempdir().expect("temporary checkout");
            let outer = std::fs::canonicalize(outer_dir.path()).expect("canonical outer root");
            let inner = outer.join("inner");
            std::fs::create_dir(&inner).expect("create the nested checkout");

            let hub = Arc::new(WatchHub::new());
            let (outer_sink, _outer_events) = sink(BATCH_CAPACITY);
            let (inner_sink, mut inner_events) = sink(BATCH_CAPACITY);
            let (outer_registration, inner_registration) = if outer_first {
                let first = hub
                    .register(outer.clone(), outer_sink)
                    .expect("watch the outer checkout");
                let second = hub
                    .register(inner.clone(), inner_sink)
                    .expect("watch the inner checkout");
                (first, second)
            } else {
                let second = hub
                    .register(inner.clone(), inner_sink)
                    .expect("watch the inner checkout");
                let first = hub
                    .register(outer.clone(), outer_sink)
                    .expect("watch the outer checkout");
                (first, second)
            };

            drop(outer_registration);
            assert!(
                hears_a_write(&inner, &mut inner_events),
                "the inner root must still be watched (outer registered first: {outer_first})"
            );
            drop(inner_registration);
            assert!(!hub.has_watcher(), "the last registration releases the hub");
        }
    }

    /// The other direction: the inner root goes, and the outer one keeps
    /// hearing about the tree the inner root stood in. Removing a recursive
    /// entry also removes every watch below it, and those watches were the
    /// outer root's coverage of that subtree.
    #[test]
    fn an_outer_root_keeps_its_watch_when_the_inner_root_is_released() {
        for outer_first in [true, false] {
            let outer_dir = tempfile::tempdir().expect("temporary checkout");
            let outer = std::fs::canonicalize(outer_dir.path()).expect("canonical outer root");
            let inner = outer.join("inner");
            std::fs::create_dir(&inner).expect("create the nested checkout");

            let hub = Arc::new(WatchHub::new());
            let (outer_sink, mut outer_events) = sink(BATCH_CAPACITY);
            let (inner_sink, _inner_events) = sink(BATCH_CAPACITY);
            let (outer_registration, inner_registration) = if outer_first {
                let first = hub
                    .register(outer.clone(), outer_sink)
                    .expect("watch the outer checkout");
                let second = hub
                    .register(inner.clone(), inner_sink)
                    .expect("watch the inner checkout");
                (first, second)
            } else {
                let second = hub
                    .register(inner.clone(), inner_sink)
                    .expect("watch the inner checkout");
                let first = hub
                    .register(outer.clone(), outer_sink)
                    .expect("watch the outer checkout");
                (first, second)
            };

            drop(inner_registration);
            assert!(
                hears_a_write(&inner, &mut outer_events),
                "the outer root must still observe the released root's tree \
                 (outer registered first: {outer_first})"
            );
            drop(outer_registration);
        }
    }

    /// Two checkouts that depend on the same global rule file share one watch,
    /// and the watch outlives the first of them.
    #[test]
    fn an_external_watch_is_shared_and_released_with_its_last_holder() {
        let shared = tempfile::tempdir().expect("temporary rule directory");
        let rule = shared.path().join("ignore");
        let first_root = tempfile::tempdir().expect("temporary checkout");
        let second_root = tempfile::tempdir().expect("temporary checkout");
        let hub = Arc::new(WatchHub::new());
        let (first_sink, _first) = sink(BATCH_CAPACITY);
        let (second_sink, _second) = sink(BATCH_CAPACITY);
        let first = hub
            .register(first_root.path().to_path_buf(), first_sink)
            .expect("watch the first checkout");
        let second = hub
            .register(second_root.path().to_path_buf(), second_sink)
            .expect("watch the second checkout");

        first.watch_external(&rule);
        second.watch_external(&rule);
        // A repeat from the same holder must not raise the count.
        second.watch_external(&rule);
        assert_eq!(
            super::lock(&hub.watcher)
                .as_ref()
                .map(|state| state.externals.len()),
            Some(1)
        );

        drop(first);
        assert_eq!(
            super::lock(&hub.watcher)
                .as_ref()
                .and_then(|state| state.externals.get(shared.path()).copied()),
            Some(1),
            "the remaining holder keeps the shared watch"
        );

        drop(second);
        assert!(!hub.has_watcher());
    }
}

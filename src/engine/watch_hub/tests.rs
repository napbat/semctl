use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use notify::EventKind;
use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
use tokio::sync::mpsc;

use super::{
    BATCH_CAPACITY, Event, HashSet, Path, PathBuf, Route, Routes, WatchBatch, WatchHub, WatchSink,
    dispatch,
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

/// Checkout A subscribes to a rule directory inside checkout B. B's
/// recursive watch covers it, so A's subscription is routed without a
/// platform watch of its own. Releasing B must then install the external
/// watch A still needs, or the policy file goes silent until a periodic
/// re-sync or session recreation.
#[test]
fn an_external_watch_survives_the_release_of_the_root_that_covered_it() {
    let a_dir = tempfile::tempdir().expect("temporary checkout");
    let b_dir = tempfile::tempdir().expect("temporary checkout");
    let b = std::fs::canonicalize(b_dir.path()).expect("canonical covering root");
    let rules = b.join("policy");
    std::fs::create_dir(&rules).expect("create the rule directory");
    let rule = rules.join("ignore");

    let hub = Arc::new(WatchHub::new());
    let (a_sink, mut a_events) = sink(BATCH_CAPACITY);
    let (b_sink, _b_events) = sink(BATCH_CAPACITY);
    let a_registration = hub
        .register(a_dir.path().to_path_buf(), a_sink)
        .expect("watch checkout A");
    let b_registration = hub.register(b.clone(), b_sink).expect("watch checkout B");

    a_registration.watch_external(&rule);
    assert_eq!(
        super::lock(&hub.watcher)
            .as_ref()
            .map(|state| state.externals.len()),
        Some(0),
        "B's recursive watch covers the rule directory, so no external watch exists yet"
    );

    drop(b_registration);
    assert_eq!(
        super::lock(&hub.watcher)
            .as_ref()
            .and_then(|state| state.externals.get(&rules).copied()),
        Some(1),
        "releasing B must install the external watch A still needs"
    );

    std::fs::write(&rule, b"target\n").expect("write the rule file");
    let paths = await_paths(&mut a_events);
    assert!(
        paths.iter().any(|path| path == &rule),
        "checkout A must hear the rule change after B is released: {paths:?}"
    );
    drop(a_registration);
    assert!(!hub.has_watcher(), "the last registration releases the hub");
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

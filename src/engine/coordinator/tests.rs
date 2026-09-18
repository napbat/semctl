//! Trigger coalescing, first-index handling, and event relevance.
//!
//! The coordinator takes its reconcile through [`Reconciler`], so these tests
//! count runs and hold a run open without a server and without a checkout.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use notify::EventKind;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use tokio::sync::{Semaphore, mpsc};

use super::{
    CheckoutCoordinator, CoordinatorSetup, CoordinatorTask, Event, HashSet, PathBuf, Reconciled,
    Reconciler, SourcePolicy, SyncLimits, SyncOutcome, TRIGGER_CAPACITY, Trigger, blocking,
    initial_job_result, is_interesting, jitter, relevance,
};
use crate::client::{Client, api};
use crate::engine::registry::CheckoutKey;
use crate::mcp::readiness::InitialIndexGate;
use crate::sync::policy;

/// Somewhere no test writes, so a coordinator that starts reading the
/// filesystem fails instead of touching a real checkout.
const TEST_ROOT: &str = "/semctl-test-checkout";

/// A reconcile that reports when it starts and finishes only when the test
/// lets it.
struct CountingReconciler {
    runs: AtomicUsize,
    /// How many embedding-job polls this reconciler was asked for.
    polls: AtomicUsize,
    started: mpsc::UnboundedSender<()>,
    /// Each run takes one permit. A test that adds none holds the run open.
    release: Arc<Semaphore>,
    /// Each job poll takes one permit, so a test can hold a first index open.
    finish_polls: Arc<Semaphore>,
}

impl CountingReconciler {
    fn new() -> (Arc<Self>, Runs) {
        let (started, starts) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let finish_polls = Arc::new(Semaphore::new(0));
        let reconciler = Arc::new(Self {
            runs: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            started,
            release: release.clone(),
            finish_polls: finish_polls.clone(),
        });
        (
            reconciler.clone(),
            Runs {
                starts,
                release,
                finish_polls,
            },
        )
    }

    fn count(&self) -> usize {
        self.runs.load(Ordering::Acquire)
    }

    fn polls(&self) -> usize {
        self.polls.load(Ordering::Acquire)
    }
}

/// The test's side of [`CountingReconciler`].
struct Runs {
    starts: mpsc::UnboundedReceiver<()>,
    release: Arc<Semaphore>,
    finish_polls: Arc<Semaphore>,
}

impl Runs {
    /// Let `count` runs finish.
    fn release(&self, count: usize) {
        self.release.add_permits(count);
    }

    /// Let `count` embedding-job polls finish.
    fn release_polls(&self, count: usize) {
        self.finish_polls.add_permits(count);
    }

    /// Wait for the next run to start.
    async fn next_start(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.starts.recv())
            .await
            .expect("a reconcile must start")
            .expect("the coordinator must stay alive");
    }

    /// Fail if another run starts within a short window.
    async fn no_further_start(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(80), self.starts.recv())
                .await
                .is_err(),
            "no further reconcile may run"
        );
    }
}

impl Reconciler for CountingReconciler {
    fn reconcile(&self, _run: super::ReconcileRun) -> Reconciled {
        self.runs.fetch_add(1, Ordering::AcqRel);
        let _ = self.started.send(());
        let release = self.release.clone();
        Box::pin(async move {
            let permit = release.acquire().await;
            // Only a closed semaphore fails, and no test closes one.
            if let Ok(permit) = permit {
                permit.forget();
            }
            Ok(SyncOutcome {
                codebase_id: "codebase".to_string(),
                job_id: "job".to_string(),
                uploaded: 0,
                to_delete: 0,
            })
        })
    }

    fn await_job(&self, _client: Client, _job_id: String) -> super::JobAwaited {
        self.polls.fetch_add(1, Ordering::AcqRel);
        let finish = self.finish_polls.clone();
        Box::pin(async move {
            // Only a closed semaphore fails, and no test closes one.
            if let Ok(permit) = finish.acquire().await {
                permit.forget();
            }
            Ok(())
        })
    }
}

/// A coordinator with no watcher, no periodic timer, and a counting reconcile.
/// The task is returned unstarted so a test can queue triggers first.
async fn coordinator(
    reconciler: Arc<dyn Reconciler>,
) -> (Arc<CheckoutCoordinator>, CoordinatorTask) {
    // A unit test must not depend on a timer.
    build(reconciler, None, Some(0)).await
}

/// [`coordinator`], with the first-index gate and the re-sync interval a test
/// needs.
async fn build(
    reconciler: Arc<dyn Reconciler>,
    gate: Option<Arc<InitialIndexGate>>,
    resync_secs: Option<u64>,
) -> (Arc<CheckoutCoordinator>, CoordinatorTask) {
    let client = Client::for_test("codebase", None);
    let (_sender, batches) = mpsc::channel(4);
    CheckoutCoordinator::new(CoordinatorSetup {
        key: CheckoutKey::for_client(&client, PathBuf::from(TEST_ROOT)).await,
        client,
        resync_secs,
        watch: Err("no watcher in tests".to_string()),
        batches,
        overflow: Arc::new(AtomicBool::new(false)),
        gate,
        reconciler,
        scan_permits: Arc::new(Semaphore::new(2)),
        limits: SyncLimits::new(Arc::new(Semaphore::new(2))),
    })
}

/// A burst is what a host produces when several tool calls resolve the same
/// checkout at once. It must cost one reconcile, not ten.
#[tokio::test]
async fn a_burst_of_triggers_becomes_one_reconcile() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let (coordinator, task) = coordinator(reconciler.clone()).await;
    for _ in 0..10 {
        coordinator.trigger(Trigger::Explicit);
    }

    task.spawn();
    runs.release(10);
    runs.next_start().await;
    runs.no_further_start().await;

    assert_eq!(reconciler.count(), 1);
}

/// A trigger that arrives while a reconcile runs cannot be folded into it: the
/// scan already read the tree. It must produce exactly one follow-up.
#[tokio::test]
async fn a_trigger_during_a_run_produces_exactly_one_follow_up() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let (coordinator, task) = coordinator(reconciler.clone()).await;
    task.spawn();

    coordinator.trigger(Trigger::Explicit);
    runs.next_start().await;
    for _ in 0..4 {
        coordinator.trigger(Trigger::Explicit);
    }

    runs.release(2);
    runs.next_start().await;
    runs.no_further_start().await;

    assert_eq!(reconciler.count(), 2);
}

/// An explicit sync after the startup index is a second reconcile, not a
/// coalesced one: the startup run was already in flight.
#[tokio::test]
async fn an_explicit_trigger_after_startup_runs_twice() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let (coordinator, task) = coordinator(reconciler.clone()).await;
    task.spawn();

    coordinator.trigger(Trigger::Startup);
    runs.next_start().await;
    runs.release(1);
    coordinator.trigger(Trigger::Explicit);
    runs.next_start().await;
    runs.release(1);
    runs.no_further_start().await;

    assert_eq!(reconciler.count(), 2);
}

/// A trigger must never be lost quietly. When the queue is full, the overflow
/// flag carries the fact that something was dropped, and a run still happens.
#[tokio::test]
async fn a_full_trigger_queue_sets_overflow_and_still_runs() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let (coordinator, task) = coordinator(reconciler.clone()).await;
    for _ in 0..=TRIGGER_CAPACITY {
        coordinator.trigger(Trigger::Explicit);
    }
    assert!(
        coordinator.status().await.trigger_overflow,
        "a dropped trigger must be recorded"
    );

    task.spawn();
    runs.release(2);
    runs.next_start().await;
    runs.no_further_start().await;

    assert_eq!(reconciler.count(), 1);
    assert!(
        !coordinator.status().await.trigger_overflow,
        "the run must clear the overflow it acted on"
    );
}

/// Status is what `sync_status` and the daemon report from.
#[tokio::test]
async fn status_reports_the_last_job_and_the_watcher_state() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let (coordinator, task) = coordinator(reconciler).await;
    task.spawn();
    coordinator.trigger(Trigger::Startup);
    runs.release(1);
    runs.next_start().await;

    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = coordinator.status().await;
            if status.last_job_id.is_some() {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a startup run always records its job");

    assert_eq!(status.last_job_id.as_deref(), Some("job"));
    assert_eq!(status.codebase_id.as_deref(), Some("codebase"));
    assert_eq!(
        status.watcher,
        super::WatcherState::Unavailable("no watcher in tests".to_string())
    );
    assert!(!status.running);
    assert!(status.last_error.is_none());
    assert!(status.to_string().contains("job job"));
}

/// A second reconcile during embedding sees the same pending gate. It must not
/// start a second poll: the poll that finished last would otherwise decide the
/// result for every session, and a succeeded first index could turn failed.
#[tokio::test]
async fn a_second_run_does_not_poll_a_gate_that_is_already_polled() {
    let (reconciler, mut runs) = CountingReconciler::new();
    let gate = Arc::new(InitialIndexGate::pending());
    let (coordinator, task) = build(reconciler.clone(), Some(gate.clone()), Some(0)).await;
    gate.register_codebase("codebase".to_string()).await;
    task.spawn();

    coordinator.trigger(Trigger::Startup);
    runs.release(1);
    runs.next_start().await;
    // The second run starts after the first finished, so the first run's claim
    // on the gate is already taken when the second reports its job.
    coordinator.trigger(Trigger::Explicit);
    runs.release(1);
    runs.next_start().await;
    runs.no_further_start().await;

    assert_eq!(reconciler.count(), 2, "both runs must reconcile");
    assert_eq!(reconciler.polls(), 1, "one gate is polled once");

    runs.release_polls(1);
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(outcome) = gate.outcome().await {
                return outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the one poll reports the first index");
    assert_eq!(outcome, Ok(()));
}

/// A checkout is re-indexed into a new codebase when the server deleted the
/// old one. The gate carries the new id, and reconciling into the stored one
/// would upload into a codebase that no longer exists.
#[tokio::test]
async fn the_gates_codebase_replaces_the_one_the_coordinator_holds() {
    let (reconciler, _runs) = CountingReconciler::new();
    let gate = Arc::new(InitialIndexGate::pending());
    let (coordinator, _task) = build(reconciler, Some(gate.clone()), Some(0)).await;
    assert_eq!(coordinator.codebase_id().await.as_deref(), Some("codebase"));

    gate.register_codebase("re-indexed".to_string()).await;

    assert!(coordinator.await_codebase(Some(&gate)).await);
    assert_eq!(
        coordinator.codebase_id().await.as_deref(),
        Some("re-indexed")
    );
}

/// A gate that ended without naming a codebase leaves the stored one in place,
/// so a later explicit sync still has somewhere to reconcile into.
#[tokio::test]
async fn a_gate_without_a_codebase_keeps_the_stored_one() {
    let (reconciler, _runs) = CountingReconciler::new();
    let gate = Arc::new(InitialIndexGate::pending());
    let (coordinator, _task) = build(reconciler, Some(gate.clone()), Some(0)).await;
    gate.finish(Err("embedding failed".to_string())).await;

    assert!(coordinator.await_codebase(Some(&gate)).await);
    assert_eq!(coordinator.codebase_id().await.as_deref(), Some("codebase"));
}

/// The interval reaches this timer from a session, so it is external data. An
/// interval that cannot name an instant must disable the backstop rather than
/// end the reconcile loop.
#[tokio::test]
async fn an_impossible_resync_interval_disables_the_periodic_timer() {
    let (reconciler, _runs) = CountingReconciler::new();
    let (coordinator, _task) = build(reconciler, None, Some(u64::MAX)).await;

    assert!(coordinator.periodic_timer().is_none());
}

/// The offset spreads coordinators without a random-number source, and one
/// checkout always picks the same offset.
#[test]
fn the_periodic_offset_is_stable_and_inside_one_interval() {
    let interval = Duration::from_secs(300);
    let first = jitter(std::path::Path::new("/work/first"), interval);
    let second = jitter(std::path::Path::new("/work/second"), interval);

    assert!(first < interval);
    assert!(second < interval);
    assert_ne!(first, second, "two checkouts must not tick together");
    assert_eq!(first, jitter(std::path::Path::new("/work/first"), interval));
}

fn test_policy(root: &std::path::Path) -> SourcePolicy {
    SourcePolicy::load(root, &blocking::Cancellation::default()).expect("load the source policy")
}

fn event(kind: EventKind) -> Event {
    Event::new(kind).add_path(PathBuf::from("src/lib.rs"))
}

#[test]
fn scan_access_does_not_schedule_another_sync() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let mut policy = test_policy(temp.path());

    for kind in [
        EventKind::Access(notify::event::AccessKind::Read),
        EventKind::Access(notify::event::AccessKind::Open(
            notify::event::AccessMode::Read,
        )),
        EventKind::Access(notify::event::AccessKind::Open(
            notify::event::AccessMode::Write,
        )),
        EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Read,
        )),
    ] {
        assert!(
            !is_interesting(&event(kind), &mut policy),
            "accepted {kind:?}"
        );
    }
}

#[test]
fn mutations_still_schedule_a_sync() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let mut policy = test_policy(temp.path());

    for kind in [
        EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Write,
        )),
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
    let temp = tempfile::tempdir().expect("temporary checkout");
    let mut policy = test_policy(temp.path());
    let event =
        Event::new(EventKind::Modify(ModifyKind::Any)).add_path(PathBuf::from(".git/index"));

    assert!(!is_interesting(&event, &mut policy));
}

#[test]
fn both_project_ignore_files_filter_watcher_events() {
    for ignore_name in crate::sync::walker::IGNORE_FILES {
        let temp = tempfile::tempdir().expect("temporary checkout");
        std::fs::write(temp.path().join(ignore_name), "private.txt\n").expect("write ignore rules");
        let mut policy = test_policy(temp.path());
        let event = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(temp.path().join("private.txt"));
        assert!(!is_interesting(&event, &mut policy));
    }
}

#[test]
fn ignore_file_changes_always_schedule_a_scan() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    std::fs::write(
        temp.path().join(".gitignore"),
        ".semctxignore\n.semctlignore\n",
    )
    .expect("write ignore rules");
    let mut policy = test_policy(temp.path());
    for name in crate::sync::walker::IGNORE_FILES {
        let event = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(temp.path().join(name));
        assert!(is_interesting(&event, &mut policy));
    }
}

/// The batch check must apply the same rules the manifest applies, and it must
/// remember the rule files it read so the next batch can be filtered cheaply.
#[tokio::test]
async fn the_relevance_check_applies_the_source_policy() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let root = std::fs::canonicalize(temp.path()).expect("canonical checkout");
    std::fs::write(root.join(".gitignore"), "ignored.txt\n").expect("write ignore rules");

    let (relevant, observed) = relevance(
        &root,
        vec![Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("ignored.txt"))],
        HashSet::new(),
    )
    .await;
    assert!(!relevant, "an ignored file must not schedule a reconcile");
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
    assert!(relevant, "a source edit must schedule a reconcile");
}

/// A private scratch file must not even cost a policy load.
#[tokio::test]
async fn private_scratch_events_are_never_relevant() {
    let temp = tempfile::tempdir().expect("temporary checkout");
    let root = std::fs::canonicalize(temp.path()).expect("canonical checkout");
    let scratch = root.join(".semctl-0123456789ab-0.edit");
    assert!(policy::is_private_path(&scratch));

    let (relevant, _) = relevance(
        &root,
        vec![Event::new(EventKind::Create(CreateKind::File)).add_path(scratch)],
        HashSet::new(),
    )
    .await;

    assert!(!relevant);
}

fn job(completed: bool, failed: i64, error: Option<&str>) -> api::JobStatus {
    api::JobStatus {
        files_to_embed: 3,
        files_to_delete: 0,
        files_embedded: if completed { 3 - failed } else { 1 },
        files_deleted: 0,
        files_failed: failed,
        chunk_count: completed.then_some(12),
        error: error.map(str::to_string),
        started_at: Some("2026-07-31T00:00:00Z".into()),
        completed_at: completed.then(|| "2026-07-31T00:00:01Z".into()),
    }
}

/// Retrieval must never run against a partial first index.
#[test]
fn first_index_requires_terminal_success() {
    assert!(initial_job_result("j", &job(false, 0, None)).is_none());
    assert_eq!(initial_job_result("j", &job(true, 0, None)), Some(Ok(())));
    assert!(
        initial_job_result("j", &job(true, 1, None))
            .expect("a completed job is terminal")
            .expect_err("failed files must fail the gate")
            .contains("1 failed file")
    );
    assert!(
        initial_job_result("j", &job(true, 0, Some("worker died")))
            .expect("an errored job is terminal")
            .expect_err("a job error must fail the gate")
            .contains("worker died")
    );
}

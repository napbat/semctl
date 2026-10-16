//! Paused-time checks of automatic failure retries and immediate requests.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};

use crate::client::Client;
use crate::engine::coordinator::{
    CheckoutCoordinator, CoordinatorSetup, JobAwaited, ReconcileRun, Reconciled, Reconciler,
    TRIGGER_CAPACITY, Trigger,
};
use crate::engine::registry::CheckoutKey;
use crate::mcp::readiness::InitialIndexGate;
use crate::sync::{SyncLimits, SyncOutcome, UploadFailure};

#[derive(Clone, Copy)]
enum Outcome {
    Complete,
    Failure,
    Partial,
}

struct ScriptedReconciler {
    outcomes: Mutex<VecDeque<Outcome>>,
    starts: mpsc::UnboundedSender<()>,
    count: AtomicUsize,
}

impl Reconciler for ScriptedReconciler {
    fn reconcile(&self, _run: ReconcileRun) -> Reconciled {
        self.count.fetch_add(1, Ordering::AcqRel);
        let _ = self.starts.send(());
        let outcome = self
            .outcomes
            .lock()
            .expect("test outcome lock must not be poisoned")
            .pop_front()
            .expect("each reconcile must have a scripted outcome");
        Box::pin(async move {
            if matches!(outcome, Outcome::Failure) {
                return Err("server unavailable".to_string());
            }
            Ok(SyncOutcome {
                codebase_id: "codebase".to_string(),
                job_id: "job".to_string(),
                uploaded: 0,
                to_delete: 0,
                failed: if matches!(outcome, Outcome::Partial) {
                    vec![UploadFailure {
                        path: "src/lib.rs".to_string(),
                        reason: "server unavailable".to_string(),
                    }]
                } else {
                    Vec::new()
                },
            })
        })
    }

    fn await_job(&self, _client: Client, _job_id: String) -> JobAwaited {
        Box::pin(async { Ok(()) })
    }
}

struct Fixture {
    coordinator: Arc<CheckoutCoordinator>,
    reconciler: Arc<ScriptedReconciler>,
    starts: mpsc::UnboundedReceiver<()>,
    scan_permits: Arc<Semaphore>,
}

impl Fixture {
    async fn new(outcomes: impl IntoIterator<Item = Outcome>) -> Self {
        Self::with_gate(outcomes, None).await
    }

    async fn with_gate(
        outcomes: impl IntoIterator<Item = Outcome>,
        gate: Option<Arc<InitialIndexGate>>,
    ) -> Self {
        let (started, starts) = mpsc::unbounded_channel();
        let reconciler = Arc::new(ScriptedReconciler {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            starts: started,
            count: AtomicUsize::new(0),
        });
        let client = Client::for_test("codebase", None);
        let (_sender, batches) = mpsc::channel(4);
        let scan_permits = Arc::new(Semaphore::new(2));
        let (coordinator, task) = CheckoutCoordinator::new(CoordinatorSetup {
            key: CheckoutKey::for_client(&client, PathBuf::from("/semctl-retry-test-checkout"))
                .await,
            client,
            resync_secs: Some(0),
            watch: Err("no watcher in tests".to_string()),
            batches,
            overflow: Arc::new(AtomicBool::new(false)),
            gate,
            reconciler: reconciler.clone(),
            scan_permits: scan_permits.clone(),
            limits: SyncLimits::new(Arc::new(Semaphore::new(2))),
        });
        task.spawn();
        Self {
            coordinator,
            reconciler,
            starts,
            scan_permits,
        }
    }

    async fn assert_start(&mut self) {
        tokio::task::yield_now().await;
        self.starts.try_recv().expect("a reconcile must start now");
    }

    async fn assert_no_start(&mut self) {
        tokio::task::yield_now().await;
        assert!(
            self.starts.try_recv().is_err(),
            "no reconcile may start yet"
        );
    }

    async fn assert_retry(&self, seconds: Option<u64>) {
        tokio::task::yield_now().await;
        assert_eq!(self.coordinator.status().await.retry_after_secs, seconds);
    }
}

#[tokio::test(start_paused = true)]
async fn automatic_failures_coalesce_bursts_and_retry_with_bounded_backoff() {
    let mut fixture = Fixture::new(std::iter::repeat_n(Outcome::Failure, 7)).await;
    fixture.coordinator.trigger(Trigger::Startup);
    fixture.assert_start().await;

    for delay in [5, 10, 20, 40, 60, 60] {
        fixture.assert_retry(Some(delay)).await;
        for _ in 0..100 {
            fixture.coordinator.trigger(Trigger::Watch);
        }
        fixture.assert_no_start().await;
        tokio::time::advance(Duration::from_secs(delay - 1)).await;
        fixture.assert_no_start().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        fixture.assert_start().await;
    }

    assert_eq!(fixture.reconciler.count.load(Ordering::Acquire), 7);
}

#[tokio::test(start_paused = true)]
async fn a_failed_startup_retries_without_a_periodic_timer_or_new_events() {
    let mut fixture = Fixture::new([Outcome::Failure, Outcome::Complete]).await;
    fixture.coordinator.trigger(Trigger::Startup);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;

    tokio::time::advance(Duration::from_secs(4)).await;
    fixture.assert_no_start().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    fixture.assert_start().await;
    fixture.assert_retry(None).await;
}

#[tokio::test(start_paused = true)]
async fn an_explicit_request_bypasses_cooldown_even_when_the_trigger_queue_is_full() {
    let mut fixture = Fixture::new([Outcome::Failure, Outcome::Failure]).await;
    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;
    for _ in 0..TRIGGER_CAPACITY {
        fixture.coordinator.trigger(Trigger::Watch);
    }
    fixture.coordinator.trigger(Trigger::Explicit);

    fixture.assert_start().await;
    fixture.assert_retry(Some(10)).await;
    fixture.assert_no_start().await;
}

#[tokio::test(start_paused = true)]
async fn a_complete_retry_resets_the_delay_for_the_next_failure() {
    let mut fixture = Fixture::new([
        Outcome::Failure,
        Outcome::Failure,
        Outcome::Complete,
        Outcome::Failure,
        Outcome::Complete,
    ])
    .await;
    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    fixture.assert_start().await;
    fixture.assert_retry(Some(10)).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    fixture.assert_start().await;
    fixture.assert_retry(None).await;
    assert!(fixture.coordinator.status().await.last_error.is_none());
    tokio::time::advance(Duration::from_secs(60)).await;
    fixture.assert_no_start().await;

    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    fixture.assert_start().await;
    fixture.assert_retry(None).await;
}

#[tokio::test(start_paused = true)]
async fn a_partial_upload_retains_work_and_retries_after_the_cooldown() {
    let mut fixture = Fixture::new([Outcome::Partial, Outcome::Complete]).await;
    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;
    let status = fixture.coordinator.status().await;
    assert!(status.last_error.is_some());
    assert!(status.to_string().contains("retry in 5s"));
    let encoded = serde_json::to_value(&status).expect("encode retry status");
    assert_eq!(encoded["retry_after_secs"], 5);

    tokio::time::advance(Duration::from_secs(5)).await;
    fixture.assert_start().await;
    fixture.assert_retry(None).await;
    assert!(fixture.coordinator.status().await.last_error.is_none());
}

#[tokio::test(start_paused = true)]
async fn renewing_a_failed_first_index_bypasses_the_existing_cooldown() {
    let gate = Arc::new(InitialIndexGate::pending());
    gate.register_codebase("codebase".to_string()).await;
    let mut fixture =
        Fixture::with_gate([Outcome::Failure, Outcome::Complete], Some(gate.clone())).await;
    fixture.coordinator.trigger(Trigger::Startup);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;
    tokio::task::yield_now().await;
    assert!(matches!(gate.outcome().await, Some(Err(_))));

    let renewed = fixture.coordinator.renew_first_index_gate().await;
    renewed.register_codebase("codebase".to_string()).await;
    fixture.assert_start().await;
    fixture.assert_retry(None).await;
    assert!(!Arc::ptr_eq(&gate, &renewed));
}

#[tokio::test(start_paused = true)]
async fn a_cooldown_holds_no_scan_permit_or_strong_coordinator_handle() {
    let mut fixture = Fixture::new([Outcome::Failure]).await;
    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;
    assert_eq!(fixture.scan_permits.available_permits(), 2);
    let weak = Arc::downgrade(&fixture.coordinator);

    drop(fixture.coordinator);
    tokio::task::yield_now().await;

    assert!(weak.upgrade().is_none());
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(fixture.starts.try_recv().is_err());
    assert_eq!(fixture.reconciler.count.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_a_cooldown_stops_the_retry() {
    let mut fixture = Fixture::new([Outcome::Failure]).await;
    fixture.coordinator.trigger(Trigger::Watch);
    fixture.assert_start().await;
    fixture.assert_retry(Some(5)).await;

    fixture.coordinator.cancel();
    tokio::time::advance(Duration::from_secs(60)).await;

    fixture.assert_no_start().await;
    assert_eq!(fixture.reconciler.count.load(Ordering::Acquire), 1);
}

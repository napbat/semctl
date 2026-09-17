//! One checkout's reconcile loop.
//!
//! A coordinator owns everything about keeping one working copy in sync: its
//! content cache, its watch registration, its last job, its first-index gate,
//! and one task that runs reconciles. Sessions do not own any of that. They
//! hold a lease, and the coordinator behaves the same whether one session or a
//! thousand are attached.
//!
//! Every trigger goes through one bounded channel into that one task. The task
//! drains the channel before each run, so a burst of triggers becomes one
//! reconcile, and a trigger that arrives during a run becomes exactly one
//! follow-up reconcile.
//!
//! Ownership and cancellation:
//!
//! - The task holds a [`std::sync::Weak`] handle to the coordinator. When the
//!   registry drops the last strong handle, the trigger senders go with it, the
//!   task's next receive ends, and the task returns. The task can therefore
//!   never keep its own coordinator alive.
//! - [`CheckoutCoordinator::cancel`] is the explicit path, in this order: abort
//!   the task, then release the watch registration, then let the last handle
//!   drop the client. A reconcile that is already running finishes its current
//!   filesystem operation and then observes cancellation through
//!   [`crate::sync::blocking`], which keeps every lock the operation holds
//!   until it exits.
//!
//! Lock order inside a coordinator is: watch state, gate, codebase id, last
//! job, run state. No call holds two of them across an await except
//! [`CheckoutCoordinator::renew_first_index_gate`], which holds the gate slot
//! while it reads that gate's own state.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Interval, MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use super::registry::CheckoutKey;
use super::scheduler;
use super::watch_hub::{WatchBatch, WatchRegistration};
use crate::client::{self, Client};
use crate::mcp::readiness::InitialIndexGate;
use crate::sync::policy::{SourcePolicy, event_may_affect_policy};
use crate::sync::{self, LastJob, SyncCache, SyncLimits, SyncOutcome, blocking};

/// Reasons a reconcile can be due, before any coalescing.
const TRIGGER_CAPACITY: usize = 64;

/// Interval for the periodic re-sync when no watcher is available. A session
/// overrides it with `SEMCTX_MCP_RESYNC_SECS`, which reaches the coordinator as
/// `resync_secs`; `0` disables the timer and leaves only the startup run and
/// explicit triggers.
const DEFAULT_RESYNC_SECS: u64 = 60;

/// An active watcher reports real edits within the debounce window, so the
/// periodic re-sync is only a backstop against events the platform dropped.
/// Running it five times less often is what makes 1,000 watched checkouts
/// affordable.
const WATCHED_RESYNC_MULTIPLIER: u64 = 5;

/// How often the first-index gate polls the server's embedding job.
const JOB_POLL_EVERY: Duration = Duration::from_millis(500);

/// Why a reconcile is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// The coordinator was just created.
    Startup,
    /// The watcher reported a relevant change.
    Watch,
    /// The periodic backstop fired.
    Periodic,
    /// A tool asked for a sync.
    Explicit,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Watch => "watch",
            Self::Periodic => "periodic",
            Self::Explicit => "explicit",
        }
    }
}

/// One reconcile request, with everything the reconcile owns.
pub(crate) struct ReconcileRun {
    pub(crate) client: Client,
    pub(crate) root: PathBuf,
    pub(crate) cache: Arc<Mutex<SyncCache>>,
    pub(crate) limits: SyncLimits,
}

/// A reconcile in progress. Boxed and `'static`, so an implementation must own
/// everything it needs instead of borrowing itself.
pub(crate) type Reconciled = Pin<Box<dyn Future<Output = Result<SyncOutcome, String>> + Send>>;
/// A job poll in progress.
pub(crate) type JobAwaited = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// How a coordinator does its work.
///
/// Production uses [`SyncReconciler`]. Tests use a counting fake, so the
/// trigger, coalescing, and cancellation behavior can be checked without a
/// server and without a checkout.
pub(crate) trait Reconciler: Send + Sync + 'static {
    /// Walk the checkout, diff it against the server, and upload what changed.
    fn reconcile(&self, run: ReconcileRun) -> Reconciled;

    /// Wait for the server to finish embedding one job. Only the first-index
    /// gate needs this: retrieval must not run against a partial first index.
    fn await_job(&self, client: Client, job_id: String) -> JobAwaited;
}

/// The production reconciler: the shared sync engine and the job poll.
pub(crate) struct SyncReconciler;

impl Reconciler for SyncReconciler {
    fn reconcile(&self, run: ReconcileRun) -> Reconciled {
        Box::pin(async move {
            sync::sync(&run.client, &run.root, &run.cache, &run.limits)
                .await
                .map_err(|error| format!("{error:#}"))
        })
    }

    fn await_job(&self, client: Client, job_id: String) -> JobAwaited {
        Box::pin(async move { wait_for_initial_job(&client, &job_id).await })
    }
}

/// A reconcile that never finishes.
///
/// Tests that check ownership rather than reconciling use it: a coordinator
/// built with it keeps exactly the state the test gave it, and it needs neither
/// a server nor a checkout on disk.
#[cfg(test)]
pub(crate) struct IdleReconciler;

#[cfg(test)]
impl Reconciler for IdleReconciler {
    fn reconcile(&self, _run: ReconcileRun) -> Reconciled {
        Box::pin(std::future::pending())
    }

    fn await_job(&self, _client: Client, _job_id: String) -> JobAwaited {
        Box::pin(std::future::pending())
    }
}

/// Whether this checkout has a realtime watcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WatcherState {
    Active,
    /// No watcher, and why. The periodic re-sync runs at its short interval.
    Unavailable(String),
}

/// The watch registration, or the reason there is none.
enum Watch {
    Active(WatchRegistration),
    Unavailable(String),
}

/// What the last reconcile did.
#[derive(Default)]
struct RunState {
    running: bool,
    last_outcome: Option<String>,
    last_error: Option<String>,
}

/// One checkout, as `sync_status` and the daemon status see it.
#[derive(Clone, Debug)]
pub(crate) struct CoordinatorStatus {
    pub(crate) root: PathBuf,
    pub(crate) codebase_id: Option<String>,
    pub(crate) leases: usize,
    pub(crate) watcher: WatcherState,
    pub(crate) last_job_id: Option<String>,
    pub(crate) running: bool,
    pub(crate) pending_triggers: usize,
    pub(crate) trigger_overflow: bool,
    pub(crate) last_outcome: Option<String>,
    pub(crate) last_error: Option<String>,
}

impl std::fmt::Display for CoordinatorStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "root {} codebase {} leases {} watcher {} job {} running {} pending {}{} outcome {} error {}",
            self.root.display(),
            self.codebase_id.as_deref().unwrap_or("(unbound)"),
            self.leases,
            match &self.watcher {
                WatcherState::Active => "active",
                WatcherState::Unavailable(reason) => reason,
            },
            self.last_job_id.as_deref().unwrap_or("(none)"),
            self.running,
            self.pending_triggers,
            if self.trigger_overflow {
                " (overflowed)"
            } else {
                ""
            },
            self.last_outcome.as_deref().unwrap_or("(none)"),
            self.last_error.as_deref().unwrap_or("(none)"),
        )
    }
}

/// Everything one coordinator needs to exist. The registry fills it; the watch
/// registration is already decided, because registering is blocking work that
/// must not happen under the registry's lock.
pub(crate) struct CoordinatorSetup {
    pub(crate) key: CheckoutKey,
    pub(crate) client: Client,
    pub(crate) resync_secs: Option<u64>,
    pub(crate) watch: Result<WatchRegistration, String>,
    pub(crate) batches: mpsc::Receiver<WatchBatch>,
    pub(crate) overflow: Arc<AtomicBool>,
    pub(crate) gate: Option<Arc<InitialIndexGate>>,
    pub(crate) reconciler: Arc<dyn Reconciler>,
    pub(crate) scan_permits: Arc<Semaphore>,
    pub(crate) limits: SyncLimits,
}

/// One checkout's owner.
pub(crate) struct CheckoutCoordinator {
    key: CheckoutKey,
    root: PathBuf,
    /// The attached client. Its codebase selection is applied per run from
    /// `codebase_id`, which the server can move.
    client: Client,
    /// The codebase this checkout is filed under. It starts as whatever the
    /// attached client named, and every successful reconcile confirms it: the
    /// server can move a checkout to another codebase.
    codebase_id: Mutex<Option<String>>,
    cache: Arc<Mutex<SyncCache>>,
    last_job: Mutex<Option<LastJob>>,
    gate: Mutex<Option<Arc<InitialIndexGate>>>,
    watch: StdMutex<Watch>,
    triggers: mpsc::Sender<Trigger>,
    /// Set when a trigger or a watch batch was dropped. The next run is then
    /// unconditional, because what was dropped cannot be judged.
    overflow: Arc<AtomicBool>,
    run: Mutex<RunState>,
    leases: AtomicUsize,
    /// When the lease count last reached zero, for the registry's sweeper.
    released_at: StdMutex<Option<Instant>>,
    task: StdMutex<Option<JoinHandle<()>>>,
    /// The detached poll that finishes a first-index gate.
    gate_task: StdMutex<Option<JoinHandle<()>>>,
    resync_secs: Option<u64>,
}

impl CheckoutCoordinator {
    /// Build a coordinator and the task that will serve it.
    ///
    /// The task is returned unstarted so the caller can queue triggers first.
    /// That also keeps creation free of "half-started" states: either the
    /// caller publishes the coordinator and starts the task, or it drops both.
    pub(crate) fn new(setup: CoordinatorSetup) -> (Arc<Self>, CoordinatorTask) {
        let (triggers, trigger_receiver) = mpsc::channel(TRIGGER_CAPACITY);
        let root = setup.key.root().to_path_buf();
        let watch = match setup.watch {
            Ok(registration) => Watch::Active(registration),
            Err(reason) => {
                warn!(root = %root.display(), %reason, "no realtime watch; using the short periodic re-sync");
                Watch::Unavailable(reason)
            }
        };
        let coordinator = Arc::new(Self {
            key: setup.key,
            root,
            // The codebase the attached client already names. A first index
            // has none yet: its caller registers one through the gate.
            codebase_id: Mutex::new(setup.client.codebase_raw().map(str::to_string)),
            client: setup.client,
            cache: Arc::new(Mutex::new(SyncCache::default())),
            last_job: Mutex::new(None),
            gate: Mutex::new(setup.gate),
            watch: StdMutex::new(watch),
            triggers,
            overflow: setup.overflow,
            run: Mutex::new(RunState::default()),
            leases: AtomicUsize::new(0),
            released_at: StdMutex::new(None),
            task: StdMutex::new(None),
            gate_task: StdMutex::new(None),
            resync_secs: setup.resync_secs,
        });
        let task = CoordinatorTask {
            coordinator: coordinator.clone(),
            triggers: trigger_receiver,
            batches: setup.batches,
            reconciler: setup.reconciler,
            scan_permits: setup.scan_permits,
            limits: setup.limits,
        };
        (coordinator, task)
    }

    pub(crate) fn key(&self) -> &CheckoutKey {
        &self.key
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The codebase this checkout is filed under.
    pub(crate) async fn codebase_id(&self) -> Option<String> {
        self.codebase_id.lock().await.clone()
    }

    /// Record a job without running a reconcile, for tests that check how a
    /// recorded job is reported.
    #[cfg(test)]
    pub(crate) async fn set_last_job(&self, job_id: &str) {
        *self.last_job.lock().await = Some(LastJob {
            job_id: job_id.to_string(),
        });
    }

    /// Ask for a reconcile.
    ///
    /// Never waits and never fails. A full queue already holds more reasons to
    /// run than one run can consume, so the trigger is folded into the
    /// overflow flag instead of blocking the caller.
    pub(crate) fn trigger(&self, trigger: Trigger) {
        if self.triggers.try_send(trigger).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
    }

    /// The gate an `index_codebase` caller waits on.
    ///
    /// A pending or successful gate is returned unchanged, so two callers share
    /// one first index. A failed gate is replaced with a fresh pending one and
    /// a startup run is queued: a first index that failed must be retryable,
    /// and the failure must not be inherited by the next caller.
    pub(crate) async fn renew_first_index_gate(&self) -> Arc<InitialIndexGate> {
        let mut slot = self.gate.lock().await;
        if let Some(gate) = slot.as_ref()
            && !matches!(gate.outcome().await, Some(Err(_)))
        {
            return gate.clone();
        }
        let gate = Arc::new(InitialIndexGate::pending());
        *slot = Some(gate.clone());
        drop(slot);
        self.trigger(Trigger::Startup);
        gate
    }

    /// The gate of a first index in progress, completed, or failed here.
    pub(crate) async fn gate(&self) -> Option<Arc<InitialIndexGate>> {
        self.gate.lock().await.clone()
    }

    pub(crate) fn watcher_state(&self) -> WatcherState {
        match &*lock(&self.watch) {
            Watch::Active(_) => WatcherState::Active,
            Watch::Unavailable(reason) => WatcherState::Unavailable(reason.clone()),
        }
    }

    /// Everything a status caller needs. Each field is read on its own, so this
    /// call holds no two coordinator locks at once.
    pub(crate) async fn status(&self) -> CoordinatorStatus {
        let codebase_id = self.codebase_id.lock().await.clone();
        let last_job_id = self
            .last_job
            .lock()
            .await
            .as_ref()
            .map(|job| job.job_id.clone());
        let run = self.run.lock().await;
        CoordinatorStatus {
            root: self.root.clone(),
            codebase_id,
            leases: self.leases.load(Ordering::Acquire),
            watcher: self.watcher_state(),
            last_job_id,
            running: run.running,
            pending_triggers: self.triggers.max_capacity() - self.triggers.capacity(),
            trigger_overflow: self.overflow.load(Ordering::Acquire),
            last_outcome: run.last_outcome.clone(),
            last_error: run.last_error.clone(),
        }
    }

    /// Take one lease. Only [`super::registry::CoordinatorLease`] calls this.
    pub(super) fn hold_lease(&self) {
        self.leases.fetch_add(1, Ordering::AcqRel);
    }

    /// Release one lease and record when the last one went.
    pub(super) fn release_lease(&self) {
        if self.leases.fetch_sub(1, Ordering::AcqRel) == 1 {
            *lock(&self.released_at) = Some(Instant::now());
        }
    }

    /// Whether no session has held this coordinator for at least `grace`.
    ///
    /// A coordinator that was never released is never idle, which covers the
    /// window between creation and the first lease.
    pub(super) fn idle_for(&self, grace: Duration) -> bool {
        self.leases.load(Ordering::Acquire) == 0
            && lock(&self.released_at).is_some_and(|released| released.elapsed() >= grace)
    }

    /// Stop this coordinator, in the documented order.
    ///
    /// A reconcile that is running finishes its current filesystem operation
    /// and then observes cancellation; it cannot be interrupted between
    /// acquiring a lock and releasing it.
    pub(super) fn cancel(&self) {
        // 1. The loop. Aborting stops further runs and cancels the reconcile
        //    the loop is awaiting.
        abort(&self.task);
        // A first-index poll belongs to a caller that holds a lease, so a
        // cancelled coordinator has no gate waiter left to orphan.
        abort(&self.gate_task);
        // 2. The watch registration, so no event can reach a dead loop.
        *lock(&self.watch) = Watch::Unavailable("coordinator cancelled".to_string());
        // 3. The client goes with the last handle to this coordinator.
        debug!(root = %self.root.display(), "coordinator cancelled");
    }

    /// The periodic backstop, or `None` when the session disabled it.
    fn periodic_timer(&self) -> Option<Interval> {
        let secs = self.resync_secs.unwrap_or(DEFAULT_RESYNC_SECS);
        if secs == 0 {
            info!("periodic re-sync disabled (SEMCTX_MCP_RESYNC_SECS=0)");
            return None;
        }
        let multiplier = match self.watcher_state() {
            WatcherState::Active => WATCHED_RESYNC_MULTIPLIER,
            WatcherState::Unavailable(_) => 1,
        };
        let period = Duration::from_secs(secs.saturating_mul(multiplier));
        let first = Instant::now() + period + jitter(&self.root, period);
        info!(secs = period.as_secs(), "periodic re-sync enabled");
        let mut timer = interval_at(tokio::time::Instant::from_std(first), period);
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Some(timer)
    }

    /// The client for one reconcile: the attached client, bound to the codebase
    /// this checkout is currently filed under.
    async fn request_client(&self) -> Client {
        match self.codebase_id.lock().await.clone() {
            Some(id) => self.client.clone().with_codebase(id),
            None => self.client.clone(),
        }
    }

    /// The codebase to reconcile into, or `None` when there is nothing to
    /// reconcile into yet.
    ///
    /// A first index has no codebase until its caller registers one. Waiting
    /// for that registration is what keeps the coordinator from racing the
    /// caller into a second codebase for the same checkout.
    async fn await_codebase(&self, gate: Option<&Arc<InitialIndexGate>>) -> bool {
        if self.codebase_id.lock().await.is_some() {
            return true;
        }
        let Some(gate) = gate else {
            // No first index is in progress. `sync` resolves or registers the
            // codebase itself, which is the `semctl index` contract.
            return true;
        };
        if let Some(id) = gate.registered_codebase().await {
            *self.codebase_id.lock().await = Some(id);
            return true;
        }
        // The caller finished the gate before it named a codebase. There is
        // nothing to sync into, and registering one here would index a checkout
        // nobody asked to index.
        debug!(root = %self.root.display(), "first index ended before it named a codebase");
        false
    }

    /// One reconcile, from permit to recorded result.
    async fn run_once(
        self: &Arc<Self>,
        trigger: Trigger,
        reconciler: &Arc<dyn Reconciler>,
        scan_permits: &Arc<Semaphore>,
        limits: &SyncLimits,
    ) {
        let gate = self.gate().await;
        if !self.await_codebase(gate.as_ref()).await {
            return;
        }
        let client = self.request_client().await;
        self.run.lock().await.running = true;
        // The permit bounds concurrent scans across every checkout in the
        // process. It is held for the whole reconcile, including its uploads,
        // because the scan's retained content is what the bound protects.
        let permit = scheduler::permit(scan_permits).await;
        let result = reconciler
            .reconcile(ReconcileRun {
                client: client.clone(),
                root: self.root.clone(),
                cache: self.cache.clone(),
                limits: limits.clone(),
            })
            .await;
        drop(permit);
        self.record(trigger, &result).await;
        // Any run can report a first index, because a run only starts once the
        // gate has named its codebase. Keying this on the trigger kind would
        // lose the report whenever another reason won the coalescing race, and
        // the caller would then wait for a result nothing produces. `gate` was
        // read before the run, so a gate installed during a run is reported by
        // the run that gate queued.
        if let Some(gate) = gate {
            self.finish_gate(
                gate,
                reconciler,
                client,
                result.map(|outcome| outcome.job_id),
            );
        }
        let status = self.status().await.to_string();
        debug!(%status, trigger = trigger.as_str(), "reconcile finished");
    }

    /// Record what a reconcile did, for status and for the search footer.
    ///
    /// A startup run always records its job: it is the one `sync_status`
    /// reports until a later sync pushes something. A periodic or watch run
    /// records only when it changed something, so an idle checkout keeps
    /// pointing at the last meaningful job.
    async fn record(&self, trigger: Trigger, result: &Result<SyncOutcome, String>) {
        let mut state = self.run.lock().await;
        state.running = false;
        match result {
            Ok(outcome) => {
                let changed = outcome.uploaded > 0 || outcome.to_delete > 0;
                if trigger == Trigger::Startup {
                    info!(
                        codebase = %outcome.codebase_id,
                        job = %outcome.job_id,
                        uploaded = outcome.uploaded,
                        to_delete = outcome.to_delete,
                        "auto-index queued",
                    );
                } else if changed {
                    info!(
                        uploaded = outcome.uploaded,
                        to_delete = outcome.to_delete,
                        job = %outcome.job_id,
                        trigger = trigger.as_str(),
                        "re-sync pushed changes",
                    );
                } else {
                    debug!(trigger = trigger.as_str(), "re-sync: no changes");
                }
                state.last_outcome = Some(format!(
                    "{} uploaded {} file(s), {} to delete",
                    trigger.as_str(),
                    outcome.uploaded,
                    outcome.to_delete
                ));
                state.last_error = None;
                drop(state);
                *self.codebase_id.lock().await = Some(outcome.codebase_id.clone());
                if trigger == Trigger::Startup || changed {
                    *self.last_job.lock().await = Some(LastJob {
                        job_id: outcome.job_id.clone(),
                    });
                }
            }
            Err(reason) => {
                warn!(error = %reason, trigger = trigger.as_str(), "auto-index failed; serving existing index");
                state.last_error = Some(reason.clone());
            }
        }
    }

    /// Hand a run's result to the first-index gate, if it is still waiting for
    /// one.
    ///
    /// Detached: embedding can take minutes, and the loop must stay free to
    /// pick up edits made while it runs. The handle is kept so cancellation
    /// stops the poll.
    fn finish_gate(
        self: &Arc<Self>,
        gate: Arc<InitialIndexGate>,
        reconciler: &Arc<dyn Reconciler>,
        client: Client,
        job: Result<String, String>,
    ) {
        let reconciler = reconciler.clone();
        let handle = tokio::spawn(async move {
            if gate.outcome().await.is_some() {
                // Its caller already reported a failure of its own.
                return;
            }
            let result = match job {
                Ok(job_id) => reconciler.await_job(client, job_id).await,
                Err(reason) => Err(format!("initial scan/upload failed: {reason}")),
            };
            gate.finish(result).await;
        });
        *lock(&self.gate_task) = Some(handle);
    }
}

impl Drop for CheckoutCoordinator {
    fn drop(&mut self) {
        // Reaching here means the loop's weak handle is dead, so it cannot run
        // again. Abort anyway: it may be parked in a reconcile that would
        // otherwise keep scanning a checkout nothing is serving.
        for slot in [&mut self.task, &mut self.gate_task] {
            if let Some(task) = slot
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                task.abort();
            }
        }
    }
}

/// The unstarted reconcile loop of one coordinator.
pub(crate) struct CoordinatorTask {
    coordinator: Arc<CheckoutCoordinator>,
    triggers: mpsc::Receiver<Trigger>,
    batches: mpsc::Receiver<WatchBatch>,
    reconciler: Arc<dyn Reconciler>,
    scan_permits: Arc<Semaphore>,
    limits: SyncLimits,
}

impl CoordinatorTask {
    /// Start the loop and give its handle to the coordinator.
    ///
    /// The loop keeps only a weak handle, so the task cannot keep the
    /// coordinator alive: when the registry releases the last strong handle,
    /// the trigger senders go with it and the loop ends by itself.
    pub(crate) fn spawn(self) {
        let Self {
            coordinator,
            triggers,
            batches,
            reconciler,
            scan_permits,
            limits,
        } = self;
        let loop_state = ReconcileLoop {
            coordinator: Arc::downgrade(&coordinator),
            triggers,
            batches,
            reconciler,
            scan_permits,
            limits,
        };
        let handle = tokio::spawn(loop_state.run());
        *lock(&coordinator.task) = Some(handle);
    }
}

struct ReconcileLoop {
    coordinator: Weak<CheckoutCoordinator>,
    triggers: mpsc::Receiver<Trigger>,
    batches: mpsc::Receiver<WatchBatch>,
    reconciler: Arc<dyn Reconciler>,
    scan_permits: Arc<Semaphore>,
    limits: SyncLimits,
}

impl ReconcileLoop {
    async fn run(mut self) {
        let Some(coordinator) = self.coordinator.upgrade() else {
            return;
        };
        let root = coordinator.root.clone();
        let overflow = coordinator.overflow.clone();
        let mut timer = coordinator.periodic_timer();
        let mut watching = matches!(coordinator.watcher_state(), WatcherState::Active);
        // Nothing strong is held while waiting, so the coordinator stays free
        // to be dropped by its registry.
        drop(coordinator);

        // Policy sources seen so far. They decide, without a file read, whether
        // an event can affect the rules themselves.
        let mut observed: HashSet<PathBuf> = HashSet::new();
        loop {
            let mut due: Option<Trigger> = None;
            let mut events: Vec<Event> = Vec::new();
            tokio::select! {
                trigger = self.triggers.recv() => match trigger {
                    Some(trigger) => due = Some(trigger),
                    // The coordinator is gone. Nothing can trigger this loop
                    // again, and nothing owns the checkout it served.
                    None => return,
                },
                batch = self.batches.recv(), if watching => match batch {
                    Some(batch) => events = batch.events,
                    // The watch registration was released. Keep serving
                    // triggers and the periodic backstop.
                    None => watching = false,
                },
                () = next_tick(&mut timer) => due = Some(Trigger::Periodic),
            }

            // Drain everything that arrived, so a burst becomes one run.
            while let Ok(trigger) = self.triggers.try_recv() {
                due = due.or(Some(trigger));
            }
            while let Ok(batch) = self.batches.try_recv() {
                events.extend(batch.events);
            }
            // Dropped triggers or events can name any path, so what was lost
            // cannot be judged: the run becomes unconditional.
            if overflow.swap(false, Ordering::AcqRel) {
                due = due.or(Some(Trigger::Watch));
            }
            if due.is_none() && !events.is_empty() {
                let (relevant, next_observed) = relevance(&root, events, observed).await;
                observed = next_observed;
                if relevant {
                    due = Some(Trigger::Watch);
                }
            }
            let Some(trigger) = due else { continue };
            let Some(coordinator) = self.coordinator.upgrade() else {
                return;
            };
            coordinator
                .run_once(trigger, &self.reconciler, &self.scan_permits, &self.limits)
                .await;
            // A rule can select a file in another directory. Subscribe to it
            // after the run, so a later edit to that rule still wakes us.
            if watching {
                refresh_external_watches(&coordinator).await;
            }
            drop(coordinator);
        }
    }
}

/// Wait for the next periodic tick, or forever when the timer is disabled.
async fn next_tick(timer: &mut Option<Interval>) {
    match timer {
        Some(timer) => {
            timer.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Spread the first tick of 1,000 coordinators across one interval.
///
/// The fraction is a digest of the root, so it needs no random-number source
/// and one checkout always picks the same offset. The caller adds it to a full
/// interval, which also keeps the backstop from re-scanning immediately after
/// the startup run.
fn jitter(root: &Path, interval: Duration) -> Duration {
    let digest = blake3::hash(root.as_os_str().as_encoded_bytes());
    let mut bucket = [0_u8; 8];
    bucket.copy_from_slice(&digest.as_bytes()[..8]);
    let fraction = u128::from(u64::from_le_bytes(bucket) % 1000);
    Duration::from_nanos(u64::try_from(interval.as_nanos() * fraction / 1000).unwrap_or(u64::MAX))
}

/// Whether this batch of events justifies a reconcile, and the policy sources
/// to keep watching for the next one.
///
/// Runs on the blocking pool: the cheap event filter is followed by a policy
/// load, which reads rule files. Notifications and manifests share one checked
/// policy engine. A load error reports the batch as relevant, so the
/// authoritative scanner runs, reports the error, and refuses to upload rather
/// than treating unreadable rules as empty.
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

/// Subscribe to every rule directory the current policy names.
///
/// Best effort: a failure is logged and the periodic re-sync covers the change.
/// Runs on the blocking pool, because it probes the filesystem and touches the
/// platform watcher.
async fn refresh_external_watches(coordinator: &Arc<CheckoutCoordinator>) {
    if !matches!(coordinator.watcher_state(), WatcherState::Active) {
        return;
    }
    let coordinator = coordinator.clone();
    let root = coordinator.root.clone();
    let refreshed = blocking::run(move |cancellation| {
        let policy = SourcePolicy::load(&root, &cancellation)?;
        let watch = lock(&coordinator.watch);
        let Watch::Active(registration) = &*watch else {
            return Ok(());
        };
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
                .event_is_relevant(path, path.is_dir(), &blocking::Cancellation::default())
                .unwrap_or(true)
        })
}

/// Read and open events are ignored: the reconcile itself walks and opens the
/// watched tree, so letting them through would make each finished sync queue
/// its successor forever on platforms that report file access.
fn can_change_tree(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Write))
        )
}

/// Poll one embedding job to a terminal state.
async fn wait_for_initial_job(client: &Client, job_id: &str) -> Result<(), String> {
    loop {
        let job = client
            .get::<client::api::JobStatus>(&format!("/v1/jobs/{job_id}"))
            .await
            .map_err(|e| format!("poll initial index job {job_id}: {e}"))?;
        if let Some(result) = initial_job_result(job_id, &job) {
            return result;
        }
        tokio::time::sleep(JOB_POLL_EVERY).await;
    }
}

/// A first index is ready only when embedding finished with nothing failed.
/// Retrieval must never run against a partial first index.
pub(crate) fn initial_job_result(
    job_id: &str,
    job: &client::api::JobStatus,
) -> Option<Result<(), String>> {
    if let Some(error) = &job.error {
        return Some(Err(format!("embedding job {job_id} failed: {error}")));
    }
    job.completed_at.as_ref()?;
    if job.files_failed > 0 {
        return Some(Err(format!(
            "embedding job {job_id} completed with {} failed file(s)",
            job.files_failed
        )));
    }
    Some(Ok(()))
}

/// A poisoned coordinator lock means a previous holder panicked while holding a
/// watch handle, a join handle, or an instant. Recovering keeps the checkout
/// served; refusing would strand it with no way to cancel it.
fn lock<T>(value: &StdMutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort(slot: &StdMutex<Option<JoinHandle<()>>>) {
    if let Some(task) = lock(slot).take() {
        task.abort();
    }
}

#[cfg(test)]
mod tests;

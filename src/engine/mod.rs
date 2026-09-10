//! The shared engine.
//!
//! One engine per process owns everything that must not be duplicated per
//! session: the permits that bound concurrent work, the filesystem watch hub,
//! and the registry of checkout coordinators. A session holds a handle to the
//! engine and a lease on each checkout it uses, and nothing that another
//! session on the same checkout would own a second copy of.
//!
//! The same engine serves the standalone `semctl mcp` process. Standalone mode
//! is this engine with exactly one session.

pub(crate) mod coordinator;
pub(crate) mod registry;
pub(crate) mod scheduler;
pub(crate) mod watch_hub;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;
use tracing::info;

use crate::client::HttpTransport;

pub(crate) use coordinator::{CoordinatorStatus, Trigger};
pub(crate) use registry::{CheckoutKey, CheckoutRegistry, CoordinatorLease};
pub(crate) use scheduler::{Scheduler, SchedulerSettings, SchedulerUsage};
pub(crate) use watch_hub::WatchHub;

/// How long a coordinator with no lease stays alive. A session that reconnects
/// within the grace reuses the watcher and the content cache it left behind.
const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(300);

/// How often the registry looks for coordinators no session holds.
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// What one engine is built with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EngineSettings {
    scheduler: SchedulerSettings,
    idle_grace: Duration,
}

impl EngineSettings {
    /// The settings this process was started with.
    pub(crate) fn from_environment() -> Self {
        Self {
            scheduler: SchedulerSettings::from_environment(),
            // Not an environment key: retention is an engine policy, and a
            // session must not be able to pin every checkout it ever touched.
            idle_grace: DEFAULT_IDLE_GRACE,
        }
    }
}

/// Everything one process shares across its sessions.
pub(crate) struct Engine {
    registry: Arc<CheckoutRegistry>,
    scheduler: Arc<Scheduler>,
    transport: HttpTransport,
    /// One-line "a newer semctl is published" prompt, set by the one update
    /// check this process runs and consumed by one search footer. `None` until
    /// a newer version is seen. Notify-only: applying the update stays the
    /// explicit `semctl upgrade`.
    update_note: Arc<Mutex<Option<String>>>,
    /// Whether the update check has been started. One process asks once,
    /// however many sessions it serves.
    update_check_started: AtomicBool,
}

impl Engine {
    /// Build the engine.
    ///
    /// Call this inside a runtime: it starts the registry's idle sweeper. The
    /// sweeper holds a weak handle, so it ends when the engine does.
    pub(crate) fn new(settings: EngineSettings) -> Result<Arc<Self>> {
        let scheduler = Arc::new(Scheduler::new(settings.scheduler));
        let registry = Arc::new(CheckoutRegistry::new(
            Arc::new(WatchHub::new()),
            &scheduler,
            settings.idle_grace,
        ));
        spawn_idle_sweeper(Arc::downgrade(&registry));
        Ok(Arc::new(Self {
            registry,
            scheduler,
            transport: HttpTransport::new()?,
            update_note: Arc::new(Mutex::new(None)),
            update_check_started: AtomicBool::new(false),
        }))
    }

    /// An engine whose reconciles are counted instead of performed.
    #[cfg(test)]
    pub(crate) fn for_test(reconciler: Arc<dyn coordinator::Reconciler>) -> Arc<Self> {
        let scheduler = Arc::new(Scheduler::from_environment());
        Arc::new(Self {
            registry: Arc::new(CheckoutRegistry::for_test(reconciler, DEFAULT_IDLE_GRACE)),
            scheduler,
            transport: HttpTransport::new().expect("build the test transport"),
            update_note: Arc::new(Mutex::new(None)),
            update_check_started: AtomicBool::new(false),
        })
    }

    pub(crate) fn registry(&self) -> &Arc<CheckoutRegistry> {
        &self.registry
    }

    pub(crate) fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// The one `reqwest` client of this process. Every session's HTTP client
    /// shares its connection pool.
    pub(crate) fn transport(&self) -> &HttpTransport {
        &self.transport
    }

    /// The update note, for a session that asked for the check.
    pub(crate) fn update_note(&self) -> &Arc<Mutex<Option<String>>> {
        &self.update_note
    }

    /// Start the one update check of this process.
    ///
    /// The first session whose context asks for it starts it; every later
    /// session reuses the note. A session with the check off neither starts it
    /// nor reads the note, so one session cannot make another pay for a
    /// lookup it declined.
    ///
    /// Detached and best effort: a failed check records nothing, because a
    /// missed notice must not affect a tool call.
    pub(crate) fn start_update_check(&self, server_override: Option<String>, enabled: bool) {
        if !enabled || self.update_check_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let note = self.update_note.clone();
        tokio::spawn(async move {
            let Some(latest) =
                crate::commands::upgrade::check_for_update(server_override.as_deref()).await
            else {
                return;
            };
            let current = env!("CARGO_PKG_VERSION");
            info!(current, %latest, "a newer semctl is available — run `semctl upgrade`");
            *note.lock().await = Some(format!(
                "(semctl update available: v{latest} — you're on v{current}; \
                 run `semctl upgrade` to update)"
            ));
        });
    }
}

/// Release coordinators no session has held for the idle grace.
fn spawn_idle_sweeper(registry: Weak<CheckoutRegistry>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            // A weak handle: the sweeper must not keep the registry, and
            // therefore every watcher, alive after the engine is gone.
            let Some(registry) = registry.upgrade() else {
                return;
            };
            registry.sweep_idle();
        }
    });
}

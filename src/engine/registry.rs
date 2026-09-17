//! Which checkout a session's work belongs to, and who owns that checkout.
//!
//! A coordinator is shared, so what identifies it must be exactly what makes
//! two sessions' work interchangeable: the server, the tenant, the credentials,
//! and the working-copy root. The codebase id is not part of the key. It is
//! binding state the server can move, and the coordinator records what the last
//! reconcile confirmed.
//!
//! Lock order is: the registry map, then a coordinator's internals. Nothing
//! holds the map lock across an await, and the two calls that talk to the watch
//! hub or the server do so with the map lock released.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tracing::debug;

use super::coordinator::{
    CheckoutCoordinator, CoordinatorSetup, Reconciler, SyncReconciler, Trigger,
};
use super::scheduler::Scheduler;
use super::watch_hub::{BATCH_CAPACITY, WatchHub, WatchSink};
use crate::client::Client;
use crate::mcp::readiness::InitialIndexGate;
use crate::session::CredentialScope;
use crate::sync::SyncLimits;

/// What makes two sessions' work on one checkout interchangeable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CheckoutKey {
    server_url: String,
    tenant: Option<String>,
    /// Two sessions with different credential scopes on one root get two
    /// coordinators. That duplicates a watcher and never mixes authorization.
    credential_scope: CredentialScope,
    root: PathBuf,
}

impl CheckoutKey {
    /// The key of `root` as `client` sees it.
    ///
    /// The tenant is the client's effective tenant now. A tenant repaired later
    /// does not move an attached coordinator; the next attach keys on the
    /// repaired value.
    ///
    /// `root` is canonicalized so two spellings of one checkout are one key. An
    /// unresolvable path is kept as given, which matches how the rest of the
    /// program treats a root it cannot canonicalize.
    pub(crate) async fn for_client(client: &Client, root: PathBuf) -> Self {
        Self {
            server_url: client.server_url().to_string(),
            tenant: client.tenant().await,
            credential_scope: client.credential_scope(),
            root: std::fs::canonicalize(&root).unwrap_or(root),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// One session's claim on one coordinator.
///
/// The coordinator outlives any one session, and the lease count is how the
/// registry knows when no session needs it any more.
pub(crate) struct CoordinatorLease {
    coordinator: Arc<CheckoutCoordinator>,
}

impl CoordinatorLease {
    fn take(coordinator: Arc<CheckoutCoordinator>) -> Self {
        coordinator.hold_lease();
        Self { coordinator }
    }

    pub(crate) fn coordinator(&self) -> &Arc<CheckoutCoordinator> {
        &self.coordinator
    }

    pub(crate) fn key(&self) -> &CheckoutKey {
        self.coordinator.key()
    }
}

impl Drop for CoordinatorLease {
    fn drop(&mut self) {
        self.coordinator.release_lease();
    }
}

/// Every checkout this process keeps in sync.
pub(crate) struct CheckoutRegistry {
    coordinators: StdMutex<HashMap<CheckoutKey, Arc<CheckoutCoordinator>>>,
    hub: Arc<WatchHub>,
    scan_permits: Arc<Semaphore>,
    limits: SyncLimits,
    reconciler: Arc<dyn Reconciler>,
    /// How long a coordinator with no lease stays alive. A session that
    /// reconnects within the grace reuses its watcher and its content cache.
    idle_grace: Duration,
}

impl CheckoutRegistry {
    pub(crate) fn new(hub: Arc<WatchHub>, scheduler: &Scheduler, idle_grace: Duration) -> Self {
        Self {
            coordinators: StdMutex::new(HashMap::new()),
            hub,
            scan_permits: scheduler.scan_permits(),
            limits: SyncLimits::new(scheduler.upload_permits()),
            reconciler: Arc::new(SyncReconciler),
            idle_grace,
        }
    }

    /// A registry whose reconciles are counted instead of performed.
    #[cfg(test)]
    pub(crate) fn for_test(reconciler: Arc<dyn Reconciler>, idle_grace: Duration) -> Self {
        let scheduler = Scheduler::from_environment();
        Self {
            coordinators: StdMutex::new(HashMap::new()),
            hub: Arc::new(WatchHub::new()),
            scan_permits: scheduler.scan_permits(),
            limits: SyncLimits::new(scheduler.upload_permits()),
            reconciler,
            idle_grace,
        }
    }

    /// The coordinator for `key`, if this process has one.
    pub(crate) fn coordinator(&self, key: &CheckoutKey) -> Option<Arc<CheckoutCoordinator>> {
        lock(&self.coordinators).get(key).cloned()
    }

    /// Keep `root` in sync for as long as the returned lease lives.
    ///
    /// An existing coordinator is reused, whichever session created it. A new
    /// one registers with the watch hub, starts its task, and queues a startup
    /// reconcile.
    pub(crate) async fn attach(
        &self,
        client: Client,
        root: PathBuf,
        resync_secs: Option<u64>,
    ) -> Result<CoordinatorLease, String> {
        let (lease, _) = self.attach_inner(client, root, resync_secs, false).await?;
        Ok(lease)
    }

    /// Attach and return the gate a first index is reported through.
    ///
    /// The gate exists before anything is registered on the server, so a
    /// retrieval call that arrives while `index_codebase` is still registering
    /// already has something to wait on. The caller registers the codebase and
    /// then calls [`InitialIndexGate::register_codebase`]; the coordinator
    /// waits for that rather than registering a second codebase itself.
    pub(crate) async fn attach_first_index(
        &self,
        client: Client,
        root: PathBuf,
        resync_secs: Option<u64>,
    ) -> Result<(CoordinatorLease, Arc<InitialIndexGate>), String> {
        let (lease, gate) = self.attach_inner(client, root, resync_secs, true).await?;
        let gate = match gate {
            Some(gate) => gate,
            // Reused coordinator: it decides whether its existing gate still
            // serves this caller or a fresh one must replace a failed index.
            None => lease.coordinator().renew_first_index_gate().await,
        };
        Ok((lease, gate))
    }

    /// Reuse or create. The returned gate is `Some` only for a coordinator this
    /// call created, which is the only case where the gate is already in place.
    async fn attach_inner(
        &self,
        client: Client,
        root: PathBuf,
        resync_secs: Option<u64>,
        first_index: bool,
    ) -> Result<(CoordinatorLease, Option<Arc<InitialIndexGate>>), String> {
        let key = CheckoutKey::for_client(&client, root).await;
        if let Some(coordinator) = self.coordinator(&key) {
            return Ok((CoordinatorLease::take(coordinator), None));
        }

        // Registering with the hub walks the tree to seed the watcher's file-id
        // cache. It runs on the blocking pool, and the map lock is not held.
        let (sender, batches) = mpsc::channel(BATCH_CAPACITY);
        let overflow = Arc::new(AtomicBool::new(false));
        let hub = self.hub.clone();
        let watch_root = key.root.clone();
        let sink = WatchSink::new(sender, overflow.clone());
        let watch = tokio::task::spawn_blocking(move || hub.register(watch_root, sink))
            .await
            .map_err(|error| format!("filesystem watch registration task: {error}"))?
            .map_err(|error| format!("{error:#}"));

        let gate = first_index.then(|| Arc::new(InitialIndexGate::pending()));
        let (coordinator, task) = CheckoutCoordinator::new(CoordinatorSetup {
            key: key.clone(),
            client,
            resync_secs,
            watch,
            batches,
            overflow,
            gate: gate.clone(),
            reconciler: self.reconciler.clone(),
            scan_permits: self.scan_permits.clone(),
            limits: self.limits.clone(),
        });
        // Hold the lease before publishing, so the sweeper can never see a
        // coordinator that is created but not yet leased.
        let lease = CoordinatorLease::take(coordinator.clone());

        let published = match lock(&self.coordinators).entry(key) {
            Entry::Occupied(entry) => Err(entry.get().clone()),
            Entry::Vacant(entry) => {
                entry.insert(coordinator.clone());
                Ok(())
            }
        };
        match published {
            Ok(()) => {
                task.spawn();
                coordinator.trigger(Trigger::Startup);
                debug!(root = %coordinator.root().display(), "checkout coordinator started");
                Ok((lease, gate))
            }
            Err(existing) => {
                // Another session created the same checkout while this call was
                // registering. Release what this call built, in the documented
                // order, and use theirs.
                drop(lease);
                drop(task);
                coordinator.cancel();
                drop(coordinator);
                Ok((CoordinatorLease::take(existing), None))
            }
        }
    }

    /// Remove every coordinator no session has held for the idle grace.
    ///
    /// Cancellation happens with the map lock released: it touches the watch
    /// hub, and the lock order forbids holding the map lock over that.
    pub(crate) fn sweep_idle(&self) {
        let mut released = Vec::new();
        {
            let mut coordinators = lock(&self.coordinators);
            coordinators.retain(|_, coordinator| {
                if coordinator.idle_for(self.idle_grace) {
                    released.push(coordinator.clone());
                    false
                } else {
                    true
                }
            });
        }
        if released.is_empty() {
            return;
        }
        for coordinator in released {
            debug!(root = %coordinator.root().display(), "releasing idle checkout coordinator");
            coordinator.cancel();
        }
        debug!(checkouts = self.len(), "idle sweep finished");
    }

    /// How many checkouts this process keeps in sync.
    pub(crate) fn len(&self) -> usize {
        lock(&self.coordinators).len()
    }
}

/// A poisoned registry lock means a previous holder panicked while holding a
/// map of handles. Recovering keeps every attached checkout served; refusing
/// would make the process unable to serve any new session.
fn lock<T>(value: &StdMutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;

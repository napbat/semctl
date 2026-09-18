//! First-index readiness for checkout and cross-codebase retrieval.
//!
//! A first index is owned by the checkout's coordinator, not by a session, so
//! readiness asks the coordinators this session holds leases on. A session
//! therefore never waits for another session's first index, and every checkout
//! this session did bring in is waited for.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify, RwLock, RwLockReadGuard};

use crate::engine::{CheckoutKey, CoordinatorLease};

/// The coordinators one session holds, keyed by checkout.
pub(super) type SessionLeases = RwLock<HashMap<CheckoutKey, CoordinatorLease>>;

/// The first-index gates of the checkouts this session holds.
async fn session_gates(
    leases: &HashMap<CheckoutKey, CoordinatorLease>,
) -> Vec<Arc<InitialIndexGate>> {
    let mut gates = Vec::with_capacity(leases.len());
    for lease in leases.values() {
        if let Some(gate) = lease.coordinator().gate().await {
            gates.push(gate);
        }
    }
    gates
}

/// Wait for every gate that the query's scope can include.
///
/// Empty ids mean a server-defined scope. Its membership is unknown here, so
/// every first index this session brought in must complete.
pub(super) async fn wait_for_gates(
    gates: &[Arc<InitialIndexGate>],
    ids: &[String],
) -> Result<(), String> {
    for gate in gates {
        gate.wait_for_codebases(ids)
            .await
            .map_err(|error| format!("initial index failed — {error}"))?;
    }
    Ok(())
}

/// Wait without holding the lease map, then reserve the checked membership for
/// the query. A tool call can attach a checkout while embedding runs. If it
/// does, repeat the readiness check before allowing the query to include the
/// new codebase.
pub(super) async fn ready_for_codebases<'a>(
    leases: &'a SessionLeases,
    ids: &[String],
) -> Result<RwLockReadGuard<'a, HashMap<CheckoutKey, CoordinatorLease>>, String> {
    loop {
        let checked = {
            let held = leases.read().await;
            Membership::of(&held).await
        };
        wait_for_gates(&checked.gates, ids).await?;
        let current = leases.read().await;
        if Membership::of(&current).await.counts() == checked.counts() {
            return Ok(current);
        }
    }
}

/// What a readiness check covered: which checkouts this session held, and how
/// many of them had a first index to wait for.
struct Membership {
    checkouts: usize,
    gates: Vec<Arc<InitialIndexGate>>,
}

impl Membership {
    async fn of(leases: &HashMap<CheckoutKey, CoordinatorLease>) -> Self {
        Self {
            checkouts: leases.len(),
            gates: session_gates(leases).await,
        }
    }

    /// A new checkout, or a new first index on a checkout this session already
    /// held, both change what the query would include.
    fn counts(&self) -> (usize, usize) {
        (self.checkouts, self.gates.len())
    }
}

/// Release the lease map before waiting for a single checkout.
///
/// `dir` must be canonical: a coordinator is keyed by its canonical root.
pub(super) async fn initial_gate_for_path(
    leases: &SessionLeases,
    dir: &Path,
) -> Option<Arc<InitialIndexGate>> {
    let held = leases.read().await;
    for lease in held.values() {
        if lease.coordinator().root() == dir {
            return lease.coordinator().gate().await;
        }
    }
    None
}

pub(crate) struct InitialIndexGate {
    state: Mutex<InitialIndexState>,
    changed: Notify,
}

#[derive(Clone, Default)]
struct InitialIndexState {
    /// Whether a caller has taken responsibility for registering the codebase.
    registering: bool,
    /// Whether a reconcile has taken responsibility for polling the embedding
    /// job of this first index.
    polling: bool,
    codebase_id: Option<String>,
    result: Option<Result<(), String>>,
}

impl InitialIndexGate {
    pub(crate) fn pending() -> Self {
        Self {
            state: Mutex::new(InitialIndexState::default()),
            changed: Notify::new(),
        }
    }

    /// Registration updates the existing gate. Its checkout stays fixed.
    pub(crate) async fn register_codebase(&self, id: String) {
        self.state.lock().await.codebase_id = Some(id);
        self.changed.notify_waiters();
    }

    pub(crate) async fn wait(&self) -> Result<(), String> {
        self.wait_for_codebases(&[]).await
    }

    /// Take responsibility for registering this first index's codebase.
    ///
    /// Exactly one caller is answered `true` for one gate. Every other caller
    /// waits, so one explicit `index_codebase` on a checkout registers one
    /// codebase however many callers ask at once.
    pub(crate) async fn claim_registration(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.registering {
            return false;
        }
        state.registering = true;
        true
    }

    /// Take responsibility for polling this first index's embedding job.
    ///
    /// Exactly one reconcile is answered `true` for one gate. A second
    /// reconcile that starts while embedding runs sees the same pending gate;
    /// without this claim it would start a second poll of a second job, and
    /// whichever poll finished last would decide the result for every session.
    ///
    /// The claim is never given back. A gate that failed is replaced with a
    /// fresh one by [`crate::engine::coordinator::CheckoutCoordinator::renew_first_index_gate`],
    /// and a cancelled coordinator has no waiter left to answer.
    pub(crate) async fn claim_poll(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.polling {
            return false;
        }
        state.polling = true;
        true
    }

    /// The final result, or `None` while the first index is still running.
    ///
    /// Never waits. The registry uses it to tell a failed first index from a
    /// pending one, and the coordinator uses it to avoid replacing a result its
    /// caller already reported.
    pub(crate) async fn outcome(&self) -> Option<Result<(), String>> {
        self.state.lock().await.result.clone()
    }

    /// Wait until the first index has a codebase, or until it ends without one.
    ///
    /// The coordinator calls this before its startup reconcile. A first index
    /// has no codebase until `index_codebase` registers it, and reconciling
    /// before then would register a second codebase for the same checkout.
    pub(crate) async fn registered_codebase(&self) -> Option<String> {
        loop {
            // Register before reading state so a transition cannot be missed.
            let changed = self.changed.notified();
            let state = self.state.lock().await.clone();
            if state.codebase_id.is_some() || state.result.is_some() {
                return state.codebase_id;
            }
            changed.await;
        }
    }

    async fn wait_for_codebases(&self, ids: &[String]) -> Result<(), String> {
        loop {
            // Register before reading state so a transition cannot be missed.
            let changed = self.changed.notified();
            let state = self.state.lock().await.clone();
            if !ids.is_empty()
                && state
                    .codebase_id
                    .as_ref()
                    .is_some_and(|id| !ids.contains(id))
            {
                return Ok(());
            }
            if let Some(result) = state.result {
                return result;
            }
            changed.await;
        }
    }

    /// Record the outcome of this first index, first writer wins.
    ///
    /// A gate is reported once. A later report describes another run of the
    /// same first index, and accepting it would let a succeeded index turn
    /// into a failed one for every session that already read the result.
    pub(crate) async fn finish(&self, result: Result<(), String>) {
        let mut state = self.state.lock().await;
        if state.result.is_some() {
            return;
        }
        state.result = Some(result);
        drop(state);
        self.changed.notify_waiters();
    }
}

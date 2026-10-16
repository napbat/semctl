//! First-index readiness for checkout and cross-codebase retrieval.
//!
//! A first index is owned by the checkout's coordinator, not by a session, so
//! readiness asks the coordinators this session holds leases on. A session
//! therefore never waits for another session's first index, and every checkout
//! this session did bring in is waited for.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, RwLock, RwLockReadGuard};
use tokio::time::Instant;

use super::CallBudget;
use crate::engine::{CheckoutKey, CoordinatorLease};
use crate::query::{FailureKind, ToolError};

/// The longest a bounded call waits for a first index. A small codebase
/// finishes inside it. A larger one is reported as pending, so the caller can
/// use its own tools instead of waiting out the whole call deadline.
const FIRST_INDEX_WAIT: Duration = Duration::from_secs(5);

/// Why a wait for a first index ended without a usable index.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NotReady {
    /// The first index still runs after the longest wait that the call may
    /// spend. A partial first index is never served.
    Pending,
    /// The first index failed. The text says why.
    Failed(String),
}

impl NotReady {
    /// The failure that the tool `op` reports for this cause.
    pub(super) fn into_tool_error(self, op: &'static str) -> ToolError {
        match self {
            Self::Pending => ToolError::new(
                op,
                FailureKind::IndexPending,
                "the first index of this codebase is still running",
            ),
            Self::Failed(detail) => ToolError::new(op, FailureKind::IndexFailed, detail),
        }
    }
}

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

/// Wait for `wait`, which reports one or more first indexes. `until` ends the
/// wait. `None` waits for as long as the first index takes.
async fn until_ready(
    until: Option<Instant>,
    wait: impl Future<Output = Result<(), String>>,
) -> Result<(), NotReady> {
    let outcome = match until {
        Some(until) => tokio::time::timeout_at(until, wait)
            .await
            .map_err(|_| NotReady::Pending)?,
        None => wait.await,
    };
    outcome.map_err(|error| NotReady::Failed(initial_index_failed(&error)))
}

/// Wait for every gate that the query's scope can include.
///
/// Empty ids mean a server-defined scope. Its membership is unknown here, so
/// every first index this session brought in must complete.
pub(super) async fn wait_for_gates(
    gates: &[Arc<InitialIndexGate>],
    ids: &[String],
    until: Option<Instant>,
) -> Result<(), NotReady> {
    until_ready(until, async {
        for gate in gates {
            gate.wait_for_codebases(ids).await?;
        }
        Ok(())
    })
    .await
}

/// Wait for the first index of one checkout, for as long as `budget` allows.
pub(super) async fn wait_for_gate(
    gate: &InitialIndexGate,
    budget: &CallBudget,
) -> Result<(), NotReady> {
    until_ready(budget.wait_until(FIRST_INDEX_WAIT), gate.wait()).await
}

/// The reason a retrieval call reports when a first index failed.
///
/// A failed gate stays failed until something asks for that index again. The
/// `IndexFailed` error names that recovery on its `next:` line.
fn initial_index_failed(error: &str) -> String {
    format!("initial index failed — {error}")
}

/// Wait without holding the lease map, then reserve the checked membership for
/// the query. A tool call can attach a checkout while embedding runs. If it
/// does, repeat the readiness check before allowing the query to include the
/// new codebase.
///
/// A bounded `budget` allows one short wait for all passes together. If a first
/// index still runs after it, the call is [`NotReady::Pending`].
pub(super) async fn ready_for_codebases<'a>(
    leases: &'a SessionLeases,
    ids: &[String],
    budget: &CallBudget,
) -> Result<RwLockReadGuard<'a, HashMap<CheckoutKey, CoordinatorLease>>, NotReady> {
    let until = budget.wait_until(FIRST_INDEX_WAIT);
    loop {
        let checked = {
            let held = leases.read().await;
            Membership::of(&held).await
        };
        wait_for_gates(&checked.gates, ids, until).await?;
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

/// How far one first index has come. `sync_status` and the daemon status
/// report it, so an agent can follow a first index that no tool call waits for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FirstIndexPhase {
    /// `index_codebase` has not yet registered a codebase for the checkout.
    Registering,
    /// The codebase exists. The scan and the upload run, or wait for a permit.
    Syncing,
    /// The upload is done and the server embeds the files. `job_id` names the
    /// server job when the coordinator has recorded it.
    Embedding { job_id: Option<String> },
    /// The server embedded every file. Retrieval serves this index.
    Ready,
    /// The first index ended without a usable index. Another `index_codebase`
    /// call retries it.
    Failed { reason: String },
}

impl FirstIndexPhase {
    /// The phase of a first index, from one snapshot of its gate.
    ///
    /// `last_job_id` is the coordinator's last recorded job. Once the upload is
    /// done, that job is the embedding job of this first index.
    fn of(state: &InitialIndexState, last_job_id: Option<&str>) -> Self {
        match &state.result {
            Some(Ok(())) => Self::Ready,
            Some(Err(reason)) => Self::Failed {
                reason: reason.clone(),
            },
            None if state.codebase_id.is_none() => Self::Registering,
            None if !state.polling => Self::Syncing,
            None => Self::Embedding {
                job_id: last_job_id.map(str::to_string),
            },
        }
    }
}

impl fmt::Display for FirstIndexPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registering => formatter.write_str("registering"),
            Self::Syncing => formatter.write_str("syncing"),
            Self::Embedding { job_id: Some(id) } => write!(formatter, "embedding (job {id})"),
            Self::Embedding { job_id: None } => formatter.write_str("embedding"),
            Self::Ready => formatter.write_str("ready"),
            Self::Failed { reason } => write!(formatter, "failed — {reason}"),
        }
    }
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
    /// reports the first index as already in progress, so one explicit
    /// `index_codebase` on a checkout registers one codebase however many
    /// callers ask at once.
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

    /// The phase of this first index now. Never waits.
    pub(crate) async fn phase(&self, last_job_id: Option<&str>) -> FirstIndexPhase {
        let state = self.state.lock().await;
        FirstIndexPhase::of(&state, last_job_id)
    }

    /// The codebase that `index_codebase` registered for this first index, if
    /// it has registered one. Never waits, unlike [`Self::registered_codebase`].
    pub(crate) async fn codebase(&self) -> Option<String> {
        self.state.lock().await.codebase_id.clone()
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

#[cfg(test)]
mod tests;

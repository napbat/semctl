//! First-index readiness for checkout and cross-codebase retrieval.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, Notify, RwLock, RwLockReadGuard};

#[derive(Clone, Default)]
pub(super) struct InitialIndexes {
    // Membership only grows for the lifetime of the MCP session. A path keeps
    // its original gate, including after completion or failure.
    pub(super) by_path: HashMap<PathBuf, Arc<InitialIndexGate>>,
}

impl InitialIndexes {
    /// Empty ids mean a server-defined scope. Its membership is unknown here,
    /// so every initial index in this session must complete.
    pub(super) async fn wait_for_codebases(&self, ids: &[String]) -> Result<(), String> {
        for gate in self.by_path.values() {
            gate.wait_for_codebases(ids)
                .await
                .map_err(|error| format!("initial index failed — {error}"))?;
        }
        Ok(())
    }
}

/// Wait without holding the registry, then reserve the checked membership for
/// the query. Registration can run during embedding. If it adds a gate, repeat
/// the readiness check before allowing the query to include the new codebase.
pub(super) async fn ready_for_codebases<'a>(
    indexes: &'a RwLock<InitialIndexes>,
    ids: &[String],
) -> Result<RwLockReadGuard<'a, InitialIndexes>, String> {
    loop {
        let snapshot = indexes.read().await.clone();
        snapshot.wait_for_codebases(ids).await?;
        let current = indexes.read().await;
        if current.by_path.len() == snapshot.by_path.len() {
            return Ok(current);
        }
    }
}

/// Release the registry before waiting for a single checkout.
pub(super) async fn initial_gate_for_path(
    indexes: &RwLock<InitialIndexes>,
    dir: &Path,
) -> Option<Arc<InitialIndexGate>> {
    indexes.read().await.by_path.get(dir).cloned()
}

pub(crate) struct InitialIndexGate {
    state: Mutex<InitialIndexState>,
    changed: Notify,
}

#[derive(Clone, Default)]
struct InitialIndexState {
    /// Whether a caller has taken responsibility for registering the codebase.
    registering: bool,
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

    /// Registration updates the existing gate. Its path membership stays fixed.
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

    pub(crate) async fn finish(&self, result: Result<(), String>) {
        self.state.lock().await.result = Some(result);
        self.changed.notify_waiters();
    }
}

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

pub(super) struct InitialIndexGate {
    state: Mutex<InitialIndexState>,
    changed: Notify,
}

#[derive(Clone, Default)]
struct InitialIndexState {
    codebase_id: Option<String>,
    result: Option<Result<(), String>>,
}

impl InitialIndexGate {
    pub(super) fn pending() -> Self {
        Self {
            state: Mutex::new(InitialIndexState::default()),
            changed: Notify::new(),
        }
    }

    /// Registration updates the existing gate. Its path membership stays fixed.
    pub(super) async fn register_codebase(&self, id: String) {
        self.state.lock().await.codebase_id = Some(id);
        self.changed.notify_waiters();
    }

    pub(super) async fn wait(&self) -> Result<(), String> {
        self.wait_for_codebases(&[]).await
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

    pub(super) async fn finish(&self, result: Result<(), String>) {
        self.state.lock().await.result = Some(result);
        self.changed.notify_waiters();
    }
}

//! The explicit first index behind the `index_codebase` tool.
//!
//! The tool registers a checkout, hands it to the engine, and reports through
//! the checkout's first-index gate. Retrieval tools wait on that same gate.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::tool_types::IndexCodebaseArgs;
use super::{
    CallBudget, FailureKind, InitialIndexGate, McpServer, ToolError, canonical_directory,
    initial_gate_for_path,
};

impl McpServer {
    /// Index the checkout that `args` names, or the launch checkout.
    pub(super) async fn index_checkout(
        &self,
        args: IndexCodebaseArgs,
        budget: &CallBudget,
    ) -> Result<String, ToolError> {
        let requested = match args.path.as_deref() {
            Some(path) => PathBuf::from(path),
            None => self.dir().await.clone(),
        };
        let dir = canonical_directory("index_codebase", &self.shared.context.cwd, &requested)?;
        // The sync manifest represents the complete Git working copy. Use that
        // same root for consent/cache lookup, readiness gates, checkout headers,
        // and watching; otherwise indexing from `repo/src` records one path but
        // later launches from `repo` incorrectly look unindexed.
        let dir = crate::codebase::working_copy_root(&dir).await;

        // A concurrent or recent first-index call owns the gate. Await it
        // rather than queueing another full upload. A gate that FAILED is not
        // awaited: it falls through below, where the engine replaces it with a
        // fresh one, so a failed first index stays retryable while a partial
        // one can never be reported as ready.
        if let Some(gate) = initial_gate_for_path(&self.shared.leases, &dir).await
            && !matches!(gate.outcome().await, Some(Err(_)))
        {
            return first_index_report(gate.wait().await, &dir, None);
        }

        if let Some(started) = self.resume_recorded_index(&dir, budget).await? {
            return Ok(started);
        }
        self.start_first_index(&dir).await
    }

    /// Start the background sync and the watcher for a checkout that already
    /// has a recorded index. `None` when the path has no recorded index.
    async fn resume_recorded_index(
        &self,
        dir: &Path,
        budget: &CallBudget,
    ) -> Result<Option<String>, ToolError> {
        // Only this exact path's recorded index is prior permission. An umbrella
        // ancestor may serve read requests, but explicitly indexing the child is
        // a request for an independently writable codebase.
        let resolved = crate::codebase::resolve_exact(&self.base_for(budget), dir)
            .await
            .map_err(|e| {
                ToolError::from_client("index_codebase", &e.context(dir.display().to_string()))
            })?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        let client = self
            .shared
            .base
            .clone()
            .with_codebase(resolved.id.clone())
            .with_local_root(Some(dir.to_path_buf()));
        if !self.shared.pinned && self.dir().await == dir {
            *self.shared.bound.lock().await = Some(client.clone());
        }
        self.watch_once(client, dir.to_path_buf()).await;
        Ok(Some(format!(
            "codebase {} was already indexed; background sync and watching started\npath {}",
            resolved.id,
            dir.display()
        )))
    }

    /// Register the checkout as a new codebase and wait for its first index.
    async fn start_first_index(&self, dir: &Path) -> Result<String, ToolError> {
        // Claim the checkout and its gate before anything is registered on the
        // server. The coordinator waits for the codebase this call registers
        // instead of registering one of its own, so one first index creates one
        // codebase. The gate is published for this session's readiness checks
        // at the same time, so a concurrent path-scoped retrieval call already
        // has something to wait on.
        let first_index_client = self
            .shared
            .base
            .clone()
            .without_codebase()
            .with_local_root(Some(dir.to_path_buf()));
        let gate = self
            .watch_first_once(first_index_client, dir.to_path_buf())
            .await
            .map_err(|e| {
                ToolError::new(
                    "index_codebase",
                    FailureKind::Unavailable,
                    format!("{}: {e}", dir.display()),
                )
            })?;
        if !gate.claim_registration().await {
            return first_index_report(gate.wait().await, dir, None);
        }

        let id = register_codebase(&self.shared.base, &gate, dir).await?;
        let client = self
            .shared
            .base
            .clone()
            .with_codebase(id.clone())
            .with_local_root(Some(dir.to_path_buf()));
        if !self.shared.pinned && self.dir().await == dir {
            *self.shared.bound.lock().await = Some(client.clone());
        }
        first_index_report(gate.wait().await, dir, Some(&id))
    }
}

/// Register `dir` on the server and report the result to `gate`.
///
/// The registration runs in a task of its own. The call deadline can drop the
/// future of the tool while the request is in flight, and a gate that was
/// claimed but never finished would hold every later call on this path. The
/// task reports to the gate whatever happens to the call. It uses `base`
/// without a deadline, because it outlives the call.
async fn register_codebase(
    base: &crate::client::Client,
    gate: &Arc<InitialIndexGate>,
    dir: &Path,
) -> Result<String, ToolError> {
    let registration = tokio::spawn({
        let base = base.clone();
        let dir = dir.to_path_buf();
        let gate = Arc::clone(gate);
        async move {
            match crate::codebase::ensure(&base, &dir).await {
                Ok(id) => {
                    gate.register_codebase(id.clone()).await;
                    Ok(id)
                }
                Err(error) => {
                    gate.finish(Err(format!("{error:#}"))).await;
                    Err(error)
                }
            }
        }
    });
    match registration.await {
        Ok(Ok(id)) => Ok(id),
        Ok(Err(e)) => Err(ToolError::from_client(
            "index_codebase",
            &e.context(dir.display().to_string()),
        )),
        Err(stopped) => Err(ToolError::new(
            "index_codebase",
            FailureKind::Unavailable,
            format!("{}: registration stopped: {stopped}", dir.display()),
        )),
    }
}

/// The tool's answer for the result of a first index. `codebase` names the
/// codebase when this call registered it.
fn first_index_report(
    outcome: Result<(), String>,
    dir: &Path,
    codebase: Option<&str>,
) -> Result<String, ToolError> {
    let path = dir.display();
    match (outcome, codebase) {
        (Ok(()), None) => Ok(format!(
            "initial indexing complete\npath {path}\nretrieval tools are now available"
        )),
        (Ok(()), Some(id)) => Ok(format!(
            "initial indexing complete\ncodebase {id}\npath {path}\nretrieval tools are now available"
        )),
        (Err(e), None) => Err(ToolError::new(
            "index_codebase",
            FailureKind::IndexFailed,
            format!("initial indexing failed for {path}: {e}"),
        )),
        (Err(e), Some(id)) => Err(ToolError::new(
            "index_codebase",
            FailureKind::IndexFailed,
            format!("initial indexing failed for {path} (codebase {id}): {e}"),
        )),
    }
}

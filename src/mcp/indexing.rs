//! The explicit first index behind the `index_codebase` tool.
//!
//! The tool registers a checkout, hands it to the engine, and returns as soon
//! as the codebase exists. The first index then runs in the checkout's
//! coordinator, so no `index_codebase` call waits for it. Retrieval tools wait
//! on the checkout's first-index gate, and `sync_status` reports its phase.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::readiness::FirstIndexPhase;
use super::tool_types::IndexCodebaseArgs;
use super::{
    CallBudget, FailureKind, InitialIndexGate, McpServer, ToolError, canonical_directory,
    initial_gate_for_path,
};
use crate::client::Client;

/// What every report of a running first index ends with. The agent can follow
/// the first index through `sync_status`, and it can use its own tools until
/// retrieval serves the new index.
const FOLLOW_PROGRESS: &str = "retrieval tools for this path report that the first index is \
     still running until it completes\n\
     call sync_status every 10 to 15 seconds to follow progress, and use local Read/Grep \
     meanwhile";

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

        if let Some(report) = self.earlier_first_index(&dir).await {
            return report;
        }
        if let Some(started) = self.resume_recorded_index(&dir, budget).await? {
            return Ok(started);
        }
        self.start_first_index(&dir, ensure_codebase).await
    }

    /// The report of a first index that an earlier call already owns on `dir`,
    /// or `None` when this call must start one.
    ///
    /// A concurrent or recent first-index call owns the gate. Report it rather
    /// than queue another full upload, and never wait for it. A gate that
    /// FAILED is not reported: it falls through, where the engine replaces it
    /// with a fresh one, so a failed first index stays retryable while a
    /// partial one can never be reported as ready.
    async fn earlier_first_index(&self, dir: &Path) -> Option<Result<String, ToolError>> {
        let gate = initial_gate_for_path(&self.shared.leases, dir).await?;
        let phase = gate.phase(None).await;
        if matches!(phase, FirstIndexPhase::Failed { .. }) {
            return None;
        }
        Some(first_index_report(
            &phase,
            dir,
            gate.codebase().await.as_deref(),
        ))
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

    /// Register the checkout as a new codebase, and return once the codebase
    /// exists. The first index continues in the checkout's coordinator.
    ///
    /// `ensure` finds or creates the codebase of the checkout. It is a
    /// parameter so a test can register without a server.
    async fn start_first_index<F>(
        &self,
        dir: &Path,
        ensure: impl FnOnce(Client, PathBuf) -> F,
    ) -> Result<String, ToolError>
    where
        F: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
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
            // Another call registers this codebase and owns the first index.
            return first_index_report(
                &gate.phase(None).await,
                dir,
                gate.codebase().await.as_deref(),
            );
        }

        let registering = ensure(self.shared.base.clone(), dir.to_path_buf());
        let id = register_codebase(&gate, dir, registering).await?;
        let client = self
            .shared
            .base
            .clone()
            .with_codebase(id.clone())
            .with_local_root(Some(dir.to_path_buf()));
        if !self.shared.pinned && self.dir().await == dir {
            *self.shared.bound.lock().await = Some(client.clone());
        }
        Ok(running_report("first index started", dir, Some(&id)))
    }
}

/// Find or create the codebase of `dir`. `base` has no deadline, because the
/// registration outlives the call that asked for it.
async fn ensure_codebase(base: Client, dir: PathBuf) -> anyhow::Result<String> {
    crate::codebase::ensure(&base, &dir).await
}

/// Run `ensure` and report its result to `gate`.
///
/// The registration runs in a task of its own. The call deadline can drop the
/// future of the tool while the request is in flight, and a gate that was
/// claimed but never finished would hold every later call on this path. The
/// task reports to the gate whatever happens to the call.
async fn register_codebase(
    gate: &Arc<InitialIndexGate>,
    dir: &Path,
    ensure: impl Future<Output = anyhow::Result<String>> + Send + 'static,
) -> Result<String, ToolError> {
    let registration = tokio::spawn({
        let gate = Arc::clone(gate);
        async move {
            match ensure.await {
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

/// The `codebase <id>` line of a report, or nothing when the codebase is not
/// known.
fn codebase_line(codebase: Option<&str>) -> String {
    codebase.map_or_else(String::new, |id| format!("codebase {id}\n"))
}

/// The report of a first index that is running. `headline` says whether this
/// call started it or found it.
fn running_report(headline: &str, dir: &Path, codebase: Option<&str>) -> String {
    format!(
        "{headline}\n{}path {}\n{FOLLOW_PROGRESS}",
        codebase_line(codebase),
        dir.display()
    )
}

/// The tool's answer for a first index that another call owns. `codebase` names
/// the codebase when the gate knows it.
fn first_index_report(
    phase: &FirstIndexPhase,
    dir: &Path,
    codebase: Option<&str>,
) -> Result<String, ToolError> {
    match phase {
        FirstIndexPhase::Ready => Ok(format!(
            "first index complete\n{}path {}\nretrieval tools are available",
            codebase_line(codebase),
            dir.display()
        )),
        FirstIndexPhase::Failed { reason } => {
            let path = dir.display();
            let detail = match codebase {
                Some(id) => format!("first index failed for {path} (codebase {id}): {reason}"),
                None => format!("first index failed for {path}: {reason}"),
            };
            Err(ToolError::new(
                "index_codebase",
                FailureKind::IndexFailed,
                detail,
            ))
        }
        running => Ok(running_report(
            &format!("first index already in progress ({running})"),
            dir,
            codebase,
        )),
    }
}

#[cfg(test)]
mod tests;

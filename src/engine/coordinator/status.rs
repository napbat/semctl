//! One checkout's status, as `sync_status` and the daemon status see it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::WatcherState;
use crate::mcp::readiness::FirstIndexPhase;
use crate::sync::SyncProgress;

/// One checkout, as `sync_status` and the daemon status see it.
///
/// `semctl daemon status` reports one of these per checkout, so this is the
/// single source of truth for that part of the status line as well.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// The phase of this checkout's first index. `None` when the checkout has
    /// no first-index gate, and for a daemon that predates the field.
    #[serde(default)]
    pub(crate) first_index: Option<FirstIndexPhase>,
    /// The latest milestone of the reconcile that is running now. `None` when
    /// no reconcile reported one, and for a daemon that predates the field.
    ///
    /// This reuses [`SyncProgress`], the type the sync engine reports with,
    /// so the wire shape and the engine cannot drift apart.
    #[serde(default)]
    pub(crate) sync_progress: Option<SyncProgress>,
}

impl std::fmt::Display for CoordinatorStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "root {} codebase {} leases {} watcher {} job {} running {} sync {} first index {} pending {}{} outcome {} error {}",
            self.root.display(),
            self.codebase_id.as_deref().unwrap_or("(unbound)"),
            self.leases,
            match &self.watcher {
                WatcherState::Active => "active",
                WatcherState::Unavailable(reason) => reason,
            },
            self.last_job_id.as_deref().unwrap_or("(none)"),
            self.running,
            self.sync_progress
                .as_ref()
                .map_or_else(|| "(none)".to_string(), ToString::to_string),
            self.first_index
                .as_ref()
                .map_or_else(|| "(none)".to_string(), ToString::to_string),
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

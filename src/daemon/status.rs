//! What a daemon reports about itself.
//!
//! One shape serves both readers: the `status` control answer on the wire and
//! `semctl daemon status` on a terminal. The per-checkout and per-permit
//! facts are the engine's own types ([`CoordinatorStatus`],
//! [`SchedulerUsage`]), so the status line cannot drift from what the engine
//! reports to a session through `sync_status`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::VERSION;
use super::serve::Daemon;
use crate::engine::{CoordinatorStatus, SchedulerUsage};

/// One daemon, as its status answer describes it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonStatus {
    /// The daemon's build version.
    pub(crate) version: String,
    /// The daemon's process id.
    pub(crate) pid: u32,
    /// Seconds since the daemon won its election.
    pub(crate) uptime_secs: u64,
    /// MCP sessions this daemon serves right now.
    pub(crate) sessions: usize,
    /// How much of every scheduler permit class is free.
    pub(crate) permits: SchedulerUsage,
    /// One entry per checkout this daemon keeps in sync.
    pub(crate) coordinators: Vec<CoordinatorStatus>,
}

/// What a daemon answers a `stop` request with, before it drains.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StopAck {
    /// The process id that is stopping.
    pub(crate) pid: u32,
    /// The build version of the daemon that is stopping.
    pub(crate) version: String,
}

impl DaemonStatus {
    /// The same facts as one fact per line, with the checkouts indented.
    pub(crate) fn render(&self) -> String {
        let mut lines = vec![
            format!("version {}", self.version),
            format!("pid {}", self.pid),
            format!("uptime {} seconds", self.uptime_secs),
            format!("sessions {}", self.sessions),
            format!(
                "scan permits {}/{}",
                self.permits.scan.available, self.permits.scan.total
            ),
            format!(
                "upload permits {}/{}",
                self.permits.upload.available, self.permits.upload.total
            ),
            format!(
                "remote permits {}/{}",
                self.permits.remote.available, self.permits.remote.total
            ),
            format!("checkouts {}", self.coordinators.len()),
        ];
        for coordinator in &self.coordinators {
            lines.push(format!("  {coordinator}"));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

/// Read one daemon's status.
///
/// Every coordinator is asked in turn, with no lock held across the loop: the
/// registry hands out cloned handles for exactly this reason. The result is a
/// report, not a decision, so a checkout that changes while the report is
/// built is not a problem.
pub(super) async fn snapshot(daemon: &Arc<Daemon>) -> DaemonStatus {
    let engine = daemon.engine();
    let mut coordinators = Vec::new();
    for coordinator in engine.registry().coordinators() {
        coordinators.push(coordinator.status().await);
    }
    // One stable order, so two readings of one daemon are comparable.
    coordinators.sort_by(|left, right| left.root.cmp(&right.root));
    DaemonStatus {
        version: VERSION.to_string(),
        pid: daemon.pid(),
        uptime_secs: daemon.uptime_secs(),
        sessions: daemon.sessions().session_count(),
        permits: engine.scheduler().usage(),
        coordinators,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DaemonStatus, StopAck};
    use crate::engine::CoordinatorStatus;
    use crate::engine::coordinator::WatcherState;
    use crate::engine::scheduler::{PermitUsage, SchedulerUsage};

    fn status() -> DaemonStatus {
        DaemonStatus {
            version: "0.2.0".to_string(),
            pid: 4242,
            uptime_secs: 12,
            sessions: 3,
            permits: SchedulerUsage {
                scan: PermitUsage {
                    available: 7,
                    total: 8,
                },
                upload: PermitUsage {
                    available: 8,
                    total: 8,
                },
                remote: PermitUsage {
                    available: 63,
                    total: 64,
                },
            },
            coordinators: vec![CoordinatorStatus {
                root: PathBuf::from("/work/checkout"),
                codebase_id: Some("codebase-1".to_string()),
                leases: 2,
                watcher: WatcherState::Active,
                last_job_id: Some("job-1".to_string()),
                running: true,
                pending_triggers: 1,
                trigger_overflow: false,
                last_outcome: Some("uploaded 3 files".to_string()),
                last_error: None,
            }],
        }
    }

    #[test]
    fn the_status_shape_survives_the_wire() {
        let line = serde_json::to_string(&status()).expect("serialize the status");
        let decoded: DaemonStatus = serde_json::from_str(&line).expect("decode the status");

        assert_eq!(decoded.version, "0.2.0");
        assert_eq!(decoded.pid, 4242);
        assert_eq!(decoded.uptime_secs, 12);
        assert_eq!(decoded.sessions, 3);
        assert_eq!(decoded.permits.scan.available, 7);
        assert_eq!(decoded.permits.remote.total, 64);
        assert_eq!(decoded.coordinators.len(), 1);
        let coordinator = &decoded.coordinators[0];
        assert_eq!(coordinator.root, PathBuf::from("/work/checkout"));
        assert_eq!(coordinator.codebase_id.as_deref(), Some("codebase-1"));
        assert_eq!(coordinator.leases, 2);
        assert_eq!(coordinator.watcher, WatcherState::Active);
        assert_eq!(coordinator.last_job_id.as_deref(), Some("job-1"));
        assert!(coordinator.running);
        assert_eq!(coordinator.pending_triggers, 1);
        assert!(!coordinator.trigger_overflow);
        assert_eq!(
            coordinator.last_outcome.as_deref(),
            Some("uploaded 3 files")
        );
        assert_eq!(coordinator.last_error, None);
    }

    /// A reader that is not this build must find the documented field names.
    #[test]
    fn the_status_line_uses_the_documented_field_names() {
        let value = serde_json::to_value(status()).expect("serialize the status");

        for field in [
            "version",
            "pid",
            "uptime_secs",
            "sessions",
            "permits",
            "coordinators",
        ] {
            assert!(value.get(field).is_some(), "{field} is missing: {value}");
        }
        assert_eq!(value["permits"]["scan"]["total"], 8);
        assert_eq!(value["coordinators"][0]["leases"], 2);
        assert_eq!(value["coordinators"][0]["watcher"], "active");
    }

    #[test]
    fn the_text_form_reports_one_fact_per_line_with_indented_checkouts() {
        let rendered = status().render();
        let lines: Vec<&str> = rendered.lines().collect();

        assert_eq!(
            lines[..8],
            [
                "version 0.2.0",
                "pid 4242",
                "uptime 12 seconds",
                "sessions 3",
                "scan permits 7/8",
                "upload permits 8/8",
                "remote permits 63/64",
                "checkouts 1",
            ]
        );
        assert!(lines[8].starts_with("  root /work/checkout"), "{rendered}");
    }

    #[test]
    fn the_stop_acknowledgement_survives_the_wire() {
        let line = serde_json::to_string(&StopAck {
            pid: 7,
            version: "0.2.0".to_string(),
        })
        .expect("serialize the acknowledgement");
        let decoded: StopAck = serde_json::from_str(&line).expect("decode the acknowledgement");

        assert_eq!(decoded.pid, 7);
        assert_eq!(decoded.version, "0.2.0");
    }
}

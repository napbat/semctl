//! Process-wide bounds on the work one engine schedules.
//!
//! One process can serve many sessions over many checkouts. Without a bound,
//! 1,000 checkouts would open as many upload requests as they have files and as
//! many interactive requests as their sessions ask for. The permits here are
//! the bound: every upload request and every interactive remote request takes
//! one.
//!
//! The permits are counted, not queued per checkout. A checkout that waits for
//! a permit stays responsive, because waiting happens in its own task.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Concurrent full-tree scans across every coordinator.
const SCAN_PERMITS_VAR: &str = "SEMCTX_DAEMON_SCAN_PERMITS";
/// Concurrent upload requests across every coordinator.
const UPLOAD_PERMITS_VAR: &str = "SEMCTX_DAEMON_UPLOAD_PERMITS";
/// Concurrent interactive remote requests across every session.
const REMOTE_PERMITS_VAR: &str = "SEMCTX_DAEMON_REMOTE_PERMITS";

/// An override below this cannot make progress, and one above it is not a
/// bound. Both ends are clamped rather than rejected: a permit count is a
/// resource hint, and a typo must not stop the process from serving.
const PERMIT_RANGE: std::ops::RangeInclusive<usize> = 1..=1024;

/// A scan reads and hashes every candidate file, so half the cores keep the
/// machine usable while several checkouts settle at once.
const SCAN_RANGE: std::ops::RangeInclusive<usize> = 2..=8;
const DEFAULT_UPLOAD_PERMITS: usize = 8;
const DEFAULT_REMOTE_PERMITS: usize = 64;

/// How many permits each class gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SchedulerSettings {
    scan: usize,
    upload: usize,
    remote: usize,
}

impl SchedulerSettings {
    /// Read the overrides from the process environment.
    ///
    /// This is the only place that reads them. The values are process-level,
    /// not session-level: they bound the whole engine, so a session must not be
    /// able to raise them.
    pub(crate) fn from_environment() -> Self {
        let parallelism = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        Self::resolve(
            std::env::var(SCAN_PERMITS_VAR).ok().as_deref(),
            std::env::var(UPLOAD_PERMITS_VAR).ok().as_deref(),
            std::env::var(REMOTE_PERMITS_VAR).ok().as_deref(),
            parallelism,
        )
    }

    /// The pure mapping behind [`Self::from_environment`].
    fn resolve(
        scan: Option<&str>,
        upload: Option<&str>,
        remote: Option<&str>,
        parallelism: usize,
    ) -> Self {
        Self {
            scan: permits(
                scan,
                (parallelism / 2).clamp(*SCAN_RANGE.start(), *SCAN_RANGE.end()),
            ),
            upload: permits(upload, DEFAULT_UPLOAD_PERMITS),
            remote: permits(remote, DEFAULT_REMOTE_PERMITS),
        }
    }
}

/// An absent or unreadable override keeps the default, which is the same rule
/// the session settings use: a typo must not change a bound silently.
fn permits(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(*PERMIT_RANGE.start(), *PERMIT_RANGE.end())
}

/// How much of one permit class is free.
///
/// `semctl daemon status` reports this, so it is also a wire type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PermitUsage {
    /// Permits no holder has taken.
    pub(crate) available: usize,
    /// Permits this class was built with.
    pub(crate) total: usize,
}

/// How much of every permit class is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SchedulerUsage {
    pub(crate) scan: PermitUsage,
    pub(crate) upload: PermitUsage,
    pub(crate) remote: PermitUsage,
}

/// The permits every checkout and every session competes for.
///
/// The handles are `Arc` because a permit outlives the call that took it: an
/// upload permit travels into an upload task, and a remote permit is held for
/// one request attempt.
pub(crate) struct Scheduler {
    scan: Arc<Semaphore>,
    upload: Arc<Semaphore>,
    remote: Arc<Semaphore>,
    /// What each class was built with. A semaphore reports what is available,
    /// not what it started with, and the status line needs both.
    settings: SchedulerSettings,
}

impl Scheduler {
    pub(crate) fn new(settings: SchedulerSettings) -> Self {
        Self {
            scan: Arc::new(Semaphore::new(settings.scan)),
            upload: Arc::new(Semaphore::new(settings.upload)),
            remote: Arc::new(Semaphore::new(settings.remote)),
            settings,
        }
    }

    /// A scheduler with the settings this process was started with.
    pub(crate) fn from_environment() -> Self {
        Self::new(SchedulerSettings::from_environment())
    }

    /// The handle a coordinator acquires one permit from per reconcile.
    pub(crate) fn scan_permits(&self) -> Arc<Semaphore> {
        self.scan.clone()
    }

    /// The handle a sync acquires one permit from per upload request.
    pub(crate) fn upload_permits(&self) -> Arc<Semaphore> {
        self.upload.clone()
    }

    /// The handle a [`crate::client::Client`] acquires one permit from per
    /// request attempt.
    pub(crate) fn remote_permits(&self) -> Arc<Semaphore> {
        self.remote.clone()
    }

    /// How much of every class is free right now.
    ///
    /// The three readings are taken one after another, so the snapshot is not
    /// one instant of the whole scheduler. It is a report, not a decision.
    pub(crate) fn usage(&self) -> SchedulerUsage {
        SchedulerUsage {
            scan: PermitUsage {
                available: self.scan.available_permits(),
                total: self.settings.scan,
            },
            upload: PermitUsage {
                available: self.upload.available_permits(),
                total: self.settings.upload,
            },
            remote: PermitUsage {
                available: self.remote.available_permits(),
                total: self.settings.remote,
            },
        }
    }
}

/// Take one permit, or `None` when the semaphore was closed.
///
/// Nothing in this program closes a scheduler semaphore, so `None` cannot
/// happen. It is returned instead of panicking because a lost bound is a
/// resource-policy failure, not a correctness failure: the caller then does its
/// work unbounded rather than refusing a user's request.
pub(crate) async fn permit(permits: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    permits.clone().acquire_owned().await.ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DEFAULT_REMOTE_PERMITS, DEFAULT_UPLOAD_PERMITS, Scheduler, SchedulerSettings};

    #[test]
    fn defaults_follow_the_documented_table() {
        let settings = SchedulerSettings::resolve(None, None, None, 16);

        assert_eq!(settings.scan, 8, "half of 16 cores, capped at 8");
        assert_eq!(settings.upload, DEFAULT_UPLOAD_PERMITS);
        assert_eq!(settings.remote, DEFAULT_REMOTE_PERMITS);
    }

    #[test]
    fn a_small_machine_still_scans_two_checkouts_at_once() {
        assert_eq!(SchedulerSettings::resolve(None, None, None, 1).scan, 2);
        assert_eq!(SchedulerSettings::resolve(None, None, None, 2).scan, 2);
        assert_eq!(SchedulerSettings::resolve(None, None, None, 6).scan, 3);
    }

    #[test]
    fn overrides_are_clamped_into_the_supported_range() {
        let settings = SchedulerSettings::resolve(Some("0"), Some("99999"), Some(" 12 "), 4);

        assert_eq!(settings.scan, 1, "zero permits cannot make progress");
        assert_eq!(settings.upload, 1024, "an override cannot remove the bound");
        assert_eq!(settings.remote, 12);
    }

    #[test]
    fn an_unreadable_override_keeps_the_default() {
        let settings = SchedulerSettings::resolve(Some(""), Some("lots"), Some("-4"), 4);

        assert_eq!(settings.scan, 2);
        assert_eq!(settings.upload, DEFAULT_UPLOAD_PERMITS);
        assert_eq!(settings.remote, DEFAULT_REMOTE_PERMITS);
    }

    /// The bound is real: the second holder waits until the first releases.
    #[tokio::test]
    async fn a_permit_blocks_while_the_class_is_exhausted() {
        let scheduler = Scheduler::new(SchedulerSettings::resolve(
            Some("1"),
            Some("1"),
            Some("1"),
            4,
        ));
        let uploads = scheduler.upload_permits();
        let held = super::permit(&uploads).await.expect("first upload permit");

        assert!(
            tokio::time::timeout(Duration::from_millis(5), super::permit(&uploads))
                .await
                .is_err(),
            "an exhausted class must make the next caller wait"
        );

        drop(held);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), super::permit(&uploads))
                .await
                .is_ok_and(|permit| permit.is_some()),
            "releasing a permit must admit the waiter"
        );
    }

    /// The status line reports what is free and what the class started with.
    #[tokio::test]
    async fn the_usage_snapshot_reports_available_and_total_permits() {
        let scheduler = Scheduler::new(SchedulerSettings::resolve(
            Some("2"),
            Some("3"),
            Some("4"),
            4,
        ));
        let uploads = scheduler.upload_permits();
        let _held = super::permit(&uploads).await.expect("upload permit");

        let usage = scheduler.usage();
        assert_eq!((usage.scan.available, usage.scan.total), (2, 2));
        assert_eq!(
            (usage.upload.available, usage.upload.total),
            (2, 3),
            "a held permit must be missing from the available count"
        );
        assert_eq!((usage.remote.available, usage.remote.total), (4, 4));
    }

    /// Each class is counted on its own. An exhausted upload class must not
    /// stop a scan or an interactive request.
    #[tokio::test]
    async fn the_classes_do_not_share_permits() {
        let scheduler = Scheduler::new(SchedulerSettings::resolve(
            Some("1"),
            Some("1"),
            Some("1"),
            4,
        ));
        let uploads = scheduler.upload_permits();
        let _held = super::permit(&uploads).await.expect("upload permit");

        for permits in [scheduler.scan_permits(), scheduler.remote_permits()] {
            assert!(
                super::permit(&permits).await.is_some(),
                "an unrelated class must still admit a permit"
            );
        }
    }
}

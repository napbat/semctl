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

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Concurrent upload requests across every coordinator.
const UPLOAD_PERMITS_VAR: &str = "SEMCTX_DAEMON_UPLOAD_PERMITS";
/// Concurrent interactive remote requests across every session.
const REMOTE_PERMITS_VAR: &str = "SEMCTX_DAEMON_REMOTE_PERMITS";

/// An override below this cannot make progress, and one above it is not a
/// bound. Both ends are clamped rather than rejected: a permit count is a
/// resource hint, and a typo must not stop the process from serving.
const PERMIT_RANGE: std::ops::RangeInclusive<usize> = 1..=1024;

const DEFAULT_UPLOAD_PERMITS: usize = 8;
const DEFAULT_REMOTE_PERMITS: usize = 64;

/// How many permits each class gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SchedulerSettings {
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
        Self::resolve(
            std::env::var(UPLOAD_PERMITS_VAR).ok().as_deref(),
            std::env::var(REMOTE_PERMITS_VAR).ok().as_deref(),
        )
    }

    /// The pure mapping behind [`Self::from_environment`].
    fn resolve(upload: Option<&str>, remote: Option<&str>) -> Self {
        Self {
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

/// The permits every checkout and every session competes for.
///
/// The handles are `Arc` because a permit outlives the call that took it: an
/// upload permit travels into an upload task, and a remote permit is held for
/// one request attempt.
pub(crate) struct Scheduler {
    upload: Arc<Semaphore>,
    remote: Arc<Semaphore>,
}

impl Scheduler {
    pub(crate) fn new(settings: SchedulerSettings) -> Self {
        Self {
            upload: Arc::new(Semaphore::new(settings.upload)),
            remote: Arc::new(Semaphore::new(settings.remote)),
        }
    }

    /// A scheduler with the settings this process was started with.
    pub(crate) fn from_environment() -> Self {
        Self::new(SchedulerSettings::from_environment())
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
        let settings = SchedulerSettings::resolve(None, None);

        assert_eq!(settings.upload, DEFAULT_UPLOAD_PERMITS);
        assert_eq!(settings.remote, DEFAULT_REMOTE_PERMITS);
    }

    #[test]
    fn overrides_are_clamped_into_the_supported_range() {
        let settings = SchedulerSettings::resolve(Some("99999"), Some(" 12 "));

        assert_eq!(settings.upload, 1024, "an override cannot remove the bound");
        assert_eq!(settings.remote, 12);

        assert_eq!(
            SchedulerSettings::resolve(Some("0"), None).upload,
            1,
            "zero permits cannot make progress"
        );
    }

    #[test]
    fn an_unreadable_override_keeps_the_default() {
        let settings = SchedulerSettings::resolve(Some("lots"), Some("-4"));

        assert_eq!(settings.upload, DEFAULT_UPLOAD_PERMITS);
        assert_eq!(settings.remote, DEFAULT_REMOTE_PERMITS);
    }

    /// The bound is real: the second holder waits until the first releases.
    #[tokio::test]
    async fn a_permit_blocks_while_the_class_is_exhausted() {
        let scheduler = Scheduler::new(SchedulerSettings::resolve(Some("1"), Some("1")));
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

    /// Each class is counted on its own. An exhausted upload class must not
    /// stop an interactive request.
    #[tokio::test]
    async fn the_classes_do_not_share_permits() {
        let scheduler = Scheduler::new(SchedulerSettings::resolve(Some("1"), Some("1")));
        let uploads = scheduler.upload_permits();
        let _held = super::permit(&uploads).await.expect("upload permit");
        let remote = scheduler.remote_permits();

        assert!(
            super::permit(&remote).await.is_some(),
            "an unrelated class must still admit a permit"
        );
    }
}

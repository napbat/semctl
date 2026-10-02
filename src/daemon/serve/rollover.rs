//! Roll an idle daemon over to a replaced executable.
//!
//! An upgrade replaces the `semctl` file on disk. A running daemon keeps the
//! old program in memory, and nothing in its own state changes. The endpoint
//! identity includes the version, so a client of the new version already
//! starts a new daemon on another endpoint. The old daemon would then wait for
//! the full idle delay after its last session. A client of the same version,
//! such as a local rebuild, would keep attaching to the old program.
//!
//! The daemon therefore keeps the identity of its executable file from the
//! start and compares the file at the same path with it at a fixed interval.
//! After the path names another file, the daemon drains as soon as no session
//! is attached and no connection is open. It never ends a live session for an
//! upgrade. The next client starts a daemon from the new executable.
//!
//! `SEMCTX_DAEMON_AUTO_UPDATE=0` turns the check off. Like the idle delay,
//! the daemon reads it from its own environment, which it inherits from the
//! client that started it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use same_file::Handle;
use tracing::{info, warn};

use super::Sessions;

/// Set to `0` to keep a daemon running after its executable is replaced.
const AUTO_UPDATE_VAR: &str = "SEMCTX_DAEMON_AUTO_UPDATE";

/// How often the daemon compares its executable path with the file it started
/// from. One open per interval costs nothing measurable, and the interval
/// bounds how long an idle daemon outlives an upgrade.
const CHECK_EVERY: Duration = Duration::from_secs(30);

/// Only the exact value `0` turns the roll-over off, which is the rule of
/// every other on-by-default switch in this program.
fn enabled(raw: Option<&str>) -> bool {
    raw != Some("0")
}

/// The executable this daemon runs from, and that file's identity.
///
/// The identity is the device and inode on Unix and the volume serial number
/// and file index on Windows. An installer replaces the file at the path, so
/// the path then names another file. Timestamps cannot show that reliably:
/// an installer can preserve them, and NTFS gives a file moved into a name
/// that was just vacated the creation time of the file that left it.
///
/// The handle stays open for the life of the daemon. The standard library
/// opens files with every share mode on Windows, so the handle never blocks
/// the rename that replaces the executable.
pub(super) struct Rollover {
    path: PathBuf,
    original: Handle,
    every: Duration,
}

impl Rollover {
    /// Watch this process's executable, unless the environment turns the
    /// roll-over off.
    ///
    /// `None` also means that the executable cannot be located or read. That
    /// daemon keeps the idle exit as its only exit for an unused daemon, which
    /// is the behavior before this check existed.
    pub(super) async fn from_environment() -> Option<Self> {
        if !enabled(std::env::var(AUTO_UPDATE_VAR).ok().as_deref()) {
            info!("{AUTO_UPDATE_VAR}=0; this daemon keeps running after an upgrade");
            return None;
        }
        let path = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                warn!(%error, "cannot locate this executable; an upgrade will not roll this daemon over");
                return None;
            }
        };
        Self::watch(path, CHECK_EVERY).await
    }

    async fn watch(path: PathBuf, every: Duration) -> Option<Self> {
        match open(path.clone()).await {
            Ok(original) => Some(Self {
                path,
                original,
                every,
            }),
            Err(error) => {
                warn!(
                    %error,
                    path = %path.display(),
                    "cannot read this executable; an upgrade will not roll this daemon over"
                );
                None
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file at the executable path is no longer the one this
    /// daemon started from.
    ///
    /// A file that cannot be read counts as replaced. It was removed, or a
    /// replacement is in progress. Either way this build is no longer the
    /// installed one, and the worst outcome is that an idle daemon exits and
    /// the next client starts another.
    async fn replaced(&self) -> bool {
        match open(self.path.clone()).await {
            Ok(current) => current != self.original,
            Err(_) => true,
        }
    }
}

/// Open `path` for its identity on the blocking pool, so a slow file system
/// never holds a runtime worker.
async fn open(path: PathBuf) -> std::io::Result<Handle> {
    tokio::task::spawn_blocking(move || Handle::from_path(path))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)))
}

/// Resolve when the executable was replaced and nothing is attached.
///
/// Without a [`Rollover`] this never resolves. The caller pins one future for
/// the whole accept loop, so a detected replacement is never forgotten.
pub(super) async fn superseded(rollover: Option<Rollover>, sessions: &Sessions) {
    let Some(rollover) = rollover else {
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(rollover.every).await;
        if rollover.replaced().await {
            break;
        }
    }
    info!(
        path = %rollover.path().display(),
        sessions = sessions.session_count(),
        "the semctl executable was replaced; this daemon exits when its last session ends"
    );
    sessions.wait_idle_for(Duration::ZERO).await;
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Rollover, Sessions, enabled, superseded};

    const EVERY: Duration = Duration::from_millis(10);

    /// Replace `path` the way an installer does: write a sibling, then rename
    /// it over the original.
    fn install(path: &Path, contents: &[u8]) {
        let staged = path.with_extension("staged");
        std::fs::write(&staged, contents).expect("stage the new executable");
        std::fs::rename(&staged, path).expect("replace the executable");
    }

    #[test]
    fn only_zero_turns_the_roll_over_off() {
        assert!(!enabled(Some("0")));
        assert!(enabled(None));
        assert!(enabled(Some("1")));
        assert!(enabled(Some("")));
        assert!(enabled(Some("off")));
    }

    #[tokio::test]
    async fn a_replaced_or_removed_executable_is_detected_and_an_untouched_one_is_not() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("semctl");
        std::fs::write(&path, b"old build").expect("write the executable");
        let rollover = Rollover::watch(path.clone(), EVERY)
            .await
            .expect("a readable executable");
        assert!(!rollover.replaced().await);

        // The same bytes: only the file identity tells the two apart.
        install(&path, b"old build");
        assert!(rollover.replaced().await);

        std::fs::remove_file(&path).expect("remove the executable");
        assert!(rollover.replaced().await);
    }

    #[tokio::test]
    async fn an_unreadable_executable_disables_the_roll_over() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert!(
            Rollover::watch(directory.path().join("missing"), EVERY)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_replaced_daemon_waits_for_its_last_session() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("semctl");
        std::fs::write(&path, b"old build").expect("write the executable");
        let rollover = Rollover::watch(path.clone(), EVERY).await;
        let sessions = Arc::new(Sessions::new());
        let session = Sessions::attach(sessions.clone(), 1);

        let waiter = tokio::spawn({
            let sessions = sessions.clone();
            async move { superseded(rollover, &sessions).await }
        });
        install(&path, b"old build");
        tokio::time::sleep(EVERY * 20).await;
        assert!(
            !waiter.is_finished(),
            "an attached session keeps a replaced daemon serving"
        );

        drop(session);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the daemon rolls over once its last session ends")
            .expect("the waiter task");
    }
}

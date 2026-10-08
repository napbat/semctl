//! Start a daemon from a client, detached from this process.
//!
//! The client does not elect. It starts a daemon and connects again, and the
//! election inside that daemon decides whether it serves or exits at once.
//! Several clients may therefore start several daemons for one endpoint; all
//! but one exit immediately.
//!
//! The client retains the child while it connects, so rejected startup can
//! reach the standalone fallback promptly. A winning daemon outlives the
//! client that started it and must not be waited for after connection.
//!
//! # What the daemon inherits
//!
//! The daemon inherits the environment of the client that started it, minus
//! the six per-session variables in [`PER_SESSION_VARS`]. Every later session
//! of that daemon therefore runs with the first client's environment, not with
//! its own. That applies to the proxy settings (`HTTP_PROXY`, `HTTPS_PROXY`,
//! `NO_PROXY`), to `PATH`, which decides which formatter an edit runs, and to
//! the Git configuration variables (`GIT_CONFIG_GLOBAL`, `GIT_DIR`, and the
//! rest), which decide which rules a source policy reads.
//!
//! `semctl daemon stop` is the way to change that: the next client to run
//! `semctl mcp` starts a daemon with its own environment.
//!
//! On Windows the daemon must inherit no handle to this client's standard
//! streams. Process creation duplicates every inheritable handle, and an MCP
//! host's pipes usually arrive inheritable, so an unshielded spawn left the
//! daemon holding the write end of this client's stdout — and the host
//! waiting for an end of file that came only when the daemon exited. See
//! `shield_standard_handles`.
//!
//! The daemon's working directory is not inherited. A client is invoked inside
//! a checkout, and a daemon that kept that directory would hold it open for
//! its whole life and would resolve a relative path of one session against
//! another session's checkout. Every session carries its own working directory
//! in its attach body, so the daemon needs none of its own.

use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use tracing::info;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
};

use crate::ipc::Endpoint;
use crate::session::PER_SESSION_VARS;

/// The subcommand that runs the daemon role.
const DAEMON_ARGS: [&str; 2] = ["daemon", "run"];

/// Start a daemon for `endpoint`.
///
/// The daemon inherits this client's environment except the per-session
/// variables: a daemon serves many sessions, so none of its own environment
/// may describe one. Standard input and output are the null device, and
/// standard error appends to the endpoint's log file, which is where the
/// daemon's `tracing` output goes.
///
/// The working directory is the endpoint's own, which
/// [`Endpoint::open_log`] has just created, and never this client's checkout.
pub(crate) fn spawn_daemon(endpoint: &Endpoint) -> Result<Child> {
    let program = std::env::current_exe().context("locate this executable")?;
    let log = endpoint.open_log()?;
    let mut command = command_for(&program, endpoint.daemon_dir());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    detach(&mut command);
    #[cfg(windows)]
    shield_standard_handles();
    let child = command.spawn().context("start a detached semctl daemon")?;
    info!(
        pid = child.id(),
        endpoint = endpoint.id(),
        log = %endpoint.log_path().display(),
        "started a semctl daemon for this endpoint"
    );
    Ok(child)
}

/// The command that runs a daemon, without its platform detachment.
///
/// Pure: it builds the command and reads nothing. The per-session variables
/// and the working directory are decided here, which is the rule this function
/// exists to make testable.
fn command_for(program: &Path, working_dir: Option<&Path>) -> Command {
    let mut command = Command::new(program);
    command.args(DAEMON_ARGS);
    #[cfg(windows)]
    command.arg("--require-detached");
    for name in PER_SESSION_VARS {
        command.env_remove(name);
    }
    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }
    command
}

/// Detach the daemon from this client's process group.
///
/// A daemon outlives the client that started it. Without this, a signal sent
/// to the client's group — what a shell sends on an interrupt — would also
/// reach the daemon and every other client's session with it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

/// Detach the daemon from this client's console and job object.
///
/// `CREATE_NO_WINDOW` starts the daemon without a console,
/// `CREATE_NEW_PROCESS_GROUP` keeps console control events from reaching it,
/// and `CREATE_BREAKAWAY_FROM_JOB` requests release from the client's jobs.
/// A restrictive ancestor can retain a child even when creation succeeds.
/// The child therefore verifies detachment before it binds its endpoint.
/// Do not combine `CREATE_NO_WINDOW` with
/// `DETACHED_PROCESS`: Windows ignores the former when the latter is set.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATION_FLAGS | CREATE_BREAKAWAY_FROM_JOB);
}

/// The creation flags that always apply.
#[cfg(windows)]
const CREATION_FLAGS: u32 = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;

/// Keep this client's standard handles out of the daemon.
///
/// The daemon is created with handle inheritance enabled — that is how the
/// three standard handles configured above reach it — and with it every
/// *other* inheritable handle of this process is duplicated into the daemon
/// too. An MCP host usually creates this client's standard pipes inheritable,
/// so a cold-spawned daemon would hold the write end of this client's stdout
/// for its whole life, and a host that waits for end of file on that pipe
/// would wait long after this client exited.
///
/// Clearing the inherit flag on this process's own standard handles closes
/// that path. The flag controls inheritance only: this process keeps using
/// the handles, and the standard library duplicates a handle itself when a
/// later child is asked to inherit a standard stream.
///
/// Best effort: a handle that cannot be adjusted leaves the old behavior for
/// that one stream, and the daemon still starts.
#[cfg(windows)]
fn shield_standard_handles() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: the call takes one selector and returns a handle, a null
        // handle, or `INVALID_HANDLE_VALUE`; it reads nothing else.
        let handle = unsafe { GetStdHandle(stream) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is a live standard handle of this process, and the
        // call only clears its inherit flag.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            tracing::debug!(
                stream,
                error = %std::io::Error::last_os_error(),
                "could not clear the inherit flag on a standard handle"
            );
        }
    }
}

/// Reject an automatically started daemon that still belongs to a Windows job.
///
/// A successful breakaway can leave a process in a restrictive ancestor job.
/// Run this check in the child before endpoint publication, not in the parent
/// after spawning: another client could attach before a parent-side check.
#[cfg(windows)]
pub(crate) fn ensure_detached() -> Result<()> {
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut in_job = 0;
    // SAFETY: the pseudo-handle names this process. A null job tests all
    // job membership, and the output points to a live BOOL-sized value.
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &raw mut in_job) } == 0 {
        return Err(std::io::Error::last_os_error()).context("check daemon job membership");
    }
    anyhow::ensure!(
        in_job == 0,
        "cannot serve a shared daemon inside a Windows job"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::{DAEMON_ARGS, command_for};
    use crate::session::PER_SESSION_VARS;

    #[test]
    fn the_daemon_is_started_with_the_run_subcommand() {
        let command = command_for(Path::new("/opt/semctl/bin/semctl"), None);

        assert_eq!(command.get_program(), OsStr::new("/opt/semctl/bin/semctl"));
        let args: Vec<&OsStr> = command.get_args().collect();
        let mut expected = DAEMON_ARGS.map(OsStr::new).to_vec();
        if cfg!(windows) {
            expected.push(OsStr::new("--require-detached"));
        }
        assert_eq!(args, expected);
        assert_eq!(command.get_current_dir(), None);
    }

    /// A daemon must not keep the checkout its first client was invoked in:
    /// it would hold that directory open and resolve another session's
    /// relative path against it.
    #[test]
    fn the_daemon_starts_in_the_endpoints_own_directory() {
        let endpoint_dir = Path::new("/run/user/1000/semctl");
        let command = command_for(Path::new("semctl"), Some(endpoint_dir));

        assert_eq!(command.get_current_dir(), Some(endpoint_dir));
    }

    /// The daemon must not be able to describe a session with its own
    /// environment. These six, and only these six, are removed.
    #[test]
    fn the_daemon_environment_loses_exactly_the_per_session_variables() {
        let command = command_for(Path::new("semctl"), None);

        let mut removed: Vec<String> = Vec::new();
        for (name, value) in command.get_envs() {
            assert_eq!(
                value, None,
                "{name:?} is set rather than removed; a daemon must inherit, not be told"
            );
            removed.push(name.to_string_lossy().into_owned());
        }
        removed.sort();
        let mut expected: Vec<String> = PER_SESSION_VARS
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        expected.sort();
        assert_eq!(removed, expected);
    }
}

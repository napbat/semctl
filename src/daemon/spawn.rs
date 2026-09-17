//! Start a daemon from a client, detached from this process.
//!
//! The client does not elect. It starts a daemon and connects again, and the
//! election inside that daemon decides whether it serves or exits at once.
//! Several clients may therefore start several daemons for one endpoint; all
//! but one exit immediately.
//!
//! Nothing here waits on the child. A losing daemon exits right away and stays
//! a zombie until this client exits, which is at most one short-lived entry per
//! spawn; a winning daemon outlives the client that started it and must not be
//! waited for at all.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use tracing::info;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
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
pub(crate) fn spawn_daemon(endpoint: &Endpoint) -> Result<()> {
    let program = std::env::current_exe().context("locate this executable")?;
    let log = endpoint.open_log()?;
    let mut command = command_for(&program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    detach(&mut command);
    let pid = start(command)?;
    info!(
        pid,
        endpoint = endpoint.id(),
        log = %endpoint.log_path().display(),
        "started a semctl daemon for this endpoint"
    );
    Ok(())
}

/// The command that runs a daemon, without its platform detachment.
///
/// Pure: it builds the command and reads nothing. The per-session variables
/// are removed here, which is the rule this function exists to make testable.
fn command_for(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.args(DAEMON_ARGS);
    for name in PER_SESSION_VARS {
        command.env_remove(name);
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
/// `CREATE_NO_WINDOW` and `DETACHED_PROCESS` keep the daemon off this
/// client's console, `CREATE_NEW_PROCESS_GROUP` keeps console control events
/// from reaching it, and `CREATE_BREAKAWAY_FROM_JOB` keeps it out of a job
/// object that would end it with this client.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATION_FLAGS | CREATE_BREAKAWAY_FROM_JOB);
}

/// The creation flags that always apply.
#[cfg(windows)]
const CREATION_FLAGS: u32 = CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;

/// Start the daemon and report its process id.
#[cfg(unix)]
fn start(mut command: Command) -> Result<u32> {
    let child = command.spawn().context("start a semctl daemon")?;
    Ok(child.id())
}

/// Start the daemon and report its process id.
///
/// A job object may forbid breakaway, and a process that asks for it anyway
/// cannot be created. The retry then accepts the job's lifetime: a daemon
/// inside the client's job is better than no daemon at all.
#[cfg(windows)]
fn start(mut command: Command) -> Result<u32> {
    use std::os::windows::process::CommandExt;

    use tracing::debug;

    match command.spawn() {
        Ok(child) => Ok(child.id()),
        Err(error) => {
            debug!(
                %error,
                "retrying the daemon start without CREATE_BREAKAWAY_FROM_JOB"
            );
            command.creation_flags(CREATION_FLAGS);
            let child = command
                .spawn()
                .context("start a semctl daemon inside this job object")?;
            Ok(child.id())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::{DAEMON_ARGS, command_for};
    use crate::session::PER_SESSION_VARS;

    #[test]
    fn the_daemon_is_started_with_the_run_subcommand() {
        let command = command_for(Path::new("/opt/semctl/bin/semctl"));

        assert_eq!(command.get_program(), OsStr::new("/opt/semctl/bin/semctl"));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, DAEMON_ARGS.map(OsStr::new).to_vec());
    }

    /// The daemon must not be able to describe a session with its own
    /// environment. These six, and only these six, are removed.
    #[test]
    fn the_daemon_environment_loses_exactly_the_per_session_variables() {
        let command = command_for(Path::new("semctl"));

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

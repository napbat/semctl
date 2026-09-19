//! The client role: attach to the shared daemon and copy bytes.
//!
//! The client owns no engine, no watcher, and no runtime worker pool. It reads
//! this invocation once, sends it as the attach body, and then pumps bytes
//! between the host's standard streams and the daemon connection.
//!
//! The role decision happens before any runtime exists, so everything here is
//! entered from a plain function. [`crate::ipc::run_client`] builds the small
//! current-thread runtime the pump needs.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use super::spawn;
use crate::cli::Cli;
use crate::ipc::handshake::SessionRequest;
use crate::ipc::{self, Endpoint, Stream};
use crate::session::SessionContext;

/// Which role a `semctl mcp` invocation takes.
const DAEMON_MODE_VAR: &str = "SEMCTX_MCP_DAEMON";

/// How long the client waits for a daemon that should already be listening.
///
/// Short on purpose: when no daemon is there, this delay is pure latency
/// before the client starts one.
const EXISTING_DEADLINE: Duration = Duration::from_millis(300);

/// How long the client waits for a daemon it just started.
///
/// Long enough for a cold start under load: the daemon must be scheduled,
/// win its election, and bind before it can answer.
const SPAWNED_DEADLINE: Duration = Duration::from_secs(10);

/// What a `semctl mcp` invocation does about the shared daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonMode {
    /// Serve this session in this process, as every version before 0.2.0 did.
    Off,
    /// Attach to a daemon, start one when needed, and serve the session in
    /// this process when that fails. The default.
    ///
    /// Measurements with 100 sessions on one checkout are why this is the
    /// default: one daemon replaces one watcher, one reconcile queue, and one
    /// runtime per session, and it falls back when it cannot serve.
    Auto,
    /// Attach to a daemon, start one when needed, and fail when that fails.
    Require,
}

impl DaemonMode {
    /// Read `SEMCTX_MCP_DAEMON` once for this invocation.
    pub(crate) fn from_environment() -> Self {
        Self::parse(std::env::var(DAEMON_MODE_VAR).ok().as_deref())
    }

    /// The pure mapping behind [`Self::from_environment`].
    ///
    /// An absent, empty, or unknown value means the default, which is the rule
    /// every other environment key in this program follows. An unknown value
    /// also raises one warning, because a typo must be visible. The default is
    /// safe for a typo: `auto` serves the session in this process when no
    /// daemon can serve it.
    fn parse(raw: Option<&str>) -> Self {
        let value = raw.map(str::trim).unwrap_or_default();
        if value.is_empty() {
            return Self::Auto;
        }
        if value.eq_ignore_ascii_case("off") {
            return Self::Off;
        }
        if value.eq_ignore_ascii_case("auto") {
            return Self::Auto;
        }
        if value.eq_ignore_ascii_case("require") {
            return Self::Require;
        }
        warn!(
            variable = DAEMON_MODE_VAR,
            value, "unknown daemon mode; using the default"
        );
        Self::Auto
    }
}

/// How the client role ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientOutcome {
    /// The client is finished and this is its process exit code.
    Exited(i32),
    /// No session was served. The caller runs the standalone role and reports
    /// this reason once.
    FallBack(String),
}

/// Attach to the shared daemon and serve this session over that connection.
///
/// A failure before the pump starts — the endpoint, the spawn, the connection
/// deadline, or a refusal — is recoverable: `auto` falls back to the
/// standalone role and `require` reports it and exits with status 1.
///
/// A failure after the pump starts is the pump's exit code and never a
/// fallback. The MCP host owns the session by then, and starting a second
/// server on the same standard streams would corrupt the JSON-RPC stream. A
/// host that wants another session reconnects.
pub(crate) fn run_client_role(cli: &Cli, mode: DaemonMode) -> ClientOutcome {
    match attach(cli) {
        Ok(exit) => ClientOutcome::Exited(exit.code()),
        Err(error) => {
            let reason = format!("{error:#}");
            match mode {
                // `off` does not reach this function: the role selection
                // already chose the standalone role. Falling back is the same
                // answer either way.
                DaemonMode::Off | DaemonMode::Auto => ClientOutcome::FallBack(reason),
                DaemonMode::Require => {
                    eprintln!(
                        "semctl mcp requires the shared daemon ({DAEMON_MODE_VAR}=require), \
                         and it is unavailable: {reason}"
                    );
                    ClientOutcome::Exited(1)
                }
            }
        }
    }
}

/// Read this invocation, attach, and pump until the session ends.
fn attach(cli: &Cli) -> Result<ipc::pump::Exit> {
    // The one place the client reads itself. The daemon builds this session's
    // context from the body below and from nothing else.
    let context = SessionContext::from_process(cli)?;
    let session = SessionRequest::from_context(&context);
    let endpoint = Endpoint::current().context("locate the local daemon endpoint")?;
    ipc::run_client(session, || connect_or_start(&endpoint))
}

/// Connect to the daemon of this endpoint, starting one when none answers.
///
/// The two deadlines are different questions. The first asks "is a daemon
/// already serving?" and must not cost much when the answer is no. The second
/// asks "did the daemon I started come up?" and must tolerate a cold start.
async fn connect_or_start(endpoint: &Endpoint) -> Result<Stream> {
    match ipc::connect(endpoint, Instant::now() + EXISTING_DEADLINE).await {
        Ok(stream) => return Ok(stream),
        Err(error) if ipc::connect_found_a_busy_daemon(&error) => {
            // A busy endpoint is a live daemon with no free instance right
            // now, not a missing one. A burst of simultaneous sessions
            // produces exactly this, and a daemon started here could only
            // lose the election while adding load to the endpoint. Wait the
            // longer deadline instead.
            debug!(
                endpoint = endpoint.id(),
                "the daemon on this endpoint is busy; waiting instead of starting another"
            );
            match ipc::connect(endpoint, Instant::now() + SPAWNED_DEADLINE).await {
                Ok(stream) => return Ok(stream),
                Err(error) if ipc::connect_found_a_busy_daemon(&error) => {
                    return Err(error).context("connect to the busy daemon on this endpoint");
                }
                // The busy daemon went away while this client waited. Fall
                // through and start a daemon of its own.
                Err(error) => debug!(
                    error = format!("{error:#}"),
                    endpoint = endpoint.id(),
                    "the busy daemon went away; starting one"
                ),
            }
        }
        Err(error) => debug!(
            error = format!("{error:#}"),
            endpoint = endpoint.id(),
            "no daemon answered this endpoint; starting one"
        ),
    }
    spawn::spawn_daemon(endpoint)?;
    ipc::connect(endpoint, Instant::now() + SPAWNED_DEADLINE)
        .await
        .context("connect to the daemon this client started")
}

#[cfg(test)]
mod tests {
    use super::DaemonMode;

    #[test]
    fn an_unset_or_empty_daemon_mode_attaches_to_the_shared_daemon() {
        for value in [None, Some(""), Some("  ")] {
            assert_eq!(DaemonMode::parse(value), DaemonMode::Auto, "{value:?}");
        }
    }

    #[test]
    fn only_an_explicit_off_serves_the_session_in_this_process() {
        for value in ["off", "OFF", " Off "] {
            assert_eq!(DaemonMode::parse(Some(value)), DaemonMode::Off, "{value}");
        }
    }

    #[test]
    fn the_two_daemon_modes_are_parsed_case_insensitively() {
        for value in ["auto", "Auto", " AUTO "] {
            assert_eq!(DaemonMode::parse(Some(value)), DaemonMode::Auto, "{value}");
        }
        for value in ["require", "Require", " REQUIRE "] {
            assert_eq!(
                DaemonMode::parse(Some(value)),
                DaemonMode::Require,
                "{value}"
            );
        }
    }

    /// A typo must not stop a session from being served, and must not turn
    /// the shared daemon off by accident either.
    #[test]
    fn an_unknown_daemon_mode_falls_back_to_the_default() {
        for value in ["on", "true", "1", "requires"] {
            assert_eq!(DaemonMode::parse(Some(value)), DaemonMode::Auto, "{value}");
        }
    }
}

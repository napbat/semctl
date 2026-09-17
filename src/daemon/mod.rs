//! The shared local daemon: one process that serves every MCP session of one
//! operating-system user and one configuration directory.
//!
//! Three roles share this binary, and [`run_role`] picks one before any Tokio
//! runtime exists. That order matters: the client role must not pay for a
//! runtime it does not use, and the daemon role needs a bounded one.
//!
//! | Role | Entry | Runtime |
//! | --- | --- | --- |
//! | Client ([`client`]) | `semctl mcp` with a daemon mode | One current thread, built by [`crate::ipc::run_client`] |
//! | Daemon ([`serve`]) | `semctl daemon run` | Multi-thread, bounded worker count |
//! | Standalone | everything else, and the client's fallback | Multi-thread, the default settings |
//!
//! The other child modules are the session path ([`session`]), the daemon
//! spawn ([`spawn`]), the status shape ([`status`]), and the two control
//! commands ([`control`]).
//!
//! The daemon never reads its own environment or working directory for a
//! per-session value. Everything a session needs arrives in its attach body
//! (see [`crate::session::SessionContext::from_handshake`]).

mod client;
pub(crate) mod control;
pub(crate) mod serve;
mod session;
mod spawn;
mod status;

use anyhow::{Context, Result};
use tracing::warn;

use crate::cli::{Cli, Command};
use crate::commands::daemon::DaemonCommand;

use client::{ClientOutcome, DaemonMode};

/// The version this build speaks, in the handshake and in the status line.
///
/// The endpoint identity already includes it, so a client of another version
/// reaches another endpoint. The attach check is the second line of defense,
/// for a client that reaches this endpoint some other way.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which role this invocation takes.
enum Role {
    /// Serve the local endpoint.
    Daemon,
    /// Attach to the daemon, with this mode's failure policy.
    Client(DaemonMode),
    /// Run the command in this process, as every earlier version did.
    Standalone,
}

/// Choose the role, reading the daemon mode at most once.
fn select(command: &Command) -> Role {
    match command {
        Command::Daemon(DaemonCommand::Run) => Role::Daemon,
        Command::Mcp => match DaemonMode::from_environment() {
            DaemonMode::Off => Role::Standalone,
            mode => Role::Client(mode),
        },
        _ => Role::Standalone,
    }
}

/// Run the role this invocation asks for.
///
/// This is everything `main` does after it parses the command line and
/// installs the log subscriber.
pub(crate) fn run_role(cli: Cli) -> Result<()> {
    match select(&cli.command) {
        Role::Daemon => serve::run(),
        Role::Client(mode) => match client::run_client_role(&cli, mode) {
            ClientOutcome::Exited(0) => Ok(()),
            // The pump is finished and the runtime is already shut down in
            // the background. Nothing is left to unwind, and the host needs
            // this exact status.
            ClientOutcome::Exited(code) => std::process::exit(code),
            ClientOutcome::FallBack(reason) => {
                // One warning, then serve the session here. The host gets a
                // working session either way, which is what `auto` promises.
                warn!(
                    %reason,
                    "the shared daemon is unavailable; serving this session in this process"
                );
                standalone(cli)
            }
        },
        Role::Standalone => standalone(cli),
    }
}

/// Run the command in this process.
///
/// The runtime is the one every earlier version of `semctl` used, so a
/// standalone invocation behaves exactly as it did before the daemon existed.
fn standalone(cli: Cli) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the runtime")?;
    runtime.block_on(cli.run())
}

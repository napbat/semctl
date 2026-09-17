//! `semctl daemon` — the shared local daemon's own command surface.
//!
//! The commands are thin. Serving lives in [`crate::daemon::serve`], and the
//! two control commands live in [`crate::daemon::control`].

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::daemon;

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Serve MCP sessions over this user's local endpoint. Started by a
    /// `semctl mcp` client, not run by hand.
    #[command(hide = true)]
    Run,

    /// Report the running daemon: its version, uptime, sessions, checkouts,
    /// and scheduler permits. Exits with status 1 when no daemon is running.
    Status(StatusArgs),

    /// Ask the running daemon to finish its sessions and exit. Exits with
    /// status 1 when no daemon is running.
    Stop,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print the status as one JSON object instead of one fact per line.
    #[arg(long)]
    pub json: bool,
}

/// Dispatch one `semctl daemon` command.
///
/// `main` selects the daemon role before it builds a runtime, so `run`
/// normally never reaches this function. It is served anyway when it does: the
/// behavior is the same, and only the runtime's worker bounds differ.
pub(crate) async fn run(command: DaemonCommand) -> Result<()> {
    match command {
        DaemonCommand::Run => daemon::serve::serve().await,
        DaemonCommand::Status(args) => daemon::control::status(args.json).await,
        DaemonCommand::Stop => daemon::control::stop().await,
    }
}

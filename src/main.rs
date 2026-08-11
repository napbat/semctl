//! `semctl` — command-line client for the semctx code-intelligence server.
//!
//! Auth: OIDC device-code against the configured identity provider; the
//! token set is stored in a `0600` `~/.config/semctl/credentials.json`
//! (see [`auth`]) — uniform across platforms, unlike the OS keychain.
//!
//! Wire: plain HTTP + JSON to the server's REST surface, with hand-written
//! typed request / response types in [`client::api`].
//!
//! Layout: [`cli`] parses the command line and dispatches into [`commands`];
//! [`client`] is the shared HTTP layer; [`mcp`] runs the CLI as an MCP stdio
//! server; [`sync`] walks and uploads a codebase; [`config`]/[`auth`] own the
//! on-disk config and credentials.

mod auth;
mod cli;
mod client;
mod codebase;
mod commands;
mod config;
mod editing;
mod mcp;
mod query;
mod sync;
mod term;

use anyhow::Result;
use clap::Parser;

use crate::cli::Command;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    init_tracing(&cli.command);
    cli.run().await
}

/// Install the process-wide log subscriber. Everything goes to **stderr**:
/// stdout carries command results and, under `semctl mcp`, the JSON-RPC stream,
/// so it must stay free of log noise. Verbosity follows `RUST_LOG` (e.g.
/// `RUST_LOG=semctl=debug`).
///
/// The default when `RUST_LOG` is unset is `info`, except for `semctl hook`:
/// the plugin invokes it directly (no shell shim to silence it), so it defaults
/// to `off` — a hook must never spew tracing onto a Claude Code session. The
/// hook's own stderr diagnostics are a separate **compile-time** `hook-debug`
/// feature (off by default), not a runtime env switch, so release builds can't
/// leak internal detail onto a session.
fn init_tracing(command: &Command) {
    use tracing_subscriber::{EnvFilter, fmt};
    let default = if matches!(command, Command::Hook(_)) {
        "off"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}

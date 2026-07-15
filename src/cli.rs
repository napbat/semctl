//! Command-line surface: the top-level [`Cli`] parser and its [`Command`] enum.
//! Global flags (server URL, tenant, codebase) live on the root and are read by
//! every subcommand; [`Cli::run`] dispatches each command to the matching module
//! under [`crate::commands`].

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::commands;

/// Top-level CLI. Subcommands live in [`crate::commands`]; flags shared
/// by every subcommand (server URL, active tenant override) hang off the
/// root struct so users can set them once per invocation.
#[derive(Debug, Parser)]
#[command(
    name = "semctl",
    version,
    about = "Semantic code search — command-line client",
    long_about = None,
)]
pub struct Cli {
    /// Override the configured server base URL for this invocation.
    /// e.g. `--server http://localhost:5200`. Falls back to the
    /// `SEMCTX_SERVER` environment variable, then the value in
    /// ~/.config/semctl/config.toml.
    #[arg(long, env = "SEMCTX_SERVER", global = true)]
    pub server: Option<String>,

    /// Override the active tenant for this invocation. Sent as the
    /// `X-Tenant-Id` header. Either the tenant slug or its Guid is
    /// accepted (the server resolves either).
    #[arg(long, env = "SEMCTX_TENANT", global = true)]
    pub tenant: Option<String>,

    /// The codebase id (Guid) the code/graph tools operate on. Optional —
    /// `semctl mcp` otherwise resolves the codebase from the working
    /// directory (its git remote, else its name). Set this (or
    /// `SEMCTX_CODEBASE`) to override that.
    #[arg(long, env = "SEMCTX_CODEBASE", global = true)]
    pub codebase: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Account & session: `login`, `logout`, `whoami`, `tenants`.
    #[command(subcommand)]
    Auth(commands::auth::AuthCommand),

    /// Cross-domain semantic search.
    Search(commands::search::SearchArgs),

    /// Register the folder as a local codebase (if needed) and sync its files to
    /// the server for indexing. Run it in a project directory; the codebase is
    /// created as a local working copy and cached so `semctl mcp` resolves it.
    Index(commands::index::IndexArgs),

    /// Exact code intelligence for the current codebase — definitions,
    /// references, callers, implementations, call/value-flow paths, and the
    /// resolution layer (imports / symbol-edges / external-links).
    #[command(subcommand)]
    Graph(commands::graph::GraphCommand),

    /// The codebase's file catalog — the directory `tree`, or a filtered `list`.
    #[command(subcommand)]
    Files(commands::files::FilesCommand),

    /// Read-only introspection: the detected `projects` graph, and the `domains`
    /// the server has registered.
    #[command(subcommand)]
    Inspect(commands::inspect::InspectCommand),

    /// Wire semctl into your AI coding tools (Claude Code, Codex). Interactive by
    /// default — the checklist shows the current wiring; check a tool to install
    /// it, uncheck to remove it. Scriptable: `semctl install claude`, `--all`,
    /// or `--none`.
    #[command(alias = "setup")]
    Install(commands::install::InstallArgs),

    /// Update the `semctl` binary itself, in place, to the latest release.
    /// Cross-platform (Windows / macOS / Linux). Distinct from `semctl install`,
    /// which manages the editor/agent integrations rather than this binary.
    #[command(alias = "self-update")]
    Upgrade,

    /// Remove semctl: unwire it from your AI tools, take it off PATH, and delete
    /// the installed binary. Add `--purge` to also delete config + credentials.
    Uninstall(commands::install::UninstallArgs),

    /// Run as an MCP stdio server — exposes the code-retrieval tools to an
    /// editor / agent (Claude Code, etc.). Launched by the host, not run
    /// interactively; reads auth from the credentials file (`semctl auth login`
    /// first). Speaks JSON-RPC over stdin/stdout — don't redirect them.
    Mcp,

    /// Claude Code / Codex plugin hook entry point: reads a hook event as JSON on
    /// stdin and emits prompt/session context when the repo is indexed. Invoked
    /// by the plugin's hooks.json, not run by hand.
    #[command(hide = true)]
    Hook(commands::hook::HookArgs),
}

impl Cli {
    pub async fn run(mut self) -> Result<()> {
        // Take the subcommand out so the remaining `Cli` (which carries
        // the global flags every subcommand reads) can be borrowed
        // immutably alongside the moved subcommand args.
        let command = std::mem::replace(&mut self.command, Command::Mcp);

        match command {
            Command::Auth(cmd) => commands::auth::run(cmd, &self).await,
            Command::Search(args) => commands::search::run(args, &self).await,
            Command::Index(args) => commands::index::run(args, &self).await,
            Command::Graph(cmd) => commands::graph::run(cmd, &self).await,
            Command::Files(cmd) => commands::files::run(cmd, &self).await,
            Command::Inspect(cmd) => commands::inspect::run(cmd, &self).await,
            Command::Install(args) => commands::install::run(&args),
            Command::Upgrade => commands::upgrade::run(&self).await,
            Command::Uninstall(args) => {
                commands::install::run_uninstall(&args);
                Ok(())
            }
            Command::Mcp => crate::mcp::run(&self).await,
            Command::Hook(args) => commands::hook::run(args, &self).await,
        }
    }
}

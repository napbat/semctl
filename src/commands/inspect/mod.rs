//! `semctl inspect …` — read-only introspection: the detected project graph for
//! the current codebase, and the domains the server has registered.

use anyhow::Result;
use clap::Subcommand;

use crate::cli::Cli;

pub mod domains;
pub mod projects;

#[derive(Debug, Subcommand)]
pub enum InspectCommand {
    /// The detected project graph for the current folder's codebase — leaf
    /// projects (Cargo/npm/go/.csproj) and the workspaces/solutions that contain
    /// them. Confirms project detection is working.
    Projects(projects::ProjectsArgs),

    /// The domains the server has registered.
    Domains,
}

pub async fn run(cmd: InspectCommand, cli: &Cli) -> Result<()> {
    match cmd {
        InspectCommand::Projects(args) => projects::run(args, cli).await,
        InspectCommand::Domains => domains::run(cli).await,
    }
}

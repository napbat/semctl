//! `semctl auth …` — account and session commands (`login`, `logout`, `whoami`,
//! `tenants`), grouped under one subcommand.

use anyhow::Result;
use clap::Subcommand;

use crate::cli::Cli;

pub mod login;
pub mod logout;
pub mod tenants;
pub mod whoami;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Authenticate against identity via OIDC device-code. Prints a short user
    /// code + URL; complete it in your browser and the CLI stashes the tokens in
    /// `~/.config/semctl/credentials.json`.
    Login(login::LoginArgs),

    /// Erase the stored tokens. The next call needs a fresh `semctl auth login`.
    Logout,

    /// Print the current authenticated principal — verifies the token is still
    /// valid and the server is reachable.
    Whoami,

    /// List the tenants you're a member of and pick one interactively on a
    /// terminal. `--switch` selects one non-interactively.
    Tenants(tenants::TenantsArgs),
}

pub async fn run(cmd: AuthCommand, cli: &Cli) -> Result<()> {
    match cmd {
        AuthCommand::Login(args) => login::run(args, cli).await,
        AuthCommand::Logout => logout::run(),
        AuthCommand::Whoami => whoami::run(cli).await,
        AuthCommand::Tenants(args) => tenants::run(args, cli).await,
    }
}

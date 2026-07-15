//! `semctl projects` — print the project graph the server detected for the
//! current folder's codebase. A quick way to confirm project detection
//! (`codebase → project[] → files[]`) is working.

use anyhow::{Context, Result};
use clap::Args;

use crate::cli::Cli;
use crate::client::{self, api};

#[derive(Debug, Args)]
pub struct ProjectsArgs {}

pub async fn run(_args: ProjectsArgs, cli: &Cli) -> Result<()> {
    let client = client::for_cwd(cli).await?;
    let codebase = client.codebase()?;
    let graph: api::ProjectGraph = client
        .get(&format!("/v1/codebases/{codebase}/projects"))
        .await
        .context("fetch projects")?;
    print!("{}", crate::query::render_projects(&graph));
    Ok(())
}

//! `semctl files …` — the current codebase's file catalog: the directory tree,
//! or a filtered listing. The CLI face of the MCP `file_tree` / `list_files`
//! tools.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::cli::Cli;
use crate::client;
use crate::query;

#[derive(Debug, Subcommand)]
pub enum FilesCommand {
    /// The codebase's files as an indented directory tree (from the catalog).
    Tree,

    /// List indexed files. `--filter` narrows to paths containing a substring,
    /// searched across the whole catalog; otherwise one page is returned with a
    /// footer telling you how to fetch the next.
    List(ListArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Case-insensitive path substring to filter by (spans the whole catalog).
    #[arg(long)]
    filter: Option<String>,

    /// Zero-based page to return when not filtering. Defaults to 0.
    #[arg(long)]
    page: Option<u32>,

    /// Rows per page (1–1000). Defaults to 1000 — the whole list in one call for
    /// a normal repo.
    #[arg(long)]
    page_size: Option<u32>,
}

pub async fn run(cmd: FilesCommand, cli: &Cli) -> Result<()> {
    let client = client::for_cwd(cli).await?;
    let out = match cmd {
        FilesCommand::Tree => query::file_tree(&client).await,
        FilesCommand::List(a) => {
            query::list_files(&client, a.filter.as_deref(), a.page, a.page_size).await
        }
    };
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
    Ok(())
}

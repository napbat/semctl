//! `semctl edit …` — grammar-native planning plus the sole local apply/undo
//! boundary. Planning subcommands only call the server and print JSON plans;
//! `apply` and `undo` perform hash-guarded local filesystem changes.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::cli::Cli;
use crate::client::{self, api};
use crate::query;

#[derive(Debug, Subcommand)]
pub enum EditCommand {
    /// Plan a semantic rename; does not write files.
    Rename(RenameArgs),
    /// Plan a conservative safe delete; does not write files.
    SafeDelete(SafeDeleteArgs),
    /// Plan replacement of a declaration body; does not write files.
    ReplaceBody(ReplaceBodyArgs),
    /// Plan insertion before a declaration; does not write files.
    InsertBefore(InsertArgs),
    /// Plan insertion after a declaration; does not write files.
    InsertAfter(InsertArgs),
    /// Verify and atomically apply a JSON plan to the bound checkout.
    Apply(ApplyArgs),
    /// Restore retained preimages while current postimage hashes still match.
    Undo(UndoArgs),
}

#[derive(Debug, Args)]
pub struct TargetArgs {
    /// Stable qualified symbol identity.
    #[arg(long, required_unless_present = "path", conflicts_with = "path")]
    symbol: Option<String>,
    /// Codebase-relative path for a positional target.
    #[arg(long, required_unless_present = "symbol", requires_all = ["line", "column"])]
    path: Option<String>,
    /// 1-based line for a positional target.
    #[arg(long, requires = "path", conflicts_with = "symbol")]
    line: Option<u32>,
    /// 0-based byte column for a positional target.
    #[arg(long, requires = "path", conflicts_with = "symbol")]
    column: Option<u32>,
}

impl TargetArgs {
    fn into_request(self) -> api::SymbolTargetRequest {
        api::SymbolTargetRequest {
            qualified_name: self.symbol,
            path: self.path,
            line: self.line,
            column: self.column,
        }
    }
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)] // Independent command-line switches.
pub struct RenameArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// New identifier spelling.
    new_name: String,
    #[arg(long)]
    include_comments: bool,
    #[arg(long)]
    include_strings: bool,
    #[arg(long)]
    include_unresolved_text: bool,
    #[arg(long)]
    allow_uncertain: bool,
}

#[derive(Debug, Args)]
pub struct SafeDeleteArgs {
    #[command(flatten)]
    target: TargetArgs,
    #[arg(long)]
    allow_uncertain: bool,
    #[arg(long)]
    allow_public_without_known_consumers: bool,
    #[arg(long = "reflection-pattern")]
    reflection_patterns: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ReplaceBodyArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// File containing the complete replacement body, including delimiters.
    #[arg(long)]
    body_file: PathBuf,
}

#[derive(Debug, Args)]
pub struct InsertArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// File containing the complete declaration to insert.
    #[arg(long)]
    source_file: PathBuf,
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// JSON file containing the exact `WorkspaceEditPlan` returned by a planner.
    plan_file: PathBuf,
    /// Explicitly approve the bounded formatter step when the plan contains one.
    #[arg(long)]
    run_formatter: bool,
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Plan id whose retained preimages should be restored.
    plan_id: String,
}

pub async fn run(command: EditCommand, cli: &Cli) -> Result<()> {
    match command {
        EditCommand::Apply(args) => apply(args, cli).await,
        EditCommand::Undo(args) => undo(args, cli).await,
        operation => plan(operation, cli).await,
    }
}

async fn plan(command: EditCommand, cli: &Cli) -> Result<()> {
    let client = client::for_cwd(cli).await?;
    let plan = match command {
        EditCommand::Rename(args) => {
            query::plan_rename(
                &client,
                &api::RenameSymbolRequest {
                    target: args.target.into_request(),
                    new_name: args.new_name,
                    include_comments: args.include_comments,
                    include_strings: args.include_strings,
                    include_unresolved_text: args.include_unresolved_text,
                    allow_uncertain: args.allow_uncertain,
                },
            )
            .await?
        }
        EditCommand::SafeDelete(args) => {
            query::plan_safe_delete(
                &client,
                &api::SafeDeleteSymbolRequest {
                    target: args.target.into_request(),
                    allow_uncertain: args.allow_uncertain,
                    allow_public_without_known_consumers: args.allow_public_without_known_consumers,
                    reflection_patterns: (!args.reflection_patterns.is_empty())
                        .then_some(args.reflection_patterns),
                },
            )
            .await?
        }
        EditCommand::ReplaceBody(args) => {
            let replacement = std::fs::read_to_string(&args.body_file)
                .with_context(|| format!("read {}", args.body_file.display()))?;
            query::plan_replace_body(
                &client,
                &api::ReplaceSymbolBodyRequest {
                    target: args.target.into_request(),
                    replacement,
                },
            )
            .await?
        }
        EditCommand::InsertBefore(args) => plan_insert(&client, args, true).await?,
        EditCommand::InsertAfter(args) => plan_insert(&client, args, false).await?,
        EditCommand::Apply(_) | EditCommand::Undo(_) => bail!("invalid planning command"),
    };
    println!("{}", query::render_edit_plan(&plan));
    Ok(())
}

async fn plan_insert(
    client: &client::Client,
    args: InsertArgs,
    before: bool,
) -> Result<api::WorkspaceEditPlan> {
    let source = std::fs::read_to_string(&args.source_file)
        .with_context(|| format!("read {}", args.source_file.display()))?;
    query::plan_insert(
        client,
        &api::InsertSymbolRequest {
            target: args.target.into_request(),
            source,
        },
        before,
    )
    .await
}

async fn apply(args: ApplyArgs, cli: &Cli) -> Result<()> {
    let text = std::fs::read_to_string(&args.plan_file)
        .with_context(|| format!("read {}", args.plan_file.display()))?;
    let plan: api::WorkspaceEditPlan = serde_json::from_str(&text)
        .with_context(|| format!("parse {}", args.plan_file.display()))?;
    let client = client::for_cwd(cli).await?;
    let outcome = crate::editing::apply(&client, &plan, args.run_formatter, false).await?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}

async fn undo(args: UndoArgs, cli: &Cli) -> Result<()> {
    let client = client::for_cwd(cli).await?;
    let outcome = crate::editing::undo(&client, &args.plan_id, false).await?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}

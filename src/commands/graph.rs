//! `semctl graph …` — inspect the code-graph layers for the current folder's
//! codebase. The direct way to test the resolution layer (imports /
//! symbol-edges / external-links) and the symbol graph (definitions /
//! references) against a live server, mirroring the MCP tools.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::cli::Cli;
use crate::client;
use crate::query;

#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Emit the original text and bounded native presentation as JSON.
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: GraphCommand,
}

#[derive(Debug, Subcommand)]
pub enum GraphCommand {
    /// Complete indexed file/directory overview with detected projects and resolved imports.
    Overview,
    /// File→file import edges (the resolution layer). Rust resolves these; C#
    /// has no file-level imports; C++ include edges are not resolved yet.
    Imports,
    /// Detected projects, groups, and their dependency links.
    Projects,
    /// Reference→definition symbol bindings via the project-qualified moniker
    /// index (the resolution layer). Any resolving language pack; C++ needs
    /// build context.
    SymbolEdges,
    /// Cross-codebase links — this codebase's imports resolved into other
    /// codebases you can see.
    ExternalLinks,
    /// Chunks that define a symbol (symbol graph — any registered language pack).
    Definitions {
        /// Exact symbol name.
        symbol: String,
    },
    /// Chunks that reference a symbol (symbol graph — any registered language pack).
    References {
        /// Exact symbol name.
        symbol: String,
        /// Optional grammar namespace: Type, Value, Macro, or Module.
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Incoming callers of a symbol — the definitions that call it (call graph).
    WhoCalls {
        /// Exact symbol name.
        symbol: String,
    },
    /// The types implementing a trait/interface (type graph).
    Implementations {
        /// Exact trait/interface name.
        symbol: String,
    },
    /// A shortest call chain from one symbol to another (call graph).
    CallPath {
        /// Exact symbol the chain starts at.
        from: String,
        /// Exact symbol the chain should reach.
        to: String,
    },
    /// Inter-procedural value flow (forward) — the external boundaries a value
    /// entering from `from` flows out to.
    Reaches {
        /// The source external boundary (e.g. `env/var`, or a substring).
        from: String,
    },
    /// Inter-procedural value flow (backward) — the external boundaries whose
    /// entering value reaches `to`.
    FlowsInto {
        /// The destination external boundary (e.g. `fs/write`, or a substring).
        to: String,
    },
    /// Inter-procedural value flow — the functions a value flows through from
    /// boundary `from` to boundary `to`.
    FlowsBetween {
        /// The source external boundary.
        from: String,
        /// The destination external boundary.
        to: String,
    },
    /// A symbol's neighbourhood in one shot — its definition plus direct callers
    /// and callees (symbol graph + call graph).
    Trace {
        /// Exact symbol name.
        symbol: String,
        /// Call-graph hops to include. Defaults to 1.
        #[arg(long, default_value_t = 1)]
        depth: u32,
    },
    /// The symbol at a file position. With `--column`, resolves the identifier
    /// under the cursor to its definition; without, the enclosing definition (hover).
    SymbolAtPosition {
        /// Codebase-relative path, e.g. `server/Startup.cs`.
        path: String,
        /// 1-based line number.
        line: u32,
        /// Optional 1-based column — resolve the identifier under the cursor.
        #[arg(long)]
        column: Option<u32>,
    },
    /// Resolve many symbols at once — definitions (or `--references`) for each.
    Batch {
        /// Exact symbol names.
        #[arg(required = true)]
        symbols: Vec<String>,
        /// Return references instead of definitions.
        #[arg(long)]
        references: bool,
    },
    /// Search declaration names and qualified name paths.
    SearchSymbols {
        query: String,
        #[arg(long, default_value = "Substring")]
        mode: String,
        #[arg(long = "kind")]
        kinds: Vec<String>,
        #[arg(long)]
        path_prefix: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Traverse declared/structural type relations.
    TypeHierarchy {
        symbol: String,
        #[arg(long, default_value = "Both")]
        direction: String,
        #[arg(long, default_value_t = 4)]
        depth: u32,
    },
    /// Complete bounded caller/callee graph around a symbol.
    CallGraph {
        symbol: String,
        #[arg(long, default_value_t = 2)]
        depth: u32,
        #[arg(long, default_value = "Both")]
        direction: String,
    },
    /// Strongly connected call-cycle groups.
    Cycles,
    /// Definitions with no resolved incoming references.
    Unused {
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100)]
        page_size: u32,
    },
    /// Byte-identical chunk groups and hashes.
    Duplicates,
    /// Grammar-nested file outline.
    Outline {
        path: String,
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(long = "kind")]
        kinds: Vec<String>,
        #[arg(long)]
        include_body: bool,
    },
    /// Indexed chunks overlapping an inclusive line range.
    Expand {
        path: String,
        line_start: u32,
        line_end: u32,
    },
}

pub async fn run(args: GraphArgs, cli: &Cli) -> Result<()> {
    let client = client::for_cwd(cli).await?;
    let presentation = if args.json {
        query::QueryPresentation::Inspector
    } else {
        query::QueryPresentation::Plain
    };
    let output = dispatch(args.command, &client, presentation)
        .await
        .map_err(query::ToolError::into_cli_error)?;
    if args.json {
        serde_json::to_writer(std::io::stdout().lock(), &output)?;
        println!();
    } else {
        print!("{}", output.text);
        if !output.text.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

async fn dispatch(
    command: GraphCommand,
    client: &client::Client,
    presentation: query::QueryPresentation,
) -> Result<query::QueryOutput, query::ToolError> {
    match command {
        GraphCommand::Overview => query::overview(client, presentation).await,
        GraphCommand::Projects => query::list_projects(client, presentation).await,
        GraphCommand::Imports => query::imports(client, presentation).await,
        GraphCommand::SymbolEdges => query::symbol_edges(client, presentation).await,
        GraphCommand::ExternalLinks => query::external_links(client, presentation).await,
        GraphCommand::Definitions { symbol } => {
            query::find_definition(client, &symbol, presentation).await
        }
        GraphCommand::References { symbol, namespace } => {
            query::find_references(client, &symbol, namespace.as_deref(), presentation).await
        }
        GraphCommand::WhoCalls { symbol } => query::who_calls(client, &symbol, presentation).await,
        GraphCommand::Implementations { symbol } => {
            query::implementations_of(client, &symbol, presentation).await
        }
        GraphCommand::CallPath { from, to } => {
            query::call_path(client, &from, &to, presentation).await
        }
        GraphCommand::Reaches { from } => query::reaches(client, &from, presentation).await,
        GraphCommand::FlowsInto { to } => query::flows_into(client, &to, presentation).await,
        GraphCommand::FlowsBetween { from, to } => {
            query::flows_between(client, &from, &to, presentation).await
        }
        GraphCommand::Trace { symbol, depth } => {
            query::trace(client, &symbol, depth, presentation).await
        }
        GraphCommand::SymbolAtPosition { path, line, column } => {
            query::symbol_at_position(client, &path, line, column, presentation).await
        }
        GraphCommand::Batch {
            symbols,
            references,
        } => query::batch_lookup(client, &symbols, references, presentation).await,
        GraphCommand::SearchSymbols {
            query: symbol_query,
            mode,
            kinds,
            path_prefix,
            project,
            language,
            limit,
        } => {
            query::search_symbols(
                client,
                &query::SymbolSearchOptions {
                    query: &symbol_query,
                    mode: &mode,
                    kinds: &kinds,
                    path_prefix: path_prefix.as_deref(),
                    project: project.as_deref(),
                    language: language.as_deref(),
                    limit,
                },
                presentation,
            )
            .await
        }
        GraphCommand::TypeHierarchy {
            symbol,
            direction,
            depth,
        } => query::type_hierarchy(client, &symbol, &direction, depth, presentation).await,
        GraphCommand::CallGraph {
            symbol,
            depth,
            direction,
        } => query::call_graph(client, &symbol, depth, &direction, presentation).await,
        GraphCommand::Cycles => query::cycles(client, presentation).await,
        GraphCommand::Unused { page, page_size } => {
            query::unused(client, page, page_size, presentation).await
        }
        GraphCommand::Duplicates => query::duplicates(client, presentation).await,
        GraphCommand::Outline {
            path,
            max_depth,
            kinds,
            include_body,
        } => {
            query::file_outline(client, &path, max_depth, &kinds, include_body, presentation).await
        }
        GraphCommand::Expand {
            path,
            line_start,
            line_end,
        } => query::expand_chunk(client, &path, line_start, line_end, presentation).await,
    }
}

//! `semctl search` — cross-domain semantic search over the indexed codebase.

use anyhow::Result;
use clap::Args;

use crate::cli::Cli;
use crate::client;
use crate::query;

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Natural-language query.
    pub query: String,

    /// Max hits to return. Server clamps at 1000.
    #[arg(long, default_value_t = 20)]
    pub top_k: u32,

    /// Restrict to specific registered domains by id. Repeatable.
    /// Without this, fan-out hits every registered domain.
    #[arg(long = "domain", value_name = "ID")]
    pub domains: Vec<String>,

    /// Ranking bias: `code` demotes docs (markdown), `docs` demotes code.
    #[arg(long, value_name = "CODE|DOCS")]
    pub prefer: Option<String>,

    /// Restrict to these chunk kinds (`function`, `container`, `block`).
    /// Repeatable.
    #[arg(long = "kind", value_name = "KIND")]
    pub kinds: Vec<String>,

    /// Render the full enclosing-symbol body per hit instead of a short snippet.
    #[arg(long)]
    pub expand: bool,

    /// Search the server's local/personal/organization/global scope lens instead
    /// of the current checkout.
    #[arg(long, conflicts_with = "codebase_ids")]
    pub scope: Option<String>,

    /// Search an explicit visible codebase id. Repeatable.
    #[arg(long = "codebase-id", value_name = "ID", conflicts_with = "scope")]
    pub codebase_ids: Vec<String>,
}

pub async fn run(args: SearchArgs, cli: &Cli) -> Result<()> {
    let mut client = client::from_cli(cli)?;

    // Opportunistically bind the cwd's codebase so `--expand` and the staleness
    // check can use the local checkout. Harmless if it stays unbound — search
    // then spans every codebase the caller can see.
    if client.codebase_raw().is_none()
        && let Ok(dir) = std::env::current_dir()
        && let Ok(Some(r)) = crate::codebase::resolve(&client, &dir).await
    {
        client = client.with_codebase(r.id);
    }
    if let Some(id) = client.codebase_raw() {
        let root = crate::config::load()
            .ok()
            .and_then(|cfg| cfg.codebase_root(id, std::env::current_dir().ok().as_deref()));
        client = client.with_local_root(root);
    }

    let opts = query::SearchOpts {
        prefer: args.prefer.clone(),
        kinds: args.kinds.clone(),
        expand: args.expand,
        scope: args.scope.clone(),
        codebase_ids: args.codebase_ids.clone(),
    };
    let out = query::search(&client, &args.query, args.top_k, &args.domains, &opts).await;
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
    Ok(())
}

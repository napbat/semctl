//! `semctl mcp` — MCP stdio server.
//!
//! Exposes the server's code-retrieval endpoints as MCP tools so an
//! editor / agent (Claude Code, etc.) can search and navigate the
//! indexed codebase. Each tool is a thin shim over [`crate::query`], which
//! delegates to the shared HTTP [`Client`]. Auth is whatever
//! `semctl auth login` stashed in the credentials file — the MCP host launches
//! `semctl mcp` and inherits that session.
//!
//! Retrieval bodies live in [`crate::query`]. Symbolic edit tools also consume
//! the server's immutable plan through [`crate::editing`] and apply it to the
//! bound checkout in the same approved MCP action.

use anyhow::Result;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ListToolsResult, ServerCapabilities, ServerInfo, Tool, ToolAnnotations},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

use crate::cli::Cli;
use crate::client::{self, Client};
use crate::query;
use crate::sync::{self, JobRegistry};

const DIRECT_EDIT_TOOLS: &[&str] = &[
    "rename_symbol",
    "safe_delete_symbol",
    "replace_symbol_body",
    "insert_before_symbol",
    "insert_after_symbol",
    "undo_edit",
];

#[derive(Clone)]
pub struct McpServer {
    shared: Arc<Shared>,
    // Read by the `#[tool_handler]`-generated `call_tool` / `list_tools`
    // impls; the dead-code analyzer can't see through the macro.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// Cached search-freshness footer keyed by the job id it describes; the inner
/// `Option` is `None` when that job warrants no warning (a clean sync). Shared
/// behind a `Mutex` so concurrent searches reuse one poll. See
/// [`McpServer::index_freshness`].
type FreshnessCache = Arc<Mutex<Option<(String, Option<String>)>>>;

#[derive(Default)]
struct InitialIndexes {
    by_path: HashMap<PathBuf, Arc<InitialIndexGate>>,
    by_codebase: HashMap<String, Arc<InitialIndexGate>>,
}

struct InitialIndexGate {
    state: Mutex<InitialIndexState>,
    changed: Notify,
}

#[derive(Clone)]
enum InitialIndexState {
    Pending,
    Ready,
    Failed(String),
}

impl InitialIndexGate {
    fn pending() -> Self {
        Self {
            state: Mutex::new(InitialIndexState::Pending),
            changed: Notify::new(),
        }
    }

    async fn wait(&self) -> std::result::Result<(), String> {
        loop {
            // Register before checking state so a completion between the check and
            // await cannot be missed.
            let changed = self.changed.notified();
            match self.state.lock().await.clone() {
                InitialIndexState::Pending => changed.await,
                InitialIndexState::Ready => return Ok(()),
                InitialIndexState::Failed(reason) => return Err(reason),
            }
        }
    }

    async fn finish(&self, result: std::result::Result<(), String>) {
        *self.state.lock().await = match result {
            Ok(()) => InitialIndexState::Ready,
            Err(reason) => InitialIndexState::Failed(reason),
        };
        self.changed.notify_waiters();
    }
}

/// State shared across handler clones. The codebase binding is resolved lazily
/// and cached here, so a server that started unauthenticated (or before its
/// repo was reachable) self-heals on the first code-tool call after the problem
/// is fixed — e.g. after `semctl auth login` — without the host having to reconnect.
struct Shared {
    /// Client with no codebase bound (or the pinned one). Serves `list_domains`
    /// and is the template selected codebase clients are derived from.
    base: Client,
    /// Launch directory we resolve the current codebase against. Registration
    /// occurs only through the explicit `index_codebase` tool.
    dir: Option<std::path::PathBuf>,
    /// Codebase pinned up front (`--codebase` / `SEMCTX_CODEBASE` / config).
    /// The launch cwd is never synced into it; a separately cached local root can
    /// still be watched safely.
    pinned: bool,
    /// The codebase-bound client, once resolved. Held across the resolve so
    /// concurrent first calls can't race into a double-registration.
    bound: Mutex<Option<Client>>,
    /// Most recent index job per codebase queued by this session's watchers.
    jobs: Arc<JobRegistry>,
    /// Canonical local roots already being kept in sync. A path-scoped tool call
    /// activates its checkout once; subsequent calls reuse the existing watcher.
    watched: Mutex<HashMap<PathBuf, String>>,
    /// First-ever indexes currently building (or completed/failed in this
    /// process). Retrieval tools await these gates; `sync_status` deliberately
    /// bypasses them so progress remains observable.
    initial_indexes: Mutex<InitialIndexes>,
    /// Cached search freshness footer, keyed by the job id it describes; filled
    /// only once that job is terminal so repeated searches don't re-poll. See
    /// [`McpServer::index_freshness`].
    freshness: FreshnessCache,
    /// One-line "a newer semctl is published" prompt, set once by the startup
    /// update check ([`spawn_update_check`]) and consumed by one search footer.
    /// `None` until/unless a newer version is seen. Notify-only: applying the
    /// update stays the explicit `semctl upgrade`.
    update_note: Arc<Mutex<Option<String>>>,
}

impl McpServer {
    fn new(base: Client, dir: Option<std::path::PathBuf>, pinned: bool) -> Self {
        Self {
            shared: Arc::new(Shared {
                base,
                dir,
                pinned,
                bound: Mutex::new(None),
                jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                watched: Mutex::new(HashMap::new()),
                initial_indexes: Mutex::new(InitialIndexes::default()),
                freshness: Arc::new(Mutex::new(None)),
                update_note: Arc::new(Mutex::new(None)),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// A freshness warning for search results, derived from the most recent
    /// index job this session queued — **only** when there's something to flag
    /// (a sync running or failed), so a clean index adds no per-search noise.
    /// `None` too when no sync ran this session: we don't fabricate a freshness
    /// claim for a codebase indexed earlier (that caveat lives in `sync_status`).
    /// Cached once the job is terminal, keyed by job id so a later sync recomputes.
    async fn index_freshness(&self, client: &Client) -> Option<String> {
        let codebase_id = client.codebase_raw()?;
        let job = self.shared.jobs.lock().await.get(codebase_id).cloned()?;
        if let Some((id, footer)) = self.shared.freshness.lock().await.as_ref()
            && *id == job.job_id
        {
            return footer.clone();
        }
        let status = self
            .shared
            .base
            .get::<client::api::JobStatus>(&format!("/v1/jobs/{}", job.job_id))
            .await
            .ok()?;
        let footer = if status.error.is_some() {
            Some(
                "(index freshness: the last sync FAILED — results may be stale; run sync_status)"
                    .to_string(),
            )
        } else if status.completed_at.is_some() {
            // Cleanly synced — stay silent rather than annotate every search.
            None
        } else {
            Some(
                "(index freshness: a sync is in progress — results may be incomplete; run sync_status)"
                    .to_string(),
            )
        };
        // Cache only terminal states (done/failed); a running/queued job changes.
        if status.completed_at.is_some() || status.error.is_some() {
            *self.shared.freshness.lock().await = Some((job.job_id.clone(), footer.clone()));
        }
        footer
    }

    /// The client a code-graph tool should use, resolving the codebase if it
    /// isn't bound yet. A previously indexed folder carries durable consent and
    /// is synced/watched immediately without asking again. A genuinely unindexed
    /// folder is reported and never auto-registered. On failure it returns a
    /// human-readable reason — distinguishing "not logged in" from "server
    /// unreachable" from "not indexed" — which the tool surfaces to the model
    /// verbatim. Self-healing: a later call retries from scratch.
    async fn bound(&self) -> std::result::Result<Client, String> {
        if let Some(dir) = &self.shared.dir {
            self.await_initial_path(dir).await?;
        }
        let client = self.bound_unchecked().await?;
        if let Some(id) = client.codebase_raw() {
            self.await_initial_codebase(id).await?;
        }
        Ok(client)
    }

    async fn bound_unchecked(&self) -> std::result::Result<Client, String> {
        // Held across the network round-trips below so concurrent first calls
        // queue and reuse one bind instead of each registering a codebase.
        let mut guard = self.shared.bound.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }

        // Pinned: the codebase is already on `base`. Never associate it with the
        // launch cwd, which may be unrelated; only watch a cached root previously
        // recorded by an explicit index.
        if self.shared.pinned {
            let c = attach_local_root(self.shared.base.clone());
            let root = c.local_root().map(Path::to_path_buf);
            *guard = Some(c.clone());
            if let Some(root) = root {
                self.watch_once(c.clone(), root).await;
            }
            return Ok(c);
        }

        let Some(dir) = self.shared.dir.clone() else {
            return Err("launch directory unknown — set SEMCTX_CODEBASE / --codebase".into());
        };

        // Honest, local pre-check: an unauthenticated server can't resolve
        // anything, and that failure has nothing to do with the codebase — so
        // say so, rather than the misleading "no codebase for this directory".
        match crate::auth::load_tokens() {
            Ok(None) => {
                return Err(
                    "not logged in — run `semctl auth login`, then just retry (no reconnect needed)"
                        .into(),
                );
            }
            Err(e) => return Err(format!("can't read stored credentials: {e:#}")),
            Ok(Some(_)) => {}
        }

        // Authenticated: resolve against the server without registering on a
        // clean miss. Registration is reserved for the explicit index tool.
        let id = match crate::codebase::resolve(&self.shared.base, &dir).await {
            Ok(Some(r)) => {
                info!(codebase = %r.id, matched_by = r.how, dir = %dir.display(), "resolved codebase");
                r.id
            }
            // Not indexed: report it, don't silently register + upload. Indexing
            // is an explicit `semctl index`; a parent is used only when declared
            // an umbrella root (see `Config::cached_codebase_for`).
            Ok(None) => {
                return Err(format!(
                    "this folder isn't indexed as a semctl codebase. Do not index it \
                         automatically: ask the user to opt in, then call `index_codebase` \
                         (path `{}`). You can also pass an already-indexed directory path or \
                         codebase ID in any codebase-scoped tool's `codebase` argument",
                    dir.display()
                ));
            }
            Err(e) => {
                return Err(format!(
                    "can't reach the semctx server, or it rejected the request: {e:#}"
                ));
            }
        };

        let client = attach_local_root(self.shared.base.clone().with_codebase(id));
        *guard = Some(client.clone());
        // Keep this directory indexed for the rest of the session. Started here
        // (not unconditionally at startup) so it only runs once the codebase is
        // actually bound — including the self-heal path after a late login.
        self.watch_once(client.clone(), dir).await;
        Ok(client)
    }

    /// Resolve an optional per-call selector. Omitted means the launch/current
    /// codebase. An already-indexed directory is prior consent: resolve it without
    /// prompting or registering, then watch it for the rest of the session.
    /// Anything else is treated as a codebase id; when that id has a cached local
    /// checkout, that checkout is watched too.
    async fn client_for(&self, selector: Option<&str>) -> std::result::Result<Client, String> {
        let Some(raw) = selector.map(str::trim).filter(|s| !s.is_empty()) else {
            return self.bound().await;
        };
        let candidate = PathBuf::from(raw);
        let path_like = candidate.is_absolute()
            || candidate.is_dir()
            || raw == "."
            || raw == ".."
            || raw.contains('/')
            || raw.contains('\\');
        if path_like {
            let dir = canonical_directory(&candidate)?;
            self.await_initial_path(&dir).await?;
        } else {
            self.await_initial_codebase(raw).await?;
        }
        self.client_for_unchecked(Some(raw)).await
    }

    /// Resolve a selector without waiting for first-index readiness. Only status
    /// calls use this; retrieval calls must go through [`Self::client_for`].
    async fn client_for_unchecked(
        &self,
        selector: Option<&str>,
    ) -> std::result::Result<Client, String> {
        let Some(raw) = selector.map(str::trim).filter(|s| !s.is_empty()) else {
            return self.bound_unchecked().await;
        };
        let candidate = PathBuf::from(raw);
        let path_like = candidate.is_absolute()
            || candidate.is_dir()
            || raw == "."
            || raw == ".."
            || raw.contains('/')
            || raw.contains('\\');
        if path_like {
            let dir = canonical_directory(&candidate)?;
            let resolved = crate::codebase::resolve(&self.shared.base, &dir)
                .await
                .map_err(|e| format!("can't resolve codebase for {}: {e:#}", dir.display()))?
                .ok_or_else(|| {
                    format!(
                        "{} isn't indexed. Do not index it automatically: ask the user to opt \
                         in, then call `index_codebase` with that path",
                        dir.display()
                    )
                })?;
            // Prefer the cached root that contains the supplied path. This matters
            // for umbrella indexes and for ids with multiple local checkouts: using
            // the MCP launch cwd here could watch/sync a different checkout.
            let watch_dir = crate::config::load()
                .ok()
                .and_then(|cfg| cfg.codebase_root(&resolved.id, Some(&dir)))
                .unwrap_or(dir);
            let client = self
                .shared
                .base
                .clone()
                .with_codebase(resolved.id)
                .with_local_root(Some(watch_dir.clone()));
            self.watch_once(client.clone(), watch_dir).await;
            return Ok(client);
        }

        match self
            .shared
            .base
            .get_opt::<client::api::CodebaseSummary>(&format!("/v1/codebases/{raw}"))
            .await
        {
            Ok(Some(_)) => {
                let client =
                    attach_local_root(self.shared.base.clone().with_codebase(raw.to_string()));
                if let Some(root) = client.local_root().map(Path::to_path_buf) {
                    self.watch_once(client.clone(), root).await;
                }
                Ok(client)
            }
            Ok(None) => Err(format!(
                "codebase id `{raw}` was not found or is not accessible"
            )),
            Err(e) => Err(format!("can't resolve codebase id `{raw}`: {e:#}")),
        }
    }

    async fn await_initial_path(&self, dir: &Path) -> std::result::Result<(), String> {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let gate = self
            .shared
            .initial_indexes
            .lock()
            .await
            .by_path
            .get(&dir)
            .cloned();
        match gate {
            Some(gate) => gate
                .wait()
                .await
                .map_err(|e| format!("initial index failed — {e}")),
            None => Ok(()),
        }
    }

    async fn await_initial_codebase(&self, id: &str) -> std::result::Result<(), String> {
        let gate = self
            .shared
            .initial_indexes
            .lock()
            .await
            .by_codebase
            .get(id)
            .cloned();
        match gate {
            Some(gate) => gate
                .wait()
                .await
                .map_err(|e| format!("initial index failed — {e}")),
            None => Ok(()),
        }
    }

    /// Start one background sync/watcher per canonical local root.
    async fn watch_once(&self, client: Client, dir: PathBuf) {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let Some(codebase_id) = client.codebase_raw().map(str::to_string) else {
            return;
        };
        let mut watched = self.shared.watched.lock().await;
        if !watched.contains_key(&dir) {
            watched.insert(dir.clone(), codebase_id);
            drop(watched);
            sync::spawn_indexing(client, dir, self.shared.jobs.clone());
        }
    }

    /// Claim a new local root and start the exact same watcher lifecycle as
    /// [`Self::watch_once`], but retain its startup-sync completion handle for the
    /// first-index readiness gate.
    async fn watch_first_once(
        &self,
        client: Client,
        dir: PathBuf,
    ) -> std::result::Result<
        tokio::sync::oneshot::Receiver<std::result::Result<sync::SyncOutcome, String>>,
        String,
    > {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let codebase_id = client
            .codebase_raw()
            .ok_or_else(|| "first index has no codebase id".to_string())?
            .to_string();
        let mut watched = self.shared.watched.lock().await;
        if watched.contains_key(&dir) {
            return Err(format!("{} is already being watched", dir.display()));
        }
        watched.insert(dir.clone(), codebase_id);
        drop(watched);
        Ok(sync::spawn_indexing_tracked(
            client,
            dir,
            self.shared.jobs.clone(),
        ))
    }

    async fn watcher_active(&self, client: &Client) -> bool {
        let Some(codebase_id) = client.codebase_raw() else {
            return false;
        };
        self.shared
            .watched
            .lock()
            .await
            .values()
            .any(|candidate| candidate == codebase_id)
    }

    async fn apply_server_plan(
        &self,
        client: &Client,
        plan: client::api::WorkspaceEditPlan,
        run_formatter: bool,
        operation: &str,
    ) -> String {
        let watching = self.watcher_active(client).await;
        match crate::editing::apply(client, &plan, run_formatter, watching).await {
            Ok(outcome) => render_edit_action_outcome(&outcome)
                .unwrap_or_else(|error| format!("{operation} result render failed: {error}")),
            Err(error) => format!("{operation} refused: {error:#}"),
        }
    }

    async fn execute_insert(&self, args: InsertSymbolArgs, before: bool) -> String {
        let operation = if before {
            "insert_before_symbol"
        } else {
            "insert_after_symbol"
        };
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("{operation} unavailable — {error}"),
        };
        let run_formatter = args.run_formatter.unwrap_or(false);
        let request = client::api::InsertSymbolRequest {
            target: args.target,
            source: args.source,
        };
        match query::plan_insert(&client, &request, before).await {
            Ok(plan) => {
                self.apply_server_plan(&client, plan, run_formatter, operation)
                    .await
            }
            Err(error) => format!("{operation} planning failed: {error:#}"),
        }
    }
}

async fn wait_for_initial_job(client: &Client, job_id: &str) -> std::result::Result<(), String> {
    loop {
        let job = client
            .get::<client::api::JobStatus>(&format!("/v1/jobs/{job_id}"))
            .await
            .map_err(|e| format!("poll initial index job {job_id}: {e}"))?;
        if let Some(result) = initial_job_result(job_id, &job) {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn initial_job_result(
    job_id: &str,
    job: &client::api::JobStatus,
) -> Option<std::result::Result<(), String>> {
    if let Some(error) = &job.error {
        return Some(Err(format!("embedding job {job_id} failed: {error}")));
    }
    job.completed_at.as_ref()?;
    if job.files_failed > 0 {
        return Some(Err(format!(
            "embedding job {job_id} completed with {} failed file(s)",
            job.files_failed
        )));
    }
    Some(Ok(()))
}

fn canonical_directory(path: &Path) -> std::result::Result<PathBuf, String> {
    let dir = std::fs::canonicalize(path)
        .map_err(|e| format!("can't resolve directory {}: {e}", path.display()))?;
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    Ok(dir)
}

// Each tool's arg struct derives JsonSchema (the MCP host reads it to
// learn the parameters) and Deserialize (rmcp fills it from the call).
// Field-level `///` docs become per-parameter descriptions in the
// schema; struct-level `///` docs would leak into the tool's input
// schema `description`, so keep struct-level notes as plain `//`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Codebase to search: an id or an indexed local directory path. Omit for
    /// the launch/current codebase. A local path is watched while this MCP runs.
    pub codebase: Option<String>,
    /// Natural-language query.
    pub query: String,
    /// Max hits to return. Defaults to 20.
    pub top_k: Option<u32>,
    /// Restrict to these registered domain ids. Empty / omitted = all.
    pub domains: Option<Vec<String>>,
    /// Ranking bias: `"code"` demotes documentation (markdown), `"docs"` demotes
    /// code. Omit for the unbiased hybrid ranking.
    pub prefer: Option<String>,
    /// Restrict to these chunk kinds — `function`, `container`, or `block`.
    /// Omit for every kind.
    pub kinds: Option<Vec<String>>,
    /// Return the full enclosing-symbol body for each hit instead of a 4-line
    /// snippet — usually removes the follow-up `Read`. Defaults to false.
    pub expand: Option<bool>,
    /// Server scope lens: `local`, `personal`, `organization`, or `global`.
    /// Mutually exclusive with `codebase_ids`.
    pub scope: Option<String>,
    /// Explicit visible codebase ids to search. Mutually exclusive with `scope`.
    pub codebase_ids: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name to look up.
    pub symbol: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReferenceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name to look up.
    pub symbol: String,
    /// Optional grammar namespace: `Type`, `Value`, `Macro`, or `Module`.
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CallPathArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name the call chain starts at.
    pub from: String,
    /// Exact symbol name the call chain should reach.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowFromArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The external boundary a value enters from — a library-call moniker
    /// (`env/var`, a crate path) or a substring of one.
    pub from: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowToArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The external boundary a value leaves to — a library-call moniker
    /// (`fs/write`, a crate path) or a substring of one.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowBetweenArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The source external boundary a value enters from.
    pub from: String,
    /// The destination external boundary a value leaves to.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TraceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name to centre the neighbourhood on.
    pub symbol: String,
    /// How many call-graph hops out to include. Defaults to 1 (direct callers
    /// and callees).
    pub depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The pattern to search for — a **regular expression** by default (Rust
    /// `regex`-crate syntax, matched per line; no look-around or backreferences),
    /// so `fn \w+\(` finds function definitions. Set `literal: true` to match it
    /// as an exact substring instead, e.g. to search `.unwrap()` without escaping.
    pub pattern: String,
    /// Match `pattern` as a literal substring instead of a regex — every
    /// character, including `. * ( ) [ ] \`, is matched verbatim (no escaping).
    /// Defaults to false. Use it for exact code like `foo.bar()` or `Vec<T>`
    /// that would otherwise need escaping and risk accidental regex matches.
    pub literal: Option<bool>,
    /// Case-insensitive matching. Defaults to false.
    pub ignore_case: Option<bool>,
    /// Optional path substring to narrow which files are searched.
    pub path: Option<String>,
    /// Max matches to return, 1–1000. Defaults to 100.
    pub max: Option<u32>,
}

/// Whether a *regex-mode* grep pattern must run through the regex engine, or can
/// take the server's faster trigram-accelerated literal path. A pattern with no regex
/// metacharacter matches identically either way (regex `test` == literal
/// `test`), so it takes the fast path; anything using a metacharacter needs the
/// engine. Pure optimization: it only applies in the default regex mode — an
/// explicit `literal: true` always takes the literal path regardless.
fn pattern_needs_regex(pattern: &str) -> bool {
    pattern.contains(|c: char| {
        matches!(
            c,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        )
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OutlineArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative path of the file to outline (e.g. `server/Startup.cs`).
    pub path: String,
    /// Maximum grammar nesting depth to return. Omit for every depth.
    pub max_depth: Option<u32>,
    /// Restrict entries to grammar symbol kinds. Omit for every kind.
    pub kinds: Option<Vec<String>>,
    /// Include each declaration's exact body. Defaults to false.
    pub include_body: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSourceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative source path.
    pub path: String,
    /// Strong content hash to pin. A stale revision is rejected atomically.
    pub revision: Option<String>,
    /// 0-based byte-range start; requires `byte_end`.
    pub byte_start: Option<u64>,
    /// 0-based, end-exclusive byte-range end.
    pub byte_end: Option<u64>,
    /// 1-based line-range start; requires `line_end`.
    pub line_start: Option<u32>,
    /// 1-based inclusive line-range end.
    pub line_end: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolSearchArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Declaration name or qualified-name pattern.
    pub query: String,
    /// `Exact`, `Prefix`, `Substring`, `Glob`, or `Fuzzy`. Defaults to Substring.
    pub mode: Option<String>,
    /// Grammar symbol kinds to retain.
    pub kinds: Option<Vec<String>>,
    /// Optional codebase-relative path prefix.
    pub path_prefix: Option<String>,
    /// Optional detected project name.
    pub project: Option<String>,
    /// Optional language id.
    pub language: Option<String>,
    /// Maximum results, 1–500. Defaults to 50.
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TypeHierarchyArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact or qualified type identity.
    pub symbol: String,
    /// `Supertypes`, `Subtypes`, or `Both`. Defaults to Both.
    pub direction: Option<String>,
    /// Relation hops, 1–16. Defaults to 4.
    pub depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CallGraphArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact or qualified seed symbol.
    pub symbol: String,
    /// Call hops, 1–10. Defaults to 2.
    pub depth: Option<u32>,
    /// `Callers`, `Callees`, or `Both`. Defaults to Both.
    pub direction: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalysisPageArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Zero-based page. Defaults to 0.
    pub page: Option<u32>,
    /// Rows per page, 1–500. Defaults to 100.
    pub page_size: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// New identifier spelling.
    pub new_name: String,
    /// Include grammar-classified comments containing the old spelling.
    pub include_comments: Option<bool>,
    /// Include grammar-classified string literals containing the old spelling.
    pub include_strings: Option<bool>,
    /// Include unresolved textual candidates. Unsafe unless reviewed.
    pub include_unresolved_text: Option<bool>,
    /// Permit uncertain candidates. Defaults to false.
    pub allow_uncertain: Option<bool>,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SafeDeleteSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// Permit uncertain dynamic sites. Defaults to false.
    pub allow_uncertain: Option<bool>,
    /// Permit a public declaration only when no durable consumers are known.
    pub allow_public_without_known_consumers: Option<bool>,
    /// Configured reflection/dynamic-use patterns to check conservatively.
    pub reflection_patterns: Option<Vec<String>>,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceBodyArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// Replacement body source, including the grammar-owned delimiters.
    pub replacement: String,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target declaration.
    pub target: client::api::SymbolTargetRequest,
    /// Complete declaration source to insert.
    pub source: String,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UndoEditArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Edit id returned by a completed symbolic edit action.
    pub edit_id: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditActionOutcome<'a> {
    edit_id: &'a str,
    operation: &'a str,
    changed_files: &'a [crate::editing::AppliedFile],
    already_applied: bool,
    already_undone: bool,
    watcher_active: bool,
    sync_state: &'a str,
}

fn render_edit_action_outcome(
    outcome: &crate::editing::ApplyOutcome,
) -> std::result::Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&EditActionOutcome {
        edit_id: &outcome.plan_id,
        operation: &outcome.operation,
        changed_files: &outcome.changed_files,
        already_applied: outcome.already_applied,
        already_undone: outcome.already_undone,
        watcher_active: outcome.watcher_active,
        sync_state: &outcome.sync_state,
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExpandArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative path of the file (e.g. `server/Startup.cs`).
    pub path: String,
    /// First line of the range (1-based, inclusive).
    pub line_start: u32,
    /// Last line of the range (1-based, inclusive).
    pub line_end: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolAtPositionArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative path of the file (e.g. `server/Startup.cs`).
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// Optional 1-based column. With it, resolves the identifier under the cursor
    /// to its definition (go-to-definition); without it, the innermost enclosing
    /// definition (hover).
    pub column: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol names to resolve in one call.
    pub symbols: Vec<String>,
    /// Return references instead of definitions for each symbol. Defaults to false.
    pub references: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListFilesArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Optional case-insensitive substring; only files whose codebase-relative
    /// path contains it are listed (e.g. `src/auth` or `.rs`),
    /// searched across the whole catalog. Omit to list every indexed file.
    pub path: Option<String>,
    /// Zero-based page to return when not filtering. Defaults to 0; the result
    /// footer tells you when to request the next page. Ignored when `path` is set.
    pub page: Option<u32>,
    /// Rows per page, 1–1000. Defaults to 1000 — the whole list in one call for
    /// a normal repo. Lower it to scroll a very large catalog page by page.
    pub page_size: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexCodebaseArgs {
    /// Local directory to register and index. Omit for the launch/current
    /// directory. Calling this tool is the explicit indexing opt-in; agents must
    /// ask the user before calling it. A first-ever index waits for embedding to
    /// complete before retrieval tools are released.
    pub path: Option<String>,
}

// Tool descriptions come entirely from `docs/tools/<name>.md`: the `#[tool]`
// macro's `description` is a string literal (darling FromMeta) that can't take
// `include_str!`, so the macro leaves it empty and `tool_doc` injects the
// Markdown at runtime. Editing a tool's prose is a Markdown change, not source.
#[tool_router]
impl McpServer {
    #[tool]
    async fn search_codebase(&self, Parameters(args): Parameters<SearchArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(e) => return format!("search_codebase unavailable — {e}"),
        };
        let opts = query::SearchOpts {
            prefer: args.prefer,
            kinds: args.kinds.unwrap_or_default(),
            expand: args.expand.unwrap_or(false),
            scope: args.scope,
            codebase_ids: args.codebase_ids.unwrap_or_default(),
        };
        let mut out = query::search(
            &client,
            &args.query,
            args.top_k.unwrap_or(20),
            &args.domains.unwrap_or_default(),
            &opts,
        )
        .await;
        // Tell the reader how current the index is, so stale hits can be re-read.
        if let Some(footer) = self.index_freshness(&client).await {
            out.push_str("\n\n");
            out.push_str(&footer);
        }
        // One-shot ride-along fallback when a newer CLI is published. The
        // SessionStart hook is the primary user-facing notice; consuming this
        // note prevents repeated search results from spending tokens on it.
        if let Some(note) = self.shared.update_note.lock().await.take() {
            out.push_str("\n\n");
            out.push_str(&note);
        }
        out
    }

    #[tool]
    async fn find_definition(&self, Parameters(args): Parameters<SymbolArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::find_definition(&client, &args.symbol).await,
            Err(e) => format!("find_definition unavailable — {e}"),
        }
    }

    #[tool]
    async fn find_references(&self, Parameters(args): Parameters<ReferenceArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::find_references(&client, &args.symbol, args.namespace.as_deref()).await
            }
            Err(e) => format!("find_references unavailable — {e}"),
        }
    }

    #[tool]
    async fn who_calls(&self, Parameters(args): Parameters<SymbolArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::who_calls(&client, &args.symbol).await,
            Err(e) => format!("who_calls unavailable — {e}"),
        }
    }

    #[tool]
    async fn implementations_of(&self, Parameters(args): Parameters<SymbolArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::implementations_of(&client, &args.symbol).await,
            Err(e) => format!("implementations_of unavailable — {e}"),
        }
    }

    #[tool]
    async fn call_path(&self, Parameters(args): Parameters<CallPathArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::call_path(&client, &args.from, &args.to).await,
            Err(e) => format!("call_path unavailable — {e}"),
        }
    }

    #[tool]
    async fn reaches(&self, Parameters(args): Parameters<FlowFromArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::reaches(&client, &args.from).await,
            Err(e) => format!("reaches unavailable — {e}"),
        }
    }

    #[tool]
    async fn flows_into(&self, Parameters(args): Parameters<FlowToArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::flows_into(&client, &args.to).await,
            Err(e) => format!("flows_into unavailable — {e}"),
        }
    }

    #[tool]
    async fn flows_between(&self, Parameters(args): Parameters<FlowBetweenArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::flows_between(&client, &args.from, &args.to).await,
            Err(e) => format!("flows_between unavailable — {e}"),
        }
    }

    #[tool]
    async fn trace(&self, Parameters(args): Parameters<TraceArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::trace(&client, &args.symbol, args.depth.unwrap_or(1)).await,
            Err(e) => format!("trace unavailable — {e}"),
        }
    }

    #[tool]
    async fn grep(&self, Parameters(args): Parameters<GrepArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::grep(
                    &client,
                    &args.pattern,
                    !args.literal.unwrap_or(false) && pattern_needs_regex(&args.pattern),
                    args.ignore_case.unwrap_or(false),
                    args.path.as_deref(),
                    args.max.unwrap_or(100),
                )
                .await
            }
            Err(e) => format!("grep unavailable — {e}"),
        }
    }

    #[tool]
    async fn file_outline(&self, Parameters(args): Parameters<OutlineArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::file_outline(
                    &client,
                    &args.path,
                    args.max_depth,
                    &args.kinds.unwrap_or_default(),
                    args.include_body.unwrap_or(false),
                )
                .await
            }
            Err(e) => format!("file_outline unavailable — {e}"),
        }
    }

    #[tool]
    async fn expand_chunk(&self, Parameters(args): Parameters<ExpandArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::expand_chunk(&client, &args.path, args.line_start, args.line_end).await
            }
            Err(e) => format!("expand_chunk unavailable — {e}"),
        }
    }

    #[tool]
    async fn symbol_at_position(
        &self,
        Parameters(args): Parameters<SymbolAtPositionArgs>,
    ) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::symbol_at_position(&client, &args.path, args.line, args.column).await
            }
            Err(e) => format!("symbol_at_position unavailable — {e}"),
        }
    }

    #[tool]
    async fn batch_lookup(&self, Parameters(args): Parameters<BatchArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::batch_lookup(&client, &args.symbols, args.references.unwrap_or(false)).await
            }
            Err(e) => format!("batch_lookup unavailable — {e}"),
        }
    }

    #[tool]
    async fn file_tree(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::file_tree(&client).await,
            Err(e) => format!("file_tree unavailable — {e}"),
        }
    }

    #[tool]
    async fn list_files(&self, Parameters(args): Parameters<ListFilesArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::list_files(&client, args.path.as_deref(), args.page, args.page_size).await
            }
            Err(e) => format!("list_files unavailable — {e}"),
        }
    }

    #[tool]
    async fn list_projects(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::list_projects(&client).await,
            Err(e) => format!("list_projects unavailable — {e}"),
        }
    }

    #[tool]
    async fn imports(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::imports(&client).await,
            Err(e) => format!("imports unavailable — {e}"),
        }
    }

    #[tool]
    async fn symbol_edges(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::symbol_edges(&client).await,
            Err(e) => format!("symbol_edges unavailable — {e}"),
        }
    }

    #[tool]
    async fn external_links(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::external_links(&client).await,
            Err(e) => format!("external_links unavailable — {e}"),
        }
    }

    #[tool]
    async fn list_domains(&self, Parameters(_): Parameters<EmptyArgs>) -> String {
        // Domains aren't codebase-scoped, and listing them is the natural probe
        // when nothing else works — so always use the plain client.
        query::list_domains(&self.shared.base).await
    }

    #[tool]
    async fn list_codebases(&self, Parameters(_): Parameters<EmptyArgs>) -> String {
        let client = self
            .shared
            .bound
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| self.shared.base.clone());
        query::list_codebases(&client).await
    }

    #[tool]
    async fn current_context(&self, Parameters(args): Parameters<NoArgs>) -> String {
        let client = match self.client_for_unchecked(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("current_context unavailable — {error}"),
        };
        let codebase_id = client.codebase_raw().map(str::to_string);
        let watching = self.watcher_active(&client).await;
        let job = if let Some(id) = &codebase_id {
            self.shared.jobs.lock().await.get(id).cloned()
        } else {
            None
        };
        query::current_context(
            &client,
            watching,
            job.as_ref().map(|job| job.job_id.as_str()),
        )
        .await
    }

    #[tool]
    async fn read_source(&self, Parameters(args): Parameters<ReadSourceArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("read_source unavailable — {error}"),
        };
        let byte_range = match (args.byte_start, args.byte_end) {
            (Some(start), Some(end)) => Some((start, end)),
            (None, None) => None,
            _ => {
                return "read_source failed: byte_start and byte_end must be supplied together"
                    .into();
            }
        };
        let line_range = match (args.line_start, args.line_end) {
            (Some(start), Some(end)) => Some((start, end)),
            (None, None) => None,
            _ => {
                return "read_source failed: line_start and line_end must be supplied together"
                    .into();
            }
        };
        if byte_range.is_some() && line_range.is_some() {
            return "read_source failed: request either bytes or lines, not both".into();
        }
        query::read_source(
            &client,
            &args.path,
            args.revision.as_deref(),
            byte_range,
            line_range,
        )
        .await
    }

    #[tool]
    async fn search_symbols(&self, Parameters(args): Parameters<SymbolSearchArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("search_symbols unavailable — {error}"),
        };
        query::search_symbols(
            &client,
            &query::SymbolSearchOptions {
                query: &args.query,
                mode: args.mode.as_deref().unwrap_or("Substring"),
                kinds: &args.kinds.unwrap_or_default(),
                path_prefix: args.path_prefix.as_deref(),
                project: args.project.as_deref(),
                language: args.language.as_deref(),
                limit: args.limit.unwrap_or(50),
            },
        )
        .await
    }

    #[tool]
    async fn type_hierarchy(&self, Parameters(args): Parameters<TypeHierarchyArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("type_hierarchy unavailable — {error}"),
        };
        query::type_hierarchy(
            &client,
            &args.symbol,
            args.direction.as_deref().unwrap_or("Both"),
            args.depth.unwrap_or(4),
        )
        .await
    }

    #[tool]
    async fn call_graph(&self, Parameters(args): Parameters<CallGraphArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("call_graph unavailable — {error}"),
        };
        query::call_graph(
            &client,
            &args.symbol,
            args.depth.unwrap_or(2),
            args.direction.as_deref().unwrap_or("Both"),
        )
        .await
    }

    #[tool]
    async fn cycles(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::cycles(&client).await,
            Err(error) => format!("cycles unavailable — {error}"),
        }
    }

    #[tool]
    async fn unused(&self, Parameters(args): Parameters<AnalysisPageArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => {
                query::unused(
                    &client,
                    args.page.unwrap_or(0),
                    args.page_size.unwrap_or(100),
                )
                .await
            }
            Err(error) => format!("unused unavailable — {error}"),
        }
    }

    #[tool]
    async fn duplicates(&self, Parameters(args): Parameters<NoArgs>) -> String {
        match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => query::duplicates(&client).await,
            Err(error) => format!("duplicates unavailable — {error}"),
        }
    }

    #[tool]
    async fn rename_symbol(&self, Parameters(args): Parameters<RenameSymbolArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("rename_symbol unavailable — {error}"),
        };
        let run_formatter = args.run_formatter.unwrap_or(false);
        let request = client::api::RenameSymbolRequest {
            target: args.target,
            new_name: args.new_name,
            include_comments: args.include_comments.unwrap_or(false),
            include_strings: args.include_strings.unwrap_or(false),
            include_unresolved_text: args.include_unresolved_text.unwrap_or(false),
            allow_uncertain: args.allow_uncertain.unwrap_or(false),
        };
        match query::plan_rename(&client, &request).await {
            Ok(plan) => {
                self.apply_server_plan(&client, plan, run_formatter, "rename_symbol")
                    .await
            }
            Err(error) => format!("rename_symbol planning failed: {error:#}"),
        }
    }

    #[tool]
    async fn safe_delete_symbol(
        &self,
        Parameters(args): Parameters<SafeDeleteSymbolArgs>,
    ) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("safe_delete_symbol unavailable — {error}"),
        };
        let run_formatter = args.run_formatter.unwrap_or(false);
        let request = client::api::SafeDeleteSymbolRequest {
            target: args.target,
            allow_uncertain: args.allow_uncertain.unwrap_or(false),
            allow_public_without_known_consumers: args
                .allow_public_without_known_consumers
                .unwrap_or(false),
            reflection_patterns: args.reflection_patterns,
        };
        match query::plan_safe_delete(&client, &request).await {
            Ok(plan) => {
                self.apply_server_plan(&client, plan, run_formatter, "safe_delete_symbol")
                    .await
            }
            Err(error) => format!("safe_delete_symbol planning failed: {error:#}"),
        }
    }

    #[tool]
    async fn replace_symbol_body(&self, Parameters(args): Parameters<ReplaceBodyArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("replace_symbol_body unavailable — {error}"),
        };
        let run_formatter = args.run_formatter.unwrap_or(false);
        let request = client::api::ReplaceSymbolBodyRequest {
            target: args.target,
            replacement: args.replacement,
        };
        match query::plan_replace_body(&client, &request).await {
            Ok(plan) => {
                self.apply_server_plan(&client, plan, run_formatter, "replace_symbol_body")
                    .await
            }
            Err(error) => format!("replace_symbol_body planning failed: {error:#}"),
        }
    }

    #[tool]
    async fn insert_before_symbol(&self, Parameters(args): Parameters<InsertSymbolArgs>) -> String {
        self.execute_insert(args, true).await
    }

    #[tool]
    async fn insert_after_symbol(&self, Parameters(args): Parameters<InsertSymbolArgs>) -> String {
        self.execute_insert(args, false).await
    }

    #[tool]
    async fn undo_edit(&self, Parameters(args): Parameters<UndoEditArgs>) -> String {
        let client = match self.client_for(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(error) => return format!("undo_edit unavailable — {error}"),
        };
        let watching = self.watcher_active(&client).await;
        match crate::editing::undo(&client, &args.edit_id, watching).await {
            Ok(outcome) => render_edit_action_outcome(&outcome)
                .unwrap_or_else(|error| format!("undo_edit result render failed: {error}")),
            Err(error) => format!("undo_edit refused: {error:#}"),
        }
    }

    #[tool]
    async fn index_codebase(&self, Parameters(args): Parameters<IndexCodebaseArgs>) -> String {
        let requested = args
            .path
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.shared.dir.clone());
        let Some(requested) = requested else {
            return "index_codebase unavailable — no path was provided and the launch directory \
                    is unknown"
                .into();
        };
        let dir = match canonical_directory(&requested) {
            Ok(dir) => dir,
            Err(e) => return format!("index_codebase unavailable — {e}"),
        };

        // A concurrent/recent first-index call owns the gate. Await it rather
        // than queueing another full upload. Failure stays closed for this MCP
        // process, so retrieval can never fall through to a partial first index.
        if let Some(gate) = self
            .shared
            .initial_indexes
            .lock()
            .await
            .by_path
            .get(&dir)
            .cloned()
        {
            return match gate.wait().await {
                Ok(()) => format!(
                    "initial indexing complete\npath {}\nretrieval tools are now available",
                    dir.display()
                ),
                Err(e) => format!("initial indexing failed for {}: {e}", dir.display()),
            };
        }

        // Only this exact path's recorded index is prior permission. An umbrella
        // ancestor may serve read requests, but explicitly indexing the child is
        // a request for an independently writable codebase.
        match crate::codebase::resolve_exact(&self.shared.base, &dir).await {
            Ok(Some(resolved)) => {
                let client = self
                    .shared
                    .base
                    .clone()
                    .with_codebase(resolved.id.clone())
                    .with_local_root(Some(dir.clone()));
                if !self.shared.pinned && self.shared.dir.as_deref() == Some(dir.as_path()) {
                    *self.shared.bound.lock().await = Some(client.clone());
                }
                self.watch_once(client, dir.clone()).await;
                return format!(
                    "codebase {} was already indexed; background sync and watching started\npath {}",
                    resolved.id,
                    dir.display()
                );
            }
            Ok(None) => {}
            Err(e) => return format!("index_codebase failed for {}: {e:#}", dir.display()),
        }

        // Publish the path gate before registration. Once `ensure` creates the
        // server codebase, concurrent current/path-scoped retrieval calls already
        // have something to wait on.
        let (gate, starts_work) = {
            let mut indexes = self.shared.initial_indexes.lock().await;
            if let Some(gate) = indexes.by_path.get(&dir) {
                (gate.clone(), false)
            } else {
                let gate = Arc::new(InitialIndexGate::pending());
                indexes.by_path.insert(dir.clone(), gate.clone());
                (gate, true)
            }
        };
        if !starts_work {
            return match gate.wait().await {
                Ok(()) => format!(
                    "initial indexing complete\npath {}\nretrieval tools are now available",
                    dir.display()
                ),
                Err(e) => format!("initial indexing failed for {}: {e}", dir.display()),
            };
        }

        let id = match crate::codebase::ensure(&self.shared.base, &dir).await {
            Ok(id) => id,
            Err(e) => {
                let reason = format!("{e:#}");
                gate.finish(Err(reason.clone())).await;
                return format!("index_codebase failed for {}: {reason}", dir.display());
            }
        };
        self.shared
            .initial_indexes
            .lock()
            .await
            .by_codebase
            .insert(id.clone(), gate.clone());
        let client = self
            .shared
            .base
            .clone()
            .with_codebase(id.clone())
            .with_local_root(Some(dir.clone()));
        if !self.shared.pinned && self.shared.dir.as_deref() == Some(dir.as_path()) {
            *self.shared.bound.lock().await = Some(client.clone());
        }
        let initial_sync = match self.watch_first_once(client.clone(), dir.clone()).await {
            Ok(initial_sync) => initial_sync,
            Err(e) => {
                gate.finish(Err(e.clone())).await;
                return format!("index_codebase failed for {}: {e}", dir.display());
            }
        };
        let task_gate = gate.clone();
        let task_client = client.clone();
        tokio::spawn(async move {
            let result = match initial_sync.await {
                Ok(Ok(outcome)) => wait_for_initial_job(&task_client, &outcome.job_id).await,
                Ok(Err(reason)) => Err(format!("initial scan/upload failed: {reason}")),
                Err(_) => Err("initial indexing task ended before reporting its result".into()),
            };
            task_gate.finish(result).await;
        });

        match gate.wait().await {
            Ok(()) => format!(
                "initial indexing complete\ncodebase {id}\npath {}\nretrieval tools are now available",
                dir.display()
            ),
            Err(e) => format!(
                "initial indexing failed\ncodebase {id}\npath {}\nerror: {e}",
                dir.display()
            ),
        }
    }

    #[tool]
    async fn sync_status(&self, Parameters(args): Parameters<NoArgs>) -> String {
        let client = match self.client_for_unchecked(args.codebase.as_deref()).await {
            Ok(client) => client,
            Err(e) => return format!("sync_status unavailable — {e}"),
        };
        let codebase_id = match client.codebase() {
            Ok(id) => id.to_string(),
            Err(e) => return format!("sync_status unavailable — {e}"),
        };
        let job = self.shared.jobs.lock().await.get(&codebase_id).cloned();
        let watching = self
            .shared
            .watched
            .lock()
            .await
            .values()
            .any(|id| id == &codebase_id);
        query::sync_status(&client, job.as_ref().map(|j| j.job_id.as_str()), watching).await
    }
}

impl McpServer {
    /// The single mapping from tool name to its Markdown description —
    /// both `list_tools` and `get_tool` route through here so the two
    /// can never drift. Adding a tool means adding its `.md` and one
    /// arm here.
    fn tool_doc(name: &str) -> Option<&'static str> {
        Some(match name {
            "search_codebase" => include_str!("docs/tools/search_codebase.md"),
            "find_definition" => include_str!("docs/tools/find_definition.md"),
            "find_references" => include_str!("docs/tools/find_references.md"),
            "who_calls" => include_str!("docs/tools/who_calls.md"),
            "implementations_of" => include_str!("docs/tools/implementations_of.md"),
            "call_path" => include_str!("docs/tools/call_path.md"),
            "reaches" => include_str!("docs/tools/reaches.md"),
            "flows_into" => include_str!("docs/tools/flows_into.md"),
            "flows_between" => include_str!("docs/tools/flows_between.md"),
            "trace" => include_str!("docs/tools/trace.md"),
            "grep" => include_str!("docs/tools/grep.md"),
            "file_outline" => include_str!("docs/tools/file_outline.md"),
            "expand_chunk" => include_str!("docs/tools/expand_chunk.md"),
            "symbol_at_position" => include_str!("docs/tools/symbol_at_position.md"),
            "batch_lookup" => include_str!("docs/tools/batch_lookup.md"),
            "file_tree" => include_str!("docs/tools/file_tree.md"),
            "list_files" => include_str!("docs/tools/list_files.md"),
            "list_projects" => include_str!("docs/tools/list_projects.md"),
            "imports" => include_str!("docs/tools/imports.md"),
            "symbol_edges" => include_str!("docs/tools/symbol_edges.md"),
            "external_links" => include_str!("docs/tools/external_links.md"),
            "list_domains" => include_str!("docs/tools/list_domains.md"),
            "index_codebase" => include_str!("docs/tools/index_codebase.md"),
            "sync_status" => include_str!("docs/tools/sync_status.md"),
            "list_codebases" => include_str!("docs/tools/list_codebases.md"),
            "current_context" => include_str!("docs/tools/current_context.md"),
            "read_source" => include_str!("docs/tools/read_source.md"),
            "search_symbols" => include_str!("docs/tools/search_symbols.md"),
            "type_hierarchy" => include_str!("docs/tools/type_hierarchy.md"),
            "call_graph" => include_str!("docs/tools/call_graph.md"),
            "cycles" => include_str!("docs/tools/cycles.md"),
            "unused" => include_str!("docs/tools/unused.md"),
            "duplicates" => include_str!("docs/tools/duplicates.md"),
            "rename_symbol" => include_str!("docs/tools/rename_symbol.md"),
            "safe_delete_symbol" => include_str!("docs/tools/safe_delete_symbol.md"),
            "replace_symbol_body" => include_str!("docs/tools/replace_symbol_body.md"),
            "insert_before_symbol" => include_str!("docs/tools/insert_before_symbol.md"),
            "insert_after_symbol" => include_str!("docs/tools/insert_after_symbol.md"),
            "undo_edit" => include_str!("docs/tools/undo_edit.md"),
            _ => return None,
        })
    }

    /// Overlay the Markdown description onto a router-built tool.
    fn with_doc(mut tool: Tool) -> Tool {
        if let Some(md) = Self::tool_doc(&tool.name) {
            tool.description = Some(md.into());
        }
        tool.annotations = Some(if tool.name == "index_codebase" {
            ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(true)
                .open_world(false)
        } else if DIRECT_EDIT_TOOLS.contains(&tool.name.as_ref()) {
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(tool.name == "undo_edit")
                .open_world(false)
        } else {
            ToolAnnotations::new().read_only(true).open_world(false)
        });
        tool
    }
}

#[cfg(test)]
mod initial_index_tests {
    use std::time::Duration;

    use super::{InitialIndexGate, client, initial_job_result};

    fn job(completed: bool, failed: i64, error: Option<&str>) -> client::api::JobStatus {
        client::api::JobStatus {
            files_to_embed: 3,
            files_to_delete: 0,
            files_embedded: if completed { 3 - failed } else { 1 },
            files_deleted: 0,
            files_failed: failed,
            chunk_count: completed.then_some(12),
            error: error.map(str::to_string),
            started_at: Some("2026-07-31T00:00:00Z".into()),
            completed_at: completed.then(|| "2026-07-31T00:00:01Z".into()),
        }
    }

    #[tokio::test]
    async fn first_index_gate_blocks_until_embedding_is_ready() {
        let gate = InitialIndexGate::pending();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), gate.wait())
                .await
                .is_err(),
            "a pending first index must block retrieval"
        );

        gate.finish(Ok(())).await;
        assert_eq!(gate.wait().await, Ok(()));
    }

    #[tokio::test]
    async fn first_index_gate_propagates_failure() {
        let gate = InitialIndexGate::pending();
        gate.finish(Err("embedding failed".into())).await;
        assert_eq!(gate.wait().await, Err("embedding failed".into()));
    }

    #[test]
    fn first_index_requires_terminal_success() {
        assert!(initial_job_result("j", &job(false, 0, None)).is_none());
        assert_eq!(initial_job_result("j", &job(true, 0, None)), Some(Ok(())));
        assert!(
            initial_job_result("j", &job(true, 1, None))
                .unwrap()
                .unwrap_err()
                .contains("1 failed file")
        );
        assert!(
            initial_job_result("j", &job(true, 0, Some("worker died")))
                .unwrap()
                .unwrap_err()
                .contains("worker died")
        );
    }
}

#[cfg(test)]
mod edit_action_tests {
    use super::render_edit_action_outcome;

    #[test]
    fn action_result_exposes_an_edit_id_without_transporting_the_plan() {
        let outcome = crate::editing::ApplyOutcome {
            plan_id: "a".repeat(64),
            operation: "rename_symbol".into(),
            changed_files: vec![crate::editing::AppliedFile {
                path: "src/lib.rs".into(),
                content_hash: "b".repeat(64),
            }],
            already_applied: false,
            already_undone: false,
            watcher_active: true,
            sync_state: "watcher will enqueue sync".into(),
        };

        let rendered = render_edit_action_outcome(&outcome).expect("render action result");
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["editId"], outcome.plan_id);
        assert!(value.get("planId").is_none());
        assert!(value.get("plan").is_none());
    }
}

#[cfg(test)]
mod steering_tests;

#[tool_handler]
impl ServerHandler for McpServer {
    // Defining these here makes `#[tool_handler]` skip its generated
    // versions (it checks `has_method`), so the Markdown descriptions
    // reach the host. `call_tool` is still generated — it ignores
    // descriptions, so the default delegation is correct.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(Self::with_doc)
            .collect();
        Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned().map(Self::with_doc)
    }

    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive] — start from default, then set
        // the fields we care about.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(include_str!("docs/instructions/server.md").to_string());
        info
    }
}

/// Entry point for the `semctl mcp` subcommand. Builds an authenticated
/// client, serves the tool surface over stdio, and blocks until the
/// host disconnects.
///
/// When no codebase is set explicitly (`SEMCTX_CODEBASE` / `--codebase`), it's
/// resolved from the host's launch directory cache; a cached *parent* counts
/// only when declared an umbrella root, and an unindexed folder resolves to
/// nothing so the startup hook/tools ask for user opt-in to `index_codebase`
/// rather than guessing by Git remote/name or auto-registering it (see
/// `bound`). A resolved codebase is then kept indexed in the background (see
/// `spawn_indexing`).
///
/// We do NOT abort the process when that binding fails. A failure (not logged
/// in, server down) is reported honestly by the code tools and retried on a
/// later call — meanwhile `list_domains` and code tools with an explicit codebase
/// selector work the moment auth is healthy, so killing the server would throw
/// those away too. Logs go to stderr (via the `tracing` subscriber); stdout is the
/// JSON-RPC channel and must stay clean.
pub async fn run(cli: &Cli) -> Result<()> {
    let base = client::from_cli(cli)?;

    // Pinned == a codebase was set up front (`--codebase` / `SEMCTX_CODEBASE` /
    // config). The launch cwd may be unrelated, so it is never synced into the
    // pinned id. A cached checkout root for that id can still be watched safely.
    let pinned = base.codebase_raw().is_some();

    // The launch directory: what we resolve against and, once indexed, auto-sync.
    // `None` means we can't read the cwd — code tools then need SEMCTX_CODEBASE.
    let dir = std::env::current_dir()
        .map(|d| std::fs::canonicalize(&d).unwrap_or(d))
        .map_err(
            |e| warn!(error = %e, "can't read working directory; code tools need SEMCTX_CODEBASE"),
        )
        .ok();

    let server = McpServer::new(base, dir, pinned);

    // Detached, best-effort check for a newer published CLI; see `spawn_update_check`.
    spawn_update_check(server.shared.update_note.clone(), cli.server.clone());

    // Bind eagerly so the happy path is ready — codebase resolved and the
    // background index kicked off — before the first tool call. This is one
    // round-trip; the heavy walk/upload runs on a detached task, so `serve`
    // still starts promptly. Best-effort: on failure we serve anyway and the
    // code tools self-heal (see `bound`).
    match server.bound().await {
        Ok(_) if pinned => info!(
            "codebase pinned explicitly; launch directory will not be synced into the pinned id"
        ),
        Ok(_) => {}
        Err(reason) => {
            warn!(%reason, "codebase not bound at startup; code tools will retry on demand");
        }
    }

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("rmcp serve: {e}"))?;

    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("rmcp wait: {e}"))?;
    Ok(())
}

/// One-shot, best-effort check for a newer published CLI, run detached at
/// startup. On a hit it records a one-line prompt in `note` (surfaced via one
/// search footer and an stderr line) — it never downloads or swaps the binary; that
/// stays the explicit `semctl upgrade`. The server caches the release lookup, so
/// there's no client-side throttle. Set `SEMCTX_MCP_UPDATE_CHECK=0` to skip it.
fn spawn_update_check(note: Arc<Mutex<Option<String>>>, server_override: Option<String>) {
    if std::env::var("SEMCTX_MCP_UPDATE_CHECK").as_deref() == Ok("0") {
        return;
    }
    tokio::spawn(async move {
        let Some(latest) =
            crate::commands::upgrade::check_for_update(server_override.as_deref()).await
        else {
            return;
        };
        let current = env!("CARGO_PKG_VERSION");
        info!(current, %latest, "a newer semctl is available — run `semctl upgrade`");
        *note.lock().await = Some(format!(
            "(semctl update available: v{latest} — you're on v{current}; \
             run `semctl upgrade` to update)"
        ));
    });
}

/// Best-effort: look up the active codebase's local checkout root (recorded by
/// `semctl index`) and fold it into the client, so hit paths render as absolute
/// and the host can open them directly. A miss (canonical / server-pulled
/// codebase, or one never indexed locally) leaves paths codebase-relative.
fn attach_local_root(client: Client) -> Client {
    let Some(id) = client.codebase_raw() else {
        return client;
    };
    let root = crate::config::load()
        .ok()
        .and_then(|cfg| cfg.codebase_root(id, std::env::current_dir().ok().as_deref()));
    if let Some(r) = &root {
        debug!(root = %r.display(), "hit paths absolutized against local checkout");
    }
    client.with_local_root(root)
}

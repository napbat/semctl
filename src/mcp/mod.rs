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
    ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{Tool, ToolAnnotations},
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

mod tool_types;
mod tools;

use tool_types::{InsertSymbolArgs, render_edit_action_outcome};

#[cfg(test)]
mod tests;

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
            tool_router: tools::router(),
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
            let dir = crate::codebase::working_copy_root(&dir).await;
            self.await_initial_path(&dir).await?;
            let selector = dir.to_string_lossy().into_owned();
            return self.client_for_unchecked(Some(&selector)).await;
        }
        self.await_initial_codebase(raw).await?;
        self.client_for_unchecked(Some(raw)).await
    }

    /// [`Self::client_for`], answering about the copy `copy` names.
    ///
    /// `"canonical"` asks about what the project publishes; anything else —
    /// including nothing — asks about the checkout this MCP is running in,
    /// which is the tree the caller is looking at.
    async fn client_for_copy(
        &self,
        selector: Option<&str>,
        copy: Option<&str>,
    ) -> std::result::Result<Client, String> {
        let client = self.client_for(selector).await?;

        Ok(match copy.map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("canonical") => client.for_canonical(),
            _ => client,
        })
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
    let dir = match std::env::current_dir() {
        Ok(dir) => {
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            Some(crate::codebase::working_copy_root(&dir).await)
        }
        Err(e) => {
            warn!(error = %e, "can't read working directory; code tools need SEMCTX_CODEBASE");
            None
        }
    };

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

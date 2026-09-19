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
//!
//! Nothing here owns a checkout. Keeping a working copy indexed belongs to
//! [`crate::engine`], which owns one coordinator per checkout for the whole
//! process. A session holds a lease on each checkout it uses, and that set of
//! leases is also what its first-index readiness waits on.

use anyhow::Result;
use rmcp::{
    ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{Tool, ToolAnnotations},
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, OnceCell, RwLock};
use tracing::{debug, info, warn};

use crate::cli::Cli;
use crate::client::{self, Client};
use crate::engine::{CheckoutKey, CoordinatorStatus, Engine, EngineSettings, Trigger};
use crate::query;
use crate::session::SessionContext;

pub(crate) mod readiness;
mod tool_types;
mod tools;

use readiness::{
    InitialIndexGate, SessionLeases, initial_gate_for_path, initial_index_failed,
    ready_for_codebases,
};

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
    #[allow(dead_code)] // The `tool_handler` macro reads this field.
    tool_router: ToolRouter<Self>,
}

/// Cached search-freshness footer keyed by the job id it describes; the inner
/// `Option` is `None` when that job warrants no warning (a clean sync). Shared
/// behind a `Mutex` so concurrent searches reuse one poll. See
/// [`McpServer::index_freshness`].
type FreshnessCache = Arc<Mutex<Option<(String, Option<String>)>>>;

/// State shared across handler clones. The codebase binding is resolved lazily
/// and cached here, so a server that started unauthenticated (or before its
/// repo was reachable) self-heals on the first code-tool call after the problem
/// is fixed — e.g. after `semctl auth login` — without the host having to reconnect.
struct Shared {
    /// This session's invocation context. Every per-session value — working
    /// directory, credentials, re-sync interval, update-check choice — is read
    /// from here, never from the process.
    context: SessionContext,
    /// Client with no codebase bound (or the pinned one). Serves `list_domains`
    /// and is the template selected codebase clients are derived from.
    base: Client,
    /// Launch working-copy root we resolve the current codebase against, derived
    /// from `context.cwd`. Registration occurs only through the explicit
    /// `index_codebase` tool.
    ///
    /// Resolved lazily by [`McpServer::dir`]: the resolution spawns a Git
    /// process, and running it during construction held the daemon's attach
    /// answer behind it, which pushed a burst of cold attaches past the
    /// client's handshake deadline.
    dir: OnceCell<PathBuf>,
    /// Codebase pinned up front (`--codebase` / `SEMCTX_CODEBASE` / config).
    /// The launch cwd is never synced into it; a separately cached local root can
    /// still be watched safely.
    pinned: bool,
    /// The codebase-bound client, once resolved. Held across the resolve so
    /// concurrent first calls can't race into a double-registration.
    bound: Mutex<Option<Client>>,
    /// The shared engine. It owns every checkout this process keeps in sync,
    /// so a second session on the same checkout adds a lease and nothing else.
    engine: Arc<Engine>,
    /// The coordinators this session keeps alive. Dropping the map releases
    /// them, and the engine frees a coordinator no session holds any more.
    ///
    /// It is also this session's readiness scope: retrieval tools await the
    /// first-index gates of these checkouts and of no others. `sync_status`
    /// deliberately bypasses the gates so progress remains observable.
    leases: SessionLeases,
    /// Cached search freshness footer, keyed by the job id it describes; filled
    /// only once that job is terminal so repeated searches don't re-poll. See
    /// [`McpServer::index_freshness`].
    freshness: FreshnessCache,
}

impl McpServer {
    /// Build the server for one session.
    ///
    /// `engine` is shared with every other session in this process. Everything
    /// else here belongs to this session: its client, its launch directory, and
    /// its codebase binding.
    pub(crate) fn new(context: SessionContext, engine: Arc<Engine>) -> Result<Self> {
        let base = client::from_context(
            &context,
            engine.transport(),
            Some(engine.scheduler().remote_permits()),
        )?;
        // Pinned == a codebase was set up front (`--codebase` / `SEMCTX_CODEBASE`
        // / config). The launch directory may be unrelated, so it is never
        // synced into the pinned id. A cached checkout root for that id can
        // still be watched safely.
        let pinned = base.codebase_raw().is_some();
        // The launch working-copy root is deliberately not resolved here. The
        // daemon builds this server before it answers `attached`, so nothing
        // that spawns a process or walks the filesystem may run yet; the
        // first caller of [`Self::dir`] pays that cost instead.
        Ok(Self::assemble(
            context,
            base,
            OnceCell::new(),
            pinned,
            engine,
        ))
    }

    /// The parts of one session, already resolved. Tests use it to serve a
    /// session without reading the configuration or the filesystem.
    #[cfg(test)]
    fn with_parts(
        context: SessionContext,
        base: Client,
        dir: PathBuf,
        pinned: bool,
        engine: Arc<Engine>,
    ) -> Self {
        Self::assemble(context, base, OnceCell::new_with(Some(dir)), pinned, engine)
    }

    /// The one place a server is put together.
    fn assemble(
        context: SessionContext,
        base: Client,
        dir: OnceCell<PathBuf>,
        pinned: bool,
        engine: Arc<Engine>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                context,
                base,
                dir,
                pinned,
                bound: Mutex::new(None),
                engine,
                leases: RwLock::new(HashMap::new()),
                freshness: Arc::new(Mutex::new(None)),
            }),
            tool_router: tools::router(),
        }
    }

    /// This session's launch working-copy root: what the current codebase is
    /// resolved against and, once indexed, auto-synced. Resolved once, on the
    /// first use.
    ///
    /// The resolution canonicalizes the launch directory and asks Git for its
    /// top level, which spawns a process. Deferring it keeps
    /// [`McpServer::new`] cheap, so the daemon's attach answer never waits on
    /// it. The session context always carries a launch directory.
    async fn dir(&self) -> &PathBuf {
        self.shared
            .dir
            .get_or_init(|| async {
                let launch = std::fs::canonicalize(&self.shared.context.cwd)
                    .unwrap_or_else(|_| self.shared.context.cwd.clone());
                crate::codebase::working_copy_root(&launch).await
            })
            .await
    }

    /// Ask the engine for its one update check, on this session's behalf.
    ///
    /// Every session does this, in both roles: the engine answers the first
    /// caller and reuses the note for the rest.
    pub(crate) fn start_update_check(&self) {
        self.shared.engine.start_update_check(
            self.shared.context.server.clone(),
            self.shared.context.update_check,
        );
    }

    /// Resolve this session's codebase before the first tool call.
    ///
    /// Binding eagerly makes the happy path ready — codebase resolved and the
    /// checkout's reconcile started — before the host asks anything. It is one
    /// round-trip; the heavy walk and upload run in the checkout's
    /// coordinator, so serving still starts promptly.
    ///
    /// It binds without waiting for a first-index gate. The gate belongs to
    /// the checkout's coordinator, which is shared, so waiting here would hold
    /// this session behind another session's embedding and behind a gate that
    /// failed. A tool call still waits: [`Self::bound`] takes the same lock and
    /// reuses this bind, and then waits for readiness on its own behalf.
    ///
    /// Both roles run this as a task of its own and serve at once, so the host
    /// never waits for it. Best effort: on failure the session serves anyway
    /// and the code tools self-heal (see [`Self::bound`]).
    pub(crate) async fn bind_at_startup(&self) {
        match self.bound_unchecked().await {
            Ok(_) if self.shared.pinned => info!(
                "codebase pinned explicitly; launch directory will not be synced into the pinned id"
            ),
            Ok(_) => {}
            Err(reason) => {
                warn!(%reason, "codebase not bound at startup; code tools will retry on demand");
            }
        }
    }

    /// The one-line "a newer semctl is published" prompt, if this session asked
    /// for the check and the engine found one.
    ///
    /// Taken once: it is a nudge, and repeated search results must not spend
    /// tokens on it. A session that turned the check off never reads it, so it
    /// cannot receive a notice another session asked for.
    async fn update_note(&self) -> Option<String> {
        if !self.shared.context.update_check {
            return None;
        }
        self.shared.engine.update_note().lock().await.take()
    }

    /// A freshness warning for search results, derived from the most recent
    /// index job this session queued — **only** when there's something to flag
    /// (a sync running or failed), so a clean index adds no per-search noise.
    /// `None` too when no sync ran this session: we don't fabricate a freshness
    /// claim for a codebase indexed earlier (that caveat lives in `sync_status`).
    /// Cached once the job is terminal, keyed by job id so a later sync recomputes.
    async fn index_freshness(&self, client: &Client) -> Option<String> {
        client.local_root()?;
        let job_id = self.checkout_status(client).await?.last_job_id?;
        if let Some((id, footer)) = self.shared.freshness.lock().await.as_ref()
            && *id == job_id
        {
            return footer.clone();
        }
        let status = self
            .shared
            .base
            .get::<client::api::JobStatus>(&format!("/v1/jobs/{job_id}"))
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
            *self.shared.freshness.lock().await = Some((job_id, footer.clone()));
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
        self.await_initial_path(self.dir().await).await?;
        let client = self.bound_unchecked().await?;
        self.await_initial_client(&client).await?;
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
            let c = attach_local_root(self.shared.base.clone(), &self.shared.context.cwd);
            *guard = Some(c.clone());
            drop(guard);
            self.watch_checkout_once(&c).await;
            return Ok(c);
        }

        let dir = self.dir().await.clone();

        // Honest, local pre-check: an unauthenticated server can't resolve
        // anything, and that failure has nothing to do with the codebase — so
        // say so, rather than the misleading "no codebase for this directory".
        match crate::auth::load_tokens(&self.shared.context.credentials) {
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

        let client = attach_local_root(
            self.shared.base.clone().with_codebase(id),
            &self.shared.context.cwd,
        );
        *guard = Some(client.clone());
        // Watcher ownership takes the index registry lock. Do not retain the
        // binding lock while another index may need it to finish registration.
        drop(guard);
        // An umbrella root can differ from the launch directory. Start watching
        // the bound checkout only after resolution succeeds, including after a
        // login performed while this MCP server was already running.
        self.watch_checkout_once(&client).await;
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
        let client = if selector_is_path_like(&self.shared.context.cwd, raw) {
            let dir = canonical_directory(&self.shared.context.cwd, &candidate)?;
            let dir = crate::codebase::working_copy_root(&dir).await;
            self.await_initial_path(&dir).await?;
            let selector = dir.to_string_lossy().into_owned();
            self.client_for_unchecked(Some(&selector)).await?
        } else {
            self.client_for_unchecked(Some(raw)).await?
        };
        self.await_initial_client(&client).await?;
        Ok(client)
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
        if selector_is_path_like(&self.shared.context.cwd, raw) {
            let dir = canonical_directory(&self.shared.context.cwd, &candidate)?;
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
                let client = attach_local_root(
                    self.shared.base.clone().with_codebase(raw.to_string()),
                    &self.shared.context.cwd,
                );
                self.watch_checkout_once(&client).await;
                Ok(client)
            }
            Ok(None) => Err(format!(
                "codebase id `{raw}` was not found or is not accessible"
            )),
            Err(e) => Err(format!("can't resolve codebase id `{raw}`: {e:#}")),
        }
    }

    /// Different checkouts can share a codebase id. A bound checkout waits for
    /// its own gate; a rootless request waits for all matching checkouts.
    async fn await_initial_client(&self, client: &Client) -> std::result::Result<(), String> {
        if let Some(root) = client.local_root() {
            self.await_initial_path(root).await
        } else if let Some(id) = client.codebase_raw() {
            self.await_initial_codebase(id).await
        } else {
            Ok(())
        }
    }

    async fn await_initial_path(&self, dir: &Path) -> std::result::Result<(), String> {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let gate = initial_gate_for_path(&self.shared.leases, &dir).await;
        match gate {
            Some(gate) => gate.wait().await.map_err(|e| initial_index_failed(&e)),
            None => Ok(()),
        }
    }

    async fn await_initial_codebase(&self, id: &str) -> std::result::Result<(), String> {
        ready_for_codebases(&self.shared.leases, &[id.to_string()])
            .await
            .map(|_| ())
    }

    /// Watch the checkout whose source identity the client sends with requests.
    async fn watch_checkout_once(&self, client: &Client) {
        if let Some(root) = client.local_root() {
            self.watch_once(client.clone(), root.to_path_buf()).await;
        }
    }

    /// Keep one canonical local root in sync for the rest of this session.
    ///
    /// The engine owns the coordinator, so this only adds a lease. Calling it
    /// again for the same checkout is free, and a checkout another session
    /// already attached keeps its warm cache and its existing watch.
    async fn watch_once(&self, client: Client, dir: PathBuf) {
        if client.codebase_raw().is_none() {
            return;
        }
        if let Err(reason) = self.attach(client, dir.clone()).await {
            warn!(root = %dir.display(), %reason, "could not keep this checkout in sync");
        }
    }

    /// Take a lease on `dir`'s coordinator and keep it with this session.
    async fn attach(
        &self,
        client: Client,
        dir: PathBuf,
    ) -> std::result::Result<CheckoutKey, String> {
        let lease = self
            .shared
            .engine
            .registry()
            .attach(client, dir, self.shared.context.resync_secs)
            .await?;
        let key = lease.key().clone();
        // A second lease on the same checkout is dropped here, which releases
        // it again: one session holds one lease per checkout.
        self.shared
            .leases
            .write()
            .await
            .entry(key.clone())
            .or_insert(lease);
        Ok(key)
    }

    /// Claim `dir` for a first index and return the gate to report it through.
    ///
    /// The gate exists before the codebase is registered on the server, so a
    /// retrieval call that arrives during registration has something to wait on.
    async fn watch_first_once(
        &self,
        client: Client,
        dir: PathBuf,
    ) -> std::result::Result<Arc<InitialIndexGate>, String> {
        let (lease, gate) = self
            .shared
            .engine
            .registry()
            .attach_first_index(client, dir, self.shared.context.resync_secs)
            .await?;
        let key = lease.key().clone();
        self.shared.leases.write().await.entry(key).or_insert(lease);
        Ok(gate)
    }

    /// The coordinator this session leases for `client`, as a status snapshot.
    ///
    /// A client with a local root names its checkout exactly. A pinned client
    /// without one can only be answered by codebase: any leased coordinator
    /// bound to that id reports on the same index.
    async fn checkout_status(&self, client: &Client) -> Option<CoordinatorStatus> {
        let leases = self.shared.leases.read().await;
        if let Some(root) = client.local_root() {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            for lease in leases.values() {
                if lease.coordinator().root() == root {
                    return Some(lease.coordinator().status().await);
                }
            }
        }
        let codebase_id = client.codebase_raw()?;
        for lease in leases.values() {
            if lease.coordinator().codebase_id().await.as_deref() == Some(codebase_id) {
                return Some(lease.coordinator().status().await);
            }
        }
        None
    }

    /// Whether this session keeps a checkout of `client`'s codebase in sync.
    /// An edit then lands in an index that is already being reconciled.
    async fn watcher_active(&self, client: &Client) -> bool {
        let Some(codebase_id) = client.codebase_raw() else {
            return false;
        };
        let leases = self.shared.leases.read().await;
        for lease in leases.values() {
            if lease.coordinator().codebase_id().await.as_deref() == Some(codebase_id) {
                return true;
            }
        }
        false
    }

    /// Ask every checkout this session leases for `client`'s codebase to
    /// reconcile now.
    async fn trigger_sync(&self, client: &Client) {
        let Some(codebase_id) = client.codebase_raw() else {
            return;
        };
        let leases = self.shared.leases.read().await;
        for lease in leases.values() {
            if lease.coordinator().codebase_id().await.as_deref() == Some(codebase_id) {
                lease.coordinator().trigger(Trigger::Explicit);
            }
        }
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
            Ok(outcome) => {
                // The edit is on disk. Ask for the sync now instead of waiting
                // for the watcher's debounce; a burst of edits still costs one
                // reconcile, because the coordinator coalesces triggers.
                self.trigger_sync(client).await;
                render_edit_action_outcome(&outcome)
                    .unwrap_or_else(|error| format!("{operation} result render failed: {error}"))
            }
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

/// Whether a selector names a directory rather than a codebase id.
///
/// A bare relative name is probed against the session's working directory, not
/// against the process working directory. One process serves sessions invoked
/// from several directories, and the daemon's own directory is not any
/// session's.
fn selector_is_path_like(cwd: &Path, raw: &str) -> bool {
    let candidate = Path::new(raw);
    candidate.is_absolute()
        || raw == "."
        || raw == ".."
        || raw.contains('/')
        || raw.contains('\\')
        || cwd.join(candidate).is_dir()
}

/// Resolve a selector path to a canonical directory.
///
/// A relative selector resolves against the session's working directory, not
/// against the process working directory. One process can serve sessions
/// invoked from different directories, and it must never move its own.
fn canonical_directory(cwd: &Path, path: &Path) -> std::result::Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let dir = std::fs::canonicalize(&absolute)
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
/// `bound`). A resolved codebase is then kept indexed by the engine's
/// coordinator for that checkout (see [`crate::engine::coordinator`]).
///
/// We do NOT abort the process when that binding fails. A failure (not logged
/// in, server down) is reported honestly by the code tools and retried on a
/// later call — meanwhile `list_domains` and code tools with an explicit codebase
/// selector work the moment auth is healthy, so killing the server would throw
/// those away too. Logs go to stderr (via the `tracing` subscriber); stdout is the
/// JSON-RPC channel and must stay clean.
pub async fn run(cli: &Cli) -> Result<()> {
    // The one place this process reads itself. Everything below takes the
    // session's values from `context`.
    let context = SessionContext::from_process(cli)?;
    // One engine per process. In standalone mode it serves exactly one session;
    // the type and the ownership are the same either way.
    let engine = Engine::new(EngineSettings::from_environment())?;
    let server = McpServer::new(context, engine)?;

    // Detached, best-effort check for a newer published CLI.
    server.start_update_check();
    // Detached as well, so `initialize` is answered while the bind runs. This
    // process owns the task and aborts it when the host disconnects.
    let binding = tokio::spawn({
        let server = server.clone();
        async move { server.bind_at_startup().await }
    });

    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("rmcp serve: {e}"))?;

    let outcome = service.waiting().await;
    binding.abort();
    outcome.map_err(|e| anyhow::anyhow!("rmcp wait: {e}"))?;
    Ok(())
}

/// Best-effort: look up the active codebase's local checkout root (recorded by
/// `semctl index`) and fold it into the client, so hit paths render as absolute
/// and the host can open them directly. A miss (canonical / server-pulled
/// codebase, or one never indexed locally) leaves paths codebase-relative.
///
/// `cwd` is the session's working directory. It picks the recorded checkout
/// that contains it when a codebase has several.
fn attach_local_root(client: Client, cwd: &Path) -> Client {
    let client = client.with_cached_local_root(Some(cwd));
    if let Some(r) = client.local_root() {
        debug!(root = %r.display(), "hit paths absolutized against local checkout");
    }
    client
}

//! MCP tool-router definitions.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ListToolsResult, ServerCapabilities, ServerInfo, Tool},
    service::RequestContext,
    tool, tool_handler, tool_router,
};

use super::tool_types::{
    AnalysisPageArgs, BatchArgs, CallGraphArgs, CallPathArgs, EmptyArgs, ExpandArgs,
    FlowBetweenArgs, FlowFromArgs, FlowToArgs, GrepArgs, IndexCodebaseArgs, InsertSymbolArgs,
    ListFilesArgs, NoArgs, OutlineArgs, ReadSourceArgs, ReferenceArgs, RenameSymbolArgs,
    ReplaceBodyArgs, SafeDeleteSymbolArgs, SearchArgs, SymbolArgs, SymbolAtPositionArgs,
    SymbolSearchArgs, TraceArgs, TypeHierarchyArgs, UndoEditArgs, pattern_needs_regex,
    render_edit_action_outcome,
};
use super::{
    InitialIndexGate, McpServer, canonical_directory, client, query, wait_for_initial_job,
};

// Tool descriptions come entirely from `docs/tools/<name>.md`: the `#[tool]`
// macro's `description` is a string literal (darling FromMeta) that can't take
// `include_str!`, so the macro leaves it empty and `tool_doc` injects the
// Markdown at runtime. Editing a tool's prose is a Markdown change, not source.
#[tool_router]
impl McpServer {
    #[tool]
    async fn search_codebase(&self, Parameters(args): Parameters<SearchArgs>) -> String {
        let client = match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
            Ok(client) => query::who_calls(&client, &args.symbol).await,
            Err(e) => format!("who_calls unavailable — {e}"),
        }
    }

    #[tool]
    async fn implementations_of(&self, Parameters(args): Parameters<SymbolArgs>) -> String {
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
            Ok(client) => query::trace(&client, &args.symbol, args.depth.unwrap_or(1)).await,
            Err(e) => format!("trace unavailable — {e}"),
        }
    }

    #[tool]
    async fn grep(&self, Parameters(args): Parameters<GrepArgs>) -> String {
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        let client = match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        let client = match self
            .client_for_copy(args.codebase.as_deref(), args.copy.as_deref())
            .await
        {
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
        // The sync manifest represents the complete Git working copy. Use that
        // same root for consent/cache lookup, readiness gates, checkout headers,
        // and watching; otherwise indexing from `repo/src` records one path but
        // later launches from `repo` incorrectly look unindexed.
        let dir = crate::codebase::working_copy_root(&dir).await;

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

#[tool_handler]
impl ServerHandler for McpServer {
    // Defining these here makes `#[tool_handler]` skip its generated
    // versions (it checks `has_method`), so the Markdown descriptions
    // reach the host. `call_tool` is still generated — it ignores
    // descriptions, so the default delegation is correct.
    // Not `async`: there is nothing to await, and the trait accepts any
    // `Future`. Written as async it is a future that never yields, which newer
    // clippy calls out rather than letting it read as if it might.
    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> {
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(Self::with_doc)
            .collect();

        std::future::ready(Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        }))
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

pub(super) fn router() -> ToolRouter<McpServer> {
    McpServer::tool_router()
}

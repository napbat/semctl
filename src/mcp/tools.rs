//! MCP tool-router definitions.

use std::path::PathBuf;

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
    SymbolSearchArgs, TraceArgs, TypeHierarchyArgs, UndoEditArgs, applied_edit_text,
    pattern_needs_regex,
};
use super::{
    FailureKind, McpServer, ToolError, canonical_directory, client, initial_gate_for_path, query,
    ready_for_codebases,
};

// Tool descriptions come entirely from `docs/tools/<name>.md`: the `#[tool]`
// macro's `description` is a string literal (darling FromMeta) that can't take
// `include_str!`, so the macro leaves it empty and `tool_doc` injects the
// Markdown at runtime. Editing a tool's prose is a Markdown change, not source.
#[tool_router]
impl McpServer {
    #[tool]
    async fn search_codebase(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<String, ToolError> {
        let opts = query::SearchOpts {
            prefer: args.prefer,
            kinds: args.kinds.unwrap_or_default(),
            expand: args.expand.unwrap_or(false),
            scope: args.scope,
            codebase_ids: args.codebase_ids.unwrap_or_default(),
        };
        let independent = args
            .codebase
            .as_deref()
            .is_none_or(|selector| selector.trim().is_empty())
            && (opts.scope.is_some() || !opts.codebase_ids.is_empty());
        let client = if independent {
            // An explicit search scope does not require the launch directory to
            // be indexed. A known binding can still identify its own local hits.
            let client = self
                .shared
                .bound
                .lock()
                .await
                .clone()
                .unwrap_or_else(|| self.shared.base.clone());
            if args
                .copy
                .as_deref()
                .is_some_and(|copy| copy.trim().eq_ignore_ascii_case("canonical"))
            {
                client.for_canonical()
            } else {
                client
            }
        } else {
            self.client_for_copy(
                "search_codebase",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?
        };
        opts.normalized_scope()?;
        // Reserve checked registry membership only for the query. A new
        // codebase cannot appear between readiness and scope evaluation.
        let indexes = if opts.scope.is_some() || !opts.codebase_ids.is_empty() {
            match ready_for_codebases(&self.shared.leases, &opts.codebase_ids).await {
                Ok(indexes) => Some(indexes),
                Err(detail) => {
                    return Err(ToolError::new(
                        "search_codebase",
                        FailureKind::IndexFailed,
                        detail,
                    ));
                }
            }
        } else {
            None
        };
        let mut out = query::search(
            &client,
            &args.query,
            args.top_k.unwrap_or(20),
            &args.domains.unwrap_or_default(),
            &opts,
        )
        .await?;
        drop(indexes);
        // The launch checkout's watcher does not describe a broader search.
        if opts.scope.is_none()
            && opts.codebase_ids.is_empty()
            && let Some(footer) = self.index_freshness(&client).await
        {
            out.push_str("\n\n");
            out.push_str(&footer);
        }
        // One-shot ride-along fallback when a newer CLI is published. The
        // SessionStart hook is the primary user-facing notice; consuming this
        // note prevents repeated search results from spending tokens on it.
        if let Some(note) = self.update_note().await {
            out.push_str("\n\n");
            out.push_str(&note);
        }
        Ok(out)
    }

    #[tool]
    async fn find_definition(
        &self,
        Parameters(args): Parameters<SymbolArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy(
                "find_definition",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?;
        query::find_definition(&client, &args.symbol).await
    }

    #[tool]
    async fn find_references(
        &self,
        Parameters(args): Parameters<ReferenceArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("find_references", args.codebase.as_deref())
            .await?;
        query::find_references(&client, &args.symbol, args.namespace.as_deref()).await
    }

    #[tool]
    async fn who_calls(
        &self,
        Parameters(args): Parameters<SymbolArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy("who_calls", args.codebase.as_deref(), args.copy.as_deref())
            .await?;
        query::who_calls(&client, &args.symbol).await
    }

    #[tool]
    async fn implementations_of(
        &self,
        Parameters(args): Parameters<SymbolArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy(
                "implementations_of",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?;
        query::implementations_of(&client, &args.symbol).await
    }

    #[tool]
    async fn call_path(
        &self,
        Parameters(args): Parameters<CallPathArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("call_path", args.codebase.as_deref())
            .await?;
        query::call_path(&client, &args.from, &args.to).await
    }

    #[tool]
    async fn reaches(
        &self,
        Parameters(args): Parameters<FlowFromArgs>,
    ) -> Result<String, ToolError> {
        let client = self.client_for("reaches", args.codebase.as_deref()).await?;
        query::reaches(&client, &args.from).await
    }

    #[tool]
    async fn flows_into(
        &self,
        Parameters(args): Parameters<FlowToArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("flows_into", args.codebase.as_deref())
            .await?;
        query::flows_into(&client, &args.to).await
    }

    #[tool]
    async fn flows_between(
        &self,
        Parameters(args): Parameters<FlowBetweenArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("flows_between", args.codebase.as_deref())
            .await?;
        query::flows_between(&client, &args.from, &args.to).await
    }

    #[tool]
    async fn trace(&self, Parameters(args): Parameters<TraceArgs>) -> Result<String, ToolError> {
        let client = self
            .client_for_copy("trace", args.codebase.as_deref(), args.copy.as_deref())
            .await?;
        query::trace(&client, &args.symbol, args.depth.unwrap_or(1)).await
    }

    #[tool]
    async fn grep(&self, Parameters(args): Parameters<GrepArgs>) -> Result<String, ToolError> {
        let client = self
            .client_for_copy("grep", args.codebase.as_deref(), args.copy.as_deref())
            .await?;
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

    #[tool]
    async fn file_outline(
        &self,
        Parameters(args): Parameters<OutlineArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy(
                "file_outline",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?;
        query::file_outline(
            &client,
            &args.path,
            args.max_depth,
            &args.kinds.unwrap_or_default(),
            args.include_body.unwrap_or(false),
        )
        .await
    }

    #[tool]
    async fn expand_chunk(
        &self,
        Parameters(args): Parameters<ExpandArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("expand_chunk", args.codebase.as_deref())
            .await?;
        query::expand_chunk(&client, &args.path, args.line_start, args.line_end).await
    }

    #[tool]
    async fn symbol_at_position(
        &self,
        Parameters(args): Parameters<SymbolAtPositionArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("symbol_at_position", args.codebase.as_deref())
            .await?;
        query::symbol_at_position(&client, &args.path, args.line, args.column).await
    }

    #[tool]
    async fn batch_lookup(
        &self,
        Parameters(args): Parameters<BatchArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("batch_lookup", args.codebase.as_deref())
            .await?;
        query::batch_lookup(&client, &args.symbols, args.references.unwrap_or(false)).await
    }

    #[tool]
    async fn file_tree(&self, Parameters(args): Parameters<NoArgs>) -> Result<String, ToolError> {
        let client = self
            .client_for("file_tree", args.codebase.as_deref())
            .await?;
        query::file_tree(&client).await
    }

    #[tool]
    async fn list_files(
        &self,
        Parameters(args): Parameters<ListFilesArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy("list_files", args.codebase.as_deref(), args.copy.as_deref())
            .await?;
        query::list_files(&client, args.path.as_deref(), args.page, args.page_size).await
    }

    #[tool]
    async fn list_projects(
        &self,
        Parameters(args): Parameters<NoArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("list_projects", args.codebase.as_deref())
            .await?;
        query::list_projects(&client).await
    }

    #[tool]
    async fn imports(&self, Parameters(args): Parameters<NoArgs>) -> Result<String, ToolError> {
        let client = self.client_for("imports", args.codebase.as_deref()).await?;
        query::imports(&client).await
    }

    #[tool]
    async fn symbol_edges(
        &self,
        Parameters(args): Parameters<NoArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("symbol_edges", args.codebase.as_deref())
            .await?;
        query::symbol_edges(&client).await
    }

    #[tool]
    async fn external_links(
        &self,
        Parameters(args): Parameters<NoArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("external_links", args.codebase.as_deref())
            .await?;
        query::external_links(&client).await
    }

    #[tool]
    async fn list_domains(
        &self,
        Parameters(_): Parameters<EmptyArgs>,
    ) -> Result<String, ToolError> {
        // Domains aren't codebase-scoped, and listing them is the natural probe
        // when nothing else works — so always use the plain client.
        query::list_domains(&self.shared.base).await
    }

    #[tool]
    async fn list_codebases(
        &self,
        Parameters(_): Parameters<EmptyArgs>,
    ) -> Result<String, ToolError> {
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
    async fn current_context(
        &self,
        Parameters(args): Parameters<NoArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_unchecked("current_context", args.codebase.as_deref())
            .await?;
        let status = self.checkout_status(&client).await;
        let watching = status.is_some();
        Ok(query::current_context(
            &client,
            watching,
            status
                .as_ref()
                .and_then(|status| status.last_job_id.as_deref()),
        )
        .await)
    }

    #[tool]
    async fn read_source(
        &self,
        Parameters(args): Parameters<ReadSourceArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy(
                "read_source",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?;
        let byte_range = match (args.byte_start, args.byte_end) {
            (Some(start), Some(end)) => Some((start, end)),
            (None, None) => None,
            _ => {
                return Err(ToolError::new(
                    "read_source",
                    FailureKind::InvalidArgument,
                    "byte_start and byte_end must be supplied together",
                ));
            }
        };
        let line_range = match (args.line_start, args.line_end) {
            (Some(start), Some(end)) => Some((start, end)),
            (None, None) => None,
            _ => {
                return Err(ToolError::new(
                    "read_source",
                    FailureKind::InvalidArgument,
                    "line_start and line_end must be supplied together",
                ));
            }
        };
        if byte_range.is_some() && line_range.is_some() {
            return Err(ToolError::new(
                "read_source",
                FailureKind::InvalidArgument,
                "request either bytes or lines, not both",
            ));
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
    async fn search_symbols(
        &self,
        Parameters(args): Parameters<SymbolSearchArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for_copy(
                "search_symbols",
                args.codebase.as_deref(),
                args.copy.as_deref(),
            )
            .await?;
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
    async fn type_hierarchy(
        &self,
        Parameters(args): Parameters<TypeHierarchyArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("type_hierarchy", args.codebase.as_deref())
            .await?;
        query::type_hierarchy(
            &client,
            &args.symbol,
            args.direction.as_deref().unwrap_or("Both"),
            args.depth.unwrap_or(4),
        )
        .await
    }

    #[tool]
    async fn call_graph(
        &self,
        Parameters(args): Parameters<CallGraphArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("call_graph", args.codebase.as_deref())
            .await?;
        query::call_graph(
            &client,
            &args.symbol,
            args.depth.unwrap_or(2),
            args.direction.as_deref().unwrap_or("Both"),
        )
        .await
    }

    #[tool]
    async fn cycles(&self, Parameters(args): Parameters<NoArgs>) -> Result<String, ToolError> {
        let client = self.client_for("cycles", args.codebase.as_deref()).await?;
        query::cycles(&client).await
    }

    #[tool]
    async fn unused(
        &self,
        Parameters(args): Parameters<AnalysisPageArgs>,
    ) -> Result<String, ToolError> {
        let client = self.client_for("unused", args.codebase.as_deref()).await?;
        query::unused(
            &client,
            args.page.unwrap_or(0),
            args.page_size.unwrap_or(100),
        )
        .await
    }

    #[tool]
    async fn duplicates(&self, Parameters(args): Parameters<NoArgs>) -> Result<String, ToolError> {
        let client = self
            .client_for("duplicates", args.codebase.as_deref())
            .await?;
        query::duplicates(&client).await
    }

    #[tool]
    async fn rename_symbol(
        &self,
        Parameters(args): Parameters<RenameSymbolArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("rename_symbol", args.codebase.as_deref())
            .await?;
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
            Err(error) => Err(ToolError::from_client(
                "rename_symbol",
                &error.context("planning"),
            )),
        }
    }

    #[tool]
    async fn safe_delete_symbol(
        &self,
        Parameters(args): Parameters<SafeDeleteSymbolArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("safe_delete_symbol", args.codebase.as_deref())
            .await?;
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
            Err(error) => Err(ToolError::from_client(
                "safe_delete_symbol",
                &error.context("planning"),
            )),
        }
    }

    #[tool]
    async fn replace_symbol_body(
        &self,
        Parameters(args): Parameters<ReplaceBodyArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("replace_symbol_body", args.codebase.as_deref())
            .await?;
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
            Err(error) => Err(ToolError::from_client(
                "replace_symbol_body",
                &error.context("planning"),
            )),
        }
    }

    #[tool]
    async fn insert_before_symbol(
        &self,
        Parameters(args): Parameters<InsertSymbolArgs>,
    ) -> Result<String, ToolError> {
        self.execute_insert(args, true).await
    }

    #[tool]
    async fn insert_after_symbol(
        &self,
        Parameters(args): Parameters<InsertSymbolArgs>,
    ) -> Result<String, ToolError> {
        self.execute_insert(args, false).await
    }

    #[tool]
    async fn undo_edit(
        &self,
        Parameters(args): Parameters<UndoEditArgs>,
    ) -> Result<String, ToolError> {
        let client = self
            .client_for("undo_edit", args.codebase.as_deref())
            .await?;
        let watching = self.watcher_active(&client).await;
        match crate::editing::undo(&client, &args.edit_id, watching).await {
            Ok(outcome) => {
                // As in `apply_server_plan`: the bytes changed, so ask for the
                // reconcile rather than waiting for the watcher.
                self.trigger_sync(&client).await;
                Ok(applied_edit_text("undo_edit", &outcome))
            }
            Err(error) => Err(ToolError::new(
                "undo_edit",
                FailureKind::Refused,
                format!("{error:#}"),
            )),
        }
    }

    #[tool]
    async fn index_codebase(
        &self,
        Parameters(args): Parameters<IndexCodebaseArgs>,
    ) -> Result<String, ToolError> {
        let requested = match args.path.as_deref() {
            Some(path) => PathBuf::from(path),
            None => self.dir().await.clone(),
        };
        let dir = canonical_directory("index_codebase", &self.shared.context.cwd, &requested)?;
        // The sync manifest represents the complete Git working copy. Use that
        // same root for consent/cache lookup, readiness gates, checkout headers,
        // and watching; otherwise indexing from `repo/src` records one path but
        // later launches from `repo` incorrectly look unindexed.
        let dir = crate::codebase::working_copy_root(&dir).await;

        // A concurrent or recent first-index call owns the gate. Await it
        // rather than queueing another full upload. A gate that FAILED is not
        // awaited: it falls through below, where the engine replaces it with a
        // fresh one, so a failed first index stays retryable while a partial
        // one can never be reported as ready.
        if let Some(gate) = initial_gate_for_path(&self.shared.leases, &dir).await
            && !matches!(gate.outcome().await, Some(Err(_)))
        {
            return match gate.wait().await {
                Ok(()) => Ok(format!(
                    "initial indexing complete\npath {}\nretrieval tools are now available",
                    dir.display()
                )),
                Err(e) => Err(ToolError::new(
                    "index_codebase",
                    FailureKind::IndexFailed,
                    format!("initial indexing failed for {}: {e}", dir.display()),
                )),
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
                if !self.shared.pinned && self.dir().await == &dir {
                    *self.shared.bound.lock().await = Some(client.clone());
                }
                self.watch_once(client, dir.clone()).await;
                return Ok(format!(
                    "codebase {} was already indexed; background sync and watching started\npath {}",
                    resolved.id,
                    dir.display()
                ));
            }
            Ok(None) => {}
            Err(e) => {
                return Err(ToolError::from_client(
                    "index_codebase",
                    &e.context(dir.display().to_string()),
                ));
            }
        }

        // Claim the checkout and its gate before anything is registered on the
        // server. The coordinator waits for the codebase this call registers
        // instead of registering one of its own, so one first index creates one
        // codebase. The gate is published for this session's readiness checks
        // at the same time, so a concurrent path-scoped retrieval call already
        // has something to wait on.
        let first_index_client = self
            .shared
            .base
            .clone()
            .without_codebase()
            .with_local_root(Some(dir.clone()));
        let gate = match self.watch_first_once(first_index_client, dir.clone()).await {
            Ok(gate) => gate,
            Err(e) => {
                return Err(ToolError::new(
                    "index_codebase",
                    FailureKind::Unavailable,
                    format!("{}: {e}", dir.display()),
                ));
            }
        };
        if !gate.claim_registration().await {
            return match gate.wait().await {
                Ok(()) => Ok(format!(
                    "initial indexing complete\npath {}\nretrieval tools are now available",
                    dir.display()
                )),
                Err(e) => Err(ToolError::new(
                    "index_codebase",
                    FailureKind::IndexFailed,
                    format!("initial indexing failed for {}: {e}", dir.display()),
                )),
            };
        }

        let id = match crate::codebase::ensure(&self.shared.base, &dir).await {
            Ok(id) => id,
            Err(e) => {
                let reason = format!("{e:#}");
                gate.finish(Err(reason)).await;
                return Err(ToolError::from_client(
                    "index_codebase",
                    &e.context(dir.display().to_string()),
                ));
            }
        };
        gate.register_codebase(id.clone()).await;
        let client = self
            .shared
            .base
            .clone()
            .with_codebase(id.clone())
            .with_local_root(Some(dir.clone()));
        if !self.shared.pinned && self.dir().await == &dir {
            *self.shared.bound.lock().await = Some(client.clone());
        }
        match gate.wait().await {
            Ok(()) => Ok(format!(
                "initial indexing complete\ncodebase {id}\npath {}\nretrieval tools are now available",
                dir.display()
            )),
            Err(e) => Err(ToolError::new(
                "index_codebase",
                FailureKind::IndexFailed,
                format!(
                    "initial indexing failed for {} (codebase {id}): {e}",
                    dir.display()
                ),
            )),
        }
    }

    #[tool]
    async fn sync_status(&self, Parameters(args): Parameters<NoArgs>) -> Result<String, ToolError> {
        let client = self
            .client_for_unchecked("sync_status", args.codebase.as_deref())
            .await?;
        if let Err(e) = client.codebase() {
            return Err(ToolError::from_client("sync_status", &e));
        }
        // Reported from the coordinator that owns this checkout, not from
        // per-session bookkeeping: any session's sync is this index's sync.
        let status = self.checkout_status(&client).await;
        let watching = status.is_some();
        query::sync_status(
            &client,
            status
                .as_ref()
                .and_then(|status| status.last_job_id.as_deref()),
            watching,
        )
        .await
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
        // Hosts can prepend server instructions to every tool description.
        // The session hook delivers server.md once per context segment instead.
        info
    }
}

pub(super) fn router() -> ToolRouter<McpServer> {
    McpServer::tool_router()
}

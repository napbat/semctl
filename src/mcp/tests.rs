//! Unit and drift guards for the MCP server.
//!
//! Per-tool docs (`docs/tools/*.md`) describe each tool. Session hooks deliver
//! the server manual (`docs/instructions/server.md`). The shared retrieval
//! skill lives at `plugins/semctx/skills/codebase-retrieval/SKILL.md`.
//! These tests check all three against the router to detect missing
//! documentation and references to tools that were renamed or removed.

use std::time::Duration;

use super::tool_types::{
    AnalysisPageArgs, BatchArgs, CallGraphArgs, CallPathArgs, ExpandArgs, FlowBetweenArgs,
    FlowFromArgs, FlowToArgs, GrepArgs, InsertSymbolArgs, ListFilesArgs, NoArgs, OutlineArgs,
    ReadSourceArgs, ReferenceArgs, RenameSymbolArgs, ReplaceBodyArgs, SafeDeleteSymbolArgs,
    SearchArgs, SymbolArgs, SymbolAtPositionArgs, SymbolSearchArgs, TraceArgs, TypeHierarchyArgs,
    UndoEditArgs, render_edit_action_outcome,
};
use super::{
    DIRECT_EDIT_TOOLS, InitialIndexGate, InitialIndexes, McpServer, client, initial_gate_for_path,
    initial_job_result, ready_for_codebases,
};

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
async fn first_index_completion_releases_active_waiters() {
    let gate = InitialIndexGate::pending();
    let (first, second, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; gate.wait(), gate.wait(), gate.finish(Ok(())))
    })
    .await
    .expect("completion must acquire the mutex while retrieval waits");
    assert_eq!(first, Ok(()));
    assert_eq!(second, Ok(()));
}

#[tokio::test]
async fn first_index_failure_releases_an_active_waiter() {
    let gate = InitialIndexGate::pending();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; gate.wait(), gate.finish(Err("embedding failed".into())))
    })
    .await
    .expect("failure must acquire the mutex while retrieval waits");
    assert_eq!(result, Err("embedding failed".into()));
}

#[tokio::test]
async fn scoped_readiness_allows_registration_and_holds_new_indexes_out() {
    let indexes = tokio::sync::RwLock::new(InitialIndexes::default());
    let gate = std::sync::Arc::new(InitialIndexGate::pending());
    indexes
        .write()
        .await
        .by_path
        .insert("checkout".into(), gate.clone());
    let searching = async {
        let guard = ready_for_codebases(&indexes, &["A".into()]).await.unwrap();
        assert!(
            indexes.try_write().is_err(),
            "registration must wait for the query"
        );
        drop(guard);
    };
    let registering = async {
        assert!(
            indexes.try_write().is_ok(),
            "registration must remain available while embedding runs"
        );
        gate.register_codebase("A".into()).await;
        gate.finish(Ok(())).await;
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; searching, registering);
    })
    .await
    .expect("registration and completion must not require the registry write lock");
    assert!(indexes.try_write().is_ok());
}

#[tokio::test]
async fn explicit_ids_ignore_unrelated_pending_and_failed_indexes_after_registration() {
    let mut indexes = InitialIndexes::default();
    let gate = std::sync::Arc::new(InitialIndexGate::pending());
    indexes.by_path.insert("checkout".into(), gate.clone());
    let waiting = async { indexes.wait_for_codebases(&["B".into()]).await };
    let registering = async { gate.register_codebase("A".into()).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; waiting, registering)
    })
    .await
    .expect("the unrelated index need not finish");
    assert_eq!(result, Ok(()));
    gate.finish(Err("embedding failed".into())).await;
    assert_eq!(indexes.wait_for_codebases(&["B".into()]).await, Ok(()));
    assert!(indexes.wait_for_codebases(&["A".into()]).await.is_err());
    assert!(indexes.wait_for_codebases(&[]).await.is_err());
}

#[tokio::test]
async fn scoped_readiness_rechecks_indexes_registered_while_it_waits() {
    let indexes = tokio::sync::RwLock::new(InitialIndexes::default());
    let first = std::sync::Arc::new(InitialIndexGate::pending());
    first.register_codebase("A".into()).await;
    indexes
        .write()
        .await
        .by_path
        .insert("first".into(), first.clone());
    let search = ready_for_codebases(&indexes, &[]);
    tokio::pin!(search);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut search)
            .await
            .is_err()
    );
    let second = std::sync::Arc::new(InitialIndexGate::pending());
    second.register_codebase("B".into()).await;
    indexes
        .write()
        .await
        .by_path
        .insert("second".into(), second.clone());
    first.finish(Ok(())).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut search)
            .await
            .is_err()
    );
    second.finish(Ok(())).await;
    let guard = tokio::time::timeout(Duration::from_secs(1), &mut search)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(guard.by_path.len(), 2);
    assert!(indexes.try_write().is_err());
}

#[tokio::test]
async fn repeated_index_waiting_allows_registration_to_finish() {
    let indexes = tokio::sync::RwLock::new(InitialIndexes::default());
    let gate = std::sync::Arc::new(InitialIndexGate::pending());
    let path = std::path::Path::new("checkout");
    indexes
        .write()
        .await
        .by_path
        .insert(path.to_path_buf(), gate.clone());
    let waiting = async {
        if let Some(gate) = initial_gate_for_path(&indexes, path).await {
            gate.wait().await.unwrap();
        }
    };
    let registering = async {
        gate.register_codebase("codebase".into()).await;
        gate.finish(Ok(())).await;
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; waiting, registering);
    })
    .await
    .expect("an active index request must release the registry lock");
}

#[tokio::test]
async fn first_index_gate_reserves_watcher_startup_before_registration() {
    let base = client::Client::for_test("codebase", None);
    let server = McpServer::new(base, None, false);
    let root = std::path::Path::new("first-checkout");
    let gate = std::sync::Arc::new(InitialIndexGate::pending());
    server
        .shared
        .initial_indexes
        .write()
        .await
        .by_path
        .insert(root.to_path_buf(), gate.clone());

    assert!(!server.claim_untracked_watch(root, "codebase").await);
    assert!(server.shared.watched.lock().await.is_empty());

    gate.finish(Ok(())).await;
    assert!(!server.claim_untracked_watch(root, "codebase").await);
    gate.finish(Err("upload failed".into())).await;
    assert!(!server.claim_untracked_watch(root, "codebase").await);

    let other = std::path::Path::new("other-checkout");
    assert!(server.claim_untracked_watch(other, "other-codebase").await);
    assert!(!server.claim_untracked_watch(other, "other-codebase").await);
}

#[tokio::test]
async fn canonical_search_omits_cached_checkout_freshness() {
    let base = client::Client::for_test("codebase", Some("checkout".into()));
    let server = McpServer::new(base.clone(), None, true);
    server.shared.jobs.lock().await.insert(
        "codebase".into(),
        crate::sync::LastJob {
            job_id: "job".into(),
        },
    );
    *server.shared.freshness.lock().await =
        Some(("job".into(), Some("checkout sync failed".into())));

    assert_eq!(
        server.index_freshness(&base).await,
        Some("checkout sync failed".into())
    );
    assert_eq!(server.index_freshness(&base.for_canonical()).await, None);
}

#[tokio::test]
async fn checkout_readiness_does_not_use_another_checkouts_codebase_gate() {
    let first_root = std::path::PathBuf::from("first-checkout");
    let first = client::Client::for_test("shared-codebase", Some(first_root.clone()));
    let server = McpServer::new(first.clone(), None, false);
    let first_gate = std::sync::Arc::new(InitialIndexGate::pending());
    let second_gate = std::sync::Arc::new(InitialIndexGate::pending());
    first_gate.register_codebase("shared-codebase".into()).await;
    second_gate
        .register_codebase("shared-codebase".into())
        .await;
    second_gate.finish(Ok(())).await;
    {
        let mut indexes = server.shared.initial_indexes.write().await;
        indexes.by_path.insert(first_root, first_gate.clone());
        indexes
            .by_path
            .insert("second-checkout".into(), second_gate.clone());
    }

    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            server.await_initial_client(&first)
        )
        .await
        .is_err()
    );
    first_gate.finish(Ok(())).await;
    second_gate
        .finish(Err("another checkout failed".into()))
        .await;
    assert_eq!(server.await_initial_client(&first).await, Ok(()));
    let rootless = client::Client::for_test("shared-codebase", None);
    assert!(
        server
            .await_initial_client(&rootless)
            .await
            .unwrap_err()
            .contains("another checkout failed")
    );
}

#[tokio::test]
async fn automatic_watching_uses_the_bound_umbrella_root() {
    let umbrella = std::path::PathBuf::from("umbrella");
    let base = client::Client::for_test("codebase", Some(umbrella.clone()));
    let server = McpServer::new(base.clone(), Some(umbrella.join("child")), false);
    let gate = std::sync::Arc::new(InitialIndexGate::pending());
    server
        .shared
        .initial_indexes
        .write()
        .await
        .by_path
        .insert(umbrella, gate);

    server.watch_checkout_once(&base).await;
    assert!(server.shared.watched.lock().await.is_empty());
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

#[test]
fn every_codebase_scoped_argument_schema_has_the_selector() {
    macro_rules! assert_selector {
        ($($ty:ty),+ $(,)?) => {$(
            let schema = schemars::schema_for!($ty);
            let json = serde_json::to_value(schema).expect("schema serializes");
            assert!(
                json.pointer("/properties/codebase").is_some(),
                "{} is missing the optional codebase selector",
                stringify!($ty)
            );
        )+};
    }
    assert_selector!(
        SearchArgs,
        SymbolArgs,
        ReferenceArgs,
        CallPathArgs,
        FlowFromArgs,
        FlowToArgs,
        FlowBetweenArgs,
        TraceArgs,
        GrepArgs,
        OutlineArgs,
        ExpandArgs,
        SymbolAtPositionArgs,
        BatchArgs,
        NoArgs,
        ListFilesArgs,
        ReadSourceArgs,
        SymbolSearchArgs,
        TypeHierarchyArgs,
        CallGraphArgs,
        AnalysisPageArgs,
        RenameSymbolArgs,
        SafeDeleteSymbolArgs,
        ReplaceBodyArgs,
        InsertSymbolArgs,
        UndoEditArgs,
    );
}

/// Registered tool names, straight from the router the MCP host sees.
fn registered_tools() -> Vec<String> {
    super::tools::router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

fn server_instructions() -> &'static str {
    include_str!("docs/instructions/server.md")
}

fn skill_md() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/plugins/semctx/skills/codebase-retrieval/SKILL.md"
    );
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Whole-token backticked identifiers — how tool names appear in prose.
/// Only spans whose entire content is a `snake_case` word count, so
/// `` `semctl index` ``, `` `path:line-range` `` and `` `--codebase` ``
/// are ignored without an allowlist entry.
fn backticked_idents(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| {
            !span.is_empty()
                && span.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && span
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .map(str::to_string)
        .collect()
}

/// Backticked idents that are neither registered tools nor known non-tool
/// vocabulary (parameters, enum values, states). A rename or removal of a
/// tool leaves the old name stranded in prose — this finds it.
fn phantom_tools(text: &str, tools: &[String]) -> Vec<String> {
    // Parameters, arg values, and job states the steering prose legitimately
    // backticks. Extend this list when adding prose, not when adding tools.
    const ALLOWED_NON_TOOLS: &[&str] = &[
        "top_k",
        "prefer",
        "kinds",
        "expand",
        "domains",
        "scope",
        "codebase_ids",
        "depth",
        "regex",
        "ignore_case",
        "pattern",
        "path",
        "max",
        "page",
        "page_size",
        "symbols",
        "references",
        "from",
        "to",
        "line",
        "column",
        "line_start",
        "line_end",
        "symbol",
        "query",
        "lang",
        "kind",
        "function",
        "container",
        "block",
        "code",
        "docs",
        "queued",
        "running",
        "done",
        "failed",
        "semctl",
        "search",
        "stale",
    ];
    // A tool reference is valid either bare (`grep`) or fully-qualified as a
    // supported host sees it. Codex uses `mcp__semctx__grep`, Claude qualifies
    // both plugin and server, and OMP sanitizes its marketplace namespace.
    let is_registered_tool = |t: &str| -> bool {
        let bare = [
            "mcp__semctx__",
            "mcp__plugin_semctx_semctx__",
            "mcp__semctx_semctx_",
        ]
        .iter()
        .find_map(|prefix| t.strip_prefix(prefix))
        .unwrap_or(t);
        tools.iter().any(|n| n == bare)
    };
    let mut out: Vec<String> = backticked_idents(text)
        .into_iter()
        .filter(|t| !is_registered_tool(t))
        .filter(|t| !ALLOWED_NON_TOOLS.contains(&t.as_str()))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn missing_from(text: &str, tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|n| !text.contains(*n))
        .cloned()
        .collect()
}

#[test]
fn every_tool_has_a_doc() {
    for name in registered_tools() {
        assert!(
            McpServer::tool_doc(&name).is_some(),
            "tool `{name}` has no docs/tools/{name}.md arm in tool_doc()"
        );
    }
}

#[test]
fn server_instructions_cover_every_tool() {
    let missing = missing_from(server_instructions(), &registered_tools());
    assert!(
        missing.is_empty(),
        "docs/instructions/server.md never mentions registered tool(s) {missing:?} — \
         update its Tool selection section"
    );
}

#[test]
fn skill_covers_every_tool() {
    let missing = missing_from(&skill_md(), &registered_tools());
    assert!(
        missing.is_empty(),
        "plugins/semctx/skills/codebase-retrieval/SKILL.md never mentions registered tool(s) {missing:?} — \
         update its routing table"
    );
}

#[test]
fn retrieval_guidance_bounds_expansion_and_routes_local_reads() {
    let skill = skill_md();
    let server = server_instructions();
    let search = McpServer::tool_doc("search_codebase").expect("search docs");
    let read_source = McpServer::tool_doc("read_source").expect("read_source docs");

    for (name, text) in [
        ("skill", skill.as_str()),
        ("server", server),
        ("search", search),
    ] {
        assert!(
            text.contains("server") && text.contains("result-content budget"),
            "{name} must expose the server result-content budget"
        );
        assert!(
            text.contains("5–8") && text.contains("expand"),
            "{name} must steer initial search toward focused snippets"
        );
    }
    assert!(
        skill.contains("host `Read`") && read_source.contains("host Read"),
        "known local current bytes belong to the host reader"
    );
    assert!(
        skill.contains("retrieval supplies evidence only"),
        "cross-repository retrieval must not silently authorize edits"
    );
}

#[test]
fn every_tool_has_explicit_safety_annotations() {
    for tool in super::tools::router()
        .list_all()
        .into_iter()
        .map(McpServer::with_doc)
    {
        let annotations = tool
            .annotations
            .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
        assert!(
            annotations.read_only_hint.is_some(),
            "{} must declare readOnlyHint",
            tool.name
        );
        assert!(
            annotations.open_world_hint.is_some(),
            "{} must declare openWorldHint",
            tool.name
        );
        if tool.name == "index_codebase" {
            assert_eq!(annotations.read_only_hint, Some(false));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
        } else if DIRECT_EDIT_TOOLS.contains(&tool.name.as_ref()) {
            assert_eq!(annotations.read_only_hint, Some(false));
            assert_eq!(annotations.destructive_hint, Some(true));
            assert_eq!(annotations.idempotent_hint, Some(tool.name == "undo_edit"));
        } else {
            assert_eq!(annotations.read_only_hint, Some(true));
        }
    }
}

#[test]
fn symbolic_edits_are_direct_actions_not_plan_transport() {
    let tools = registered_tools();
    for name in DIRECT_EDIT_TOOLS {
        assert!(
            tools.iter().any(|tool| tool == name),
            "missing direct edit tool {name}"
        );
    }
    for removed in ["apply_edit_plan", "undo_edit_plan"] {
        assert!(
            !tools.iter().any(|tool| tool == removed),
            "raw plan transport tool {removed} must not be exposed over MCP"
        );
    }
}

#[test]
fn retrieval_skill_has_one_repository_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in ["SKILL.md", "agents/openai.yaml"] {
        assert!(
            root.join("plugins/semctx/skills/codebase-retrieval")
                .join(relative)
                .is_file(),
            "shared retrieval skill is missing {relative}"
        );
        for duplicate in [
            "skills/codebase-retrieval",
            "codex-plugin/skills/codebase-retrieval",
        ] {
            assert!(
                !root.join(duplicate).join(relative).exists(),
                "do not copy shared skill assets into a host adapter"
            );
        }
    }
}

#[test]
fn prior_index_permission_is_durable_in_agent_guidance() {
    for (name, text) in [
        ("server instructions", server_instructions().to_string()),
        ("retrieval skill", skill_md()),
    ] {
        assert!(
            text.contains("previously indexed") && text.contains("without asking again"),
            "{name} must say that an existing index is prior consent"
        );
    }
}

#[test]
fn first_index_gate_is_explained_in_agent_guidance() {
    for (name, text) in [
        ("server instructions", server_instructions().to_string()),
        ("retrieval skill", skill_md()),
    ] {
        assert!(
            text.contains("first-ever") && text.contains("sync_status"),
            "{name} must explain the first-index readiness gate and progress path"
        );
    }
}

#[test]
fn steering_docs_name_no_phantom_tools() {
    let tools = registered_tools();
    let phantoms = phantom_tools(server_instructions(), &tools);
    assert!(
        phantoms.is_empty(),
        "docs/instructions/server.md backticks unknown ident(s) {phantoms:?} — \
         a renamed/removed tool, or a new term for ALLOWED_NON_TOOLS"
    );
    let phantoms = phantom_tools(&skill_md(), &tools);
    assert!(
        phantoms.is_empty(),
        "plugins/semctx/skills/codebase-retrieval/SKILL.md backticks unknown ident(s) {phantoms:?} — \
         a renamed/removed tool, or a new term for ALLOWED_NON_TOOLS"
    );
}

#[test]
fn phantom_detection_actually_fires() {
    let tools = registered_tools();
    let phantoms = phantom_tools("call `made_up_tool` first, then `grep`.", &tools);
    assert_eq!(
        phantoms,
        vec!["made_up_tool".to_string()],
        "the phantom guard must flag unregistered tool names"
    );
}

#[test]
fn phantom_guard_understands_host_mcp_prefixes() {
    let tools = registered_tools();
    // Fully-qualified names map to their bare registered tool.
    for name in [
        "mcp__semctx__grep",
        "mcp__plugin_semctx_semctx__grep",
        "mcp__semctx_semctx_grep",
    ] {
        assert_eq!(
            phantom_tools(&format!("use `{name}`"), &tools),
            Vec::<String>::new()
        );
    }
    // A bogus qualified name is still caught.
    assert_eq!(
        phantom_tools("use `mcp__semctx_semctx_made_up`", &tools),
        vec!["mcp__semctx_semctx_made_up".to_string()]
    );
}

#[test]
fn nudge_copy_names_no_phantom_tools() {
    use crate::commands::hook::message::{self, SearchKind, ToolNameStyle};
    let tools = registered_tools();
    // Exercise EVERY message branch for all host naming styles — tier1 plus
    // all four tier2 tails (symbol, concept, literal, filename).
    for names in [
        ToolNameStyle::CodexPlugin,
        ToolNameStyle::ClaudePlugin,
        ToolNameStyle::OmpMarketplace,
    ] {
        let copy = format!(
            "{} {} {} {} {}",
            message::tier1(names),
            message::tier2(names, SearchKind::Content, 5, Some("symbol")), // Symbol tail
            message::tier2(names, SearchKind::Content, 5, Some("how retry works")), // Concept tail
            message::tier2(names, SearchKind::Content, 5, Some("foo|bar")), // Literal tail
            message::tier2(names, SearchKind::Filename, 5, None),          // Filename tail
        );
        let phantoms = phantom_tools(&copy, &tools);
        assert!(
            phantoms.is_empty(),
            "PreToolUse nudge copy names unknown tool(s) {phantoms:?} for {names:?}"
        );
    }
}

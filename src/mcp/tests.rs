//! Unit and drift guards for the MCP server.
//!
//! Per-tool docs (`docs/tools/*.md`) describe each tool. Session hooks deliver
//! the server manual (`docs/instructions/server.md`). The shared retrieval
//! skill lives at `plugins/semctx/skills/codebase-retrieval/SKILL.md`.
//! These tests check all three against the router to detect missing
//! documentation and references to tools that were renamed or removed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::readiness::wait_for_gates;
use super::tool_types::{
    AnalysisPageArgs, BatchArgs, CallGraphArgs, CallPathArgs, ExpandArgs, FlowBetweenArgs,
    FlowFromArgs, FlowToArgs, GrepArgs, InsertSymbolArgs, ListFilesArgs, NoArgs, OutlineArgs,
    ReadSourceArgs, ReferenceArgs, RenameSymbolArgs, ReplaceBodyArgs, SafeDeleteSymbolArgs,
    SearchArgs, SymbolArgs, SymbolAtPositionArgs, SymbolSearchArgs, TraceArgs, TypeHierarchyArgs,
    UndoEditArgs, render_edit_action_outcome,
};
use super::{
    DIRECT_EDIT_TOOLS, InitialIndexGate, McpServer, client, initial_gate_for_path,
    initial_index_failed, ready_for_codebases, selector_is_path_like,
};
use crate::engine::Engine;
use crate::engine::coordinator::{CheckoutCoordinator, IdleReconciler};
use crate::session::SessionContext;

/// An MCP server for one throwaway session.
///
/// These tests never read the session's working directory, so a path that does
/// not exist serves as the launch root. The engine counts reconciles instead of
/// performing them, and a root that does not exist also leaves every coordinator
/// without a platform watcher, so no test touches the filesystem watcher.
fn server(base: client::Client, dir: impl Into<PathBuf>, pinned: bool) -> McpServer {
    session(
        base,
        dir,
        pinned,
        Engine::for_test(Arc::new(IdleReconciler)),
    )
}

/// One session on a shared engine, for tests about what one session sees.
fn session(
    base: client::Client,
    dir: impl Into<PathBuf>,
    pinned: bool,
    engine: Arc<Engine>,
) -> McpServer {
    McpServer::with_parts(SessionContext::for_test(), base, dir.into(), pinned, engine)
}

/// Claim `root` for a first index on this session's behalf, as
/// `index_codebase` does, and return the gate retrieval waits on.
async fn first_index(server: &McpServer, codebase: &str, root: &Path) -> Arc<InitialIndexGate> {
    let client = client::Client::for_test(codebase, Some(root.to_path_buf()));
    server
        .watch_first_once(client, root.to_path_buf())
        .await
        .expect("claim the first index")
}

/// Attach this session to `root`, as a resolved tool call does, and return the
/// coordinator the engine gave it.
async fn attach(
    server: &McpServer,
    client: &client::Client,
    root: &Path,
) -> Arc<CheckoutCoordinator> {
    let key = server
        .attach(client.clone(), root.to_path_buf())
        .await
        .expect("attach the checkout");
    server
        .shared
        .leases
        .read()
        .await
        .get(&key)
        .expect("the session keeps the lease it took")
        .coordinator()
        .clone()
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

/// A gate is reported once. A later report describes another run of the same
/// first index, and accepting it would turn a succeeded index into a failed
/// one for every session that already read the result.
#[tokio::test]
async fn one_run_claims_the_gate_poll_and_the_first_result_stands() {
    let gate = InitialIndexGate::pending();
    assert!(gate.claim_poll().await, "the first run polls this gate");
    assert!(
        !gate.claim_poll().await,
        "a second run must not start a second poll"
    );

    gate.finish(Ok(())).await;
    gate.finish(Err("a later run failed".into())).await;
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

/// A query reserves the membership it checked, so a checkout attached during
/// the query cannot join its scope. Attaching must stay possible while
/// embedding runs, or a first index would block every other tool call.
#[tokio::test]
async fn scoped_readiness_allows_registration_and_holds_new_indexes_out() {
    let server = server(client::Client::for_test("codebase", None), "launch", false);
    let gate = first_index(&server, "codebase", Path::new("checkout")).await;
    let leases = &server.shared.leases;
    let searching = async {
        let guard = ready_for_codebases(leases, &["A".into()])
            .await
            .expect("the gate completes");
        assert!(
            leases.try_write().is_err(),
            "attaching a checkout must wait for the query"
        );
        drop(guard);
    };
    let registering = async {
        assert!(
            leases.try_write().is_ok(),
            "attaching must remain available while embedding runs"
        );
        gate.register_codebase("A".into()).await;
        gate.finish(Ok(())).await;
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; searching, registering);
    })
    .await
    .expect("registration and completion must not require the lease write lock");
    assert!(leases.try_write().is_ok());
}

/// A first index belongs to the checkout's coordinator, which every session on
/// that checkout shares. A second session's startup bind must not wait for it:
/// in the daemon that would hold this session's `initialize` behind another
/// session's embedding, and behind a gate that failed it would hold it for the
/// life of the session.
#[tokio::test]
async fn a_second_sessions_startup_bind_does_not_wait_for_another_first_index() {
    let engine = Engine::for_test(Arc::new(IdleReconciler));
    let root = PathBuf::from("shared-checkout");
    let first = session(
        client::Client::for_test("codebase", Some(root.clone())),
        "first-launch",
        true,
        engine.clone(),
    );
    // Never finished: this is the first session's embedding, still running.
    let _pending = first_index(&first, "codebase", &root).await;

    let base = client::Client::for_test("codebase", Some(root.clone()));
    let second = session(base.clone(), root.clone(), true, engine);
    attach(&second, &base, &root).await;

    tokio::time::timeout(Duration::from_secs(1), second.bind_at_startup())
        .await
        .expect("the startup bind must not wait for another session's first index");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), second.bound())
            .await
            .is_err(),
        "a retrieval call still waits for the first index"
    );
}

/// A failed first index stays failed until something asks for that index
/// again, so the message a retrieval call reports must name the recovery.
#[test]
fn a_failed_first_index_reports_how_to_retry() {
    let message = initial_index_failed("embedding job 7 failed");

    assert!(message.contains("embedding job 7 failed"), "{message}");
    assert!(
        message.contains("call `index_codebase` for this path to retry"),
        "{message}"
    );
}

/// A bare relative selector names a directory under the session's working
/// directory, never under the process working directory: one daemon serves
/// sessions invoked from many trees, and its own directory is not any
/// session's.
#[test]
fn a_relative_selector_is_a_path_under_the_session_directory() {
    let session_dir = tempfile::tempdir().expect("temporary session directory");
    std::fs::create_dir(session_dir.path().join("sub")).expect("create the checkout");
    let elsewhere = tempfile::tempdir().expect("another session directory");

    assert!(selector_is_path_like(session_dir.path(), "sub"));
    assert!(
        !selector_is_path_like(elsewhere.path(), "sub"),
        "another session's directory holds no such checkout"
    );
    assert!(!selector_is_path_like(session_dir.path(), "codebase-id"));
    for raw in [".", "..", "a/b", "/absolute"] {
        assert!(selector_is_path_like(elsewhere.path(), raw), "{raw}");
    }
}

/// A named scope must not wait for a checkout it cannot include, before or
/// after that checkout's first index fails.
#[tokio::test]
async fn explicit_ids_ignore_unrelated_pending_and_failed_indexes_after_registration() {
    let gate = Arc::new(InitialIndexGate::pending());
    let gates = vec![gate.clone()];
    let waiting = async { wait_for_gates(&gates, &["B".into()]).await };
    let registering = async { gate.register_codebase("A".into()).await };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(biased; waiting, registering)
    })
    .await
    .expect("the unrelated index need not finish");
    assert_eq!(result, Ok(()));

    gate.finish(Err("embedding failed".into())).await;
    assert_eq!(wait_for_gates(&gates, &["B".into()]).await, Ok(()));
    assert!(wait_for_gates(&gates, &["A".into()]).await.is_err());
    assert!(
        wait_for_gates(&gates, &[]).await.is_err(),
        "a server-defined scope can include it, so its failure counts"
    );
}

/// A checkout attached while the query waits must be waited for too: its
/// codebase would otherwise be searched before its first index finished.
#[tokio::test]
async fn scoped_readiness_rechecks_indexes_registered_while_it_waits() {
    let server = server(client::Client::for_test("A", None), "launch", false);
    let first = first_index(&server, "A", Path::new("first")).await;
    first.register_codebase("A".into()).await;

    let search = ready_for_codebases(&server.shared.leases, &[]);
    tokio::pin!(search);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut search)
            .await
            .is_err()
    );

    let second = first_index(&server, "B", Path::new("second")).await;
    second.register_codebase("B".into()).await;
    first.finish(Ok(())).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut search)
            .await
            .is_err(),
        "the checkout attached while waiting must also be ready"
    );

    second.finish(Ok(())).await;
    let guard = tokio::time::timeout(Duration::from_secs(1), &mut search)
        .await
        .expect("both gates completed")
        .expect("both first indexes succeeded");
    assert_eq!(guard.len(), 2);
    assert!(server.shared.leases.try_write().is_err());
}

/// Waiting for one checkout must not hold the lease map: registration needs it.
#[tokio::test]
async fn repeated_index_waiting_allows_registration_to_finish() {
    let server = server(client::Client::for_test("codebase", None), "launch", false);
    let path = Path::new("checkout");
    let gate = first_index(&server, "codebase", path).await;
    let waiting = async {
        if let Some(gate) = initial_gate_for_path(&server.shared.leases, path).await {
            gate.wait().await.expect("the first index succeeds");
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
    .expect("an active index request must release the lease map");
}

/// A session waits for the checkouts it brought in, and for no others. One
/// host's first index must never block another host's search.
#[tokio::test]
async fn an_empty_selector_waits_only_on_this_sessions_gates() {
    let engine = Engine::for_test(Arc::new(IdleReconciler));
    let mine = session(
        client::Client::for_test("mine", None),
        "my-launch",
        false,
        engine.clone(),
    );
    let theirs = session(
        client::Client::for_test("theirs", None),
        "their-launch",
        false,
        engine,
    );
    // The other session's first index never finishes.
    let _their_gate = first_index(&theirs, "theirs", Path::new("their-checkout")).await;
    let my_gate = first_index(&mine, "mine", Path::new("my-checkout")).await;
    my_gate.finish(Ok(())).await;

    let guard = tokio::time::timeout(
        Duration::from_secs(1),
        ready_for_codebases(&mine.shared.leases, &[]),
    )
    .await
    .expect("a gate this session does not hold must not block it")
    .expect("this session's own first index succeeded");

    assert_eq!(guard.len(), 1);
}

/// A first index owns its checkout before the codebase is registered. A plain
/// path-scoped call on the same checkout must join that coordinator, not start
/// a second one, and must not disturb the gate retrieval is waiting on.
#[tokio::test]
async fn a_plain_watch_joins_the_first_index_coordinator() {
    let root = PathBuf::from("first-checkout");
    let base = client::Client::for_test("codebase", Some(root.clone()));
    let server = server(base.clone(), "launch", false);
    let gate = server
        .watch_first_once(base.clone(), root.clone())
        .await
        .expect("claim the first index");

    server.watch_once(base.clone(), root.clone()).await;

    let leases = server.shared.leases.read().await;
    assert_eq!(leases.len(), 1, "one checkout is one coordinator");
    let coordinator = leases
        .values()
        .next()
        .expect("the session holds the lease")
        .coordinator();
    assert!(
        Arc::ptr_eq(
            &coordinator
                .gate()
                .await
                .expect("the coordinator keeps the first-index gate"),
            &gate
        ),
        "a plain watch must not replace the gate retrieval waits on"
    );
    drop(leases);

    // A second checkout is a second coordinator.
    let other_root = PathBuf::from("other-checkout");
    let other = client::Client::for_test("other-codebase", Some(other_root.clone()));
    server.watch_once(other, other_root).await;
    assert_eq!(server.shared.leases.read().await.len(), 2);
}

#[tokio::test]
async fn canonical_search_omits_cached_checkout_freshness() {
    let base = client::Client::for_test("codebase", Some("checkout".into()));
    let server = server(base.clone(), "launch", true);
    let coordinator = attach(&server, &base, Path::new("checkout")).await;
    coordinator.set_last_job("job").await;
    *server.shared.freshness.lock().await =
        Some(("job".into(), Some("checkout sync failed".into())));

    assert_eq!(
        server.index_freshness(&base).await,
        Some("checkout sync failed".into())
    );
    assert_eq!(server.index_freshness(&base.for_canonical()).await, None);
}

/// Two checkouts can share one codebase id. A bound checkout waits for its own
/// first index; a request with no checkout waits for every one of them.
#[tokio::test]
async fn checkout_readiness_does_not_use_another_checkouts_codebase_gate() {
    let first_root = PathBuf::from("first-checkout");
    let first = client::Client::for_test("shared-codebase", Some(first_root.clone()));
    let server = server(first.clone(), "launch", false);
    let first_gate = first_index(&server, "shared-codebase", &first_root).await;
    let second_gate = first_index(&server, "shared-codebase", Path::new("second-checkout")).await;
    first_gate.register_codebase("shared-codebase".into()).await;
    second_gate
        .register_codebase("shared-codebase".into())
        .await;
    second_gate
        .finish(Err("another checkout failed".into()))
        .await;

    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            server.await_initial_client(&first)
        )
        .await
        .is_err(),
        "another checkout's first index must not answer for this one"
    );
    first_gate.finish(Ok(())).await;
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

/// The bound checkout can be an umbrella root above the launch directory. The
/// sync manifest is complete desired state, so the watched root must be the
/// umbrella the client is bound to, never the nested launch directory.
#[tokio::test]
async fn automatic_watching_uses_the_bound_umbrella_root() {
    let umbrella = PathBuf::from("umbrella");
    let base = client::Client::for_test("codebase", Some(umbrella.clone()));
    let server = server(base.clone(), umbrella.join("child"), false);

    server.watch_checkout_once(&base).await;

    let leases = server.shared.leases.read().await;
    let roots: Vec<_> = leases
        .values()
        .map(|lease| lease.coordinator().root().to_path_buf())
        .collect();
    assert_eq!(roots, vec![umbrella]);
}

#[tokio::test]
async fn first_index_gate_propagates_failure() {
    let gate = InitialIndexGate::pending();
    gate.finish(Err("embedding failed".into())).await;
    assert_eq!(gate.wait().await, Err("embedding failed".into()));
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

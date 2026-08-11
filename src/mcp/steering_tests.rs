//! Drift guards for the prose surfaces that steer agents to the MCP tools.
//!
//! Three surfaces describe the tool inventory: the per-tool docs
//! (`docs/tools/*.md`, compiled in via `tool_doc`), the server-level
//! instructions (`docs/instructions/server.md`), and the shared coding-agent
//! skill (`plugins/semctx/skills/codebase-retrieval/SKILL.md`). The last
//! two are hand-written and drift silently — the skill sat at 8 of 23 tools
//! for a month while the registry grew. These tests pin both files to the
//! router: adding, renaming, or removing a tool without updating them fails
//! `cargo test`.

use super::{
    AnalysisPageArgs, BatchArgs, CallGraphArgs, CallPathArgs, DIRECT_EDIT_TOOLS, ExpandArgs,
    FlowBetweenArgs, FlowFromArgs, FlowToArgs, GrepArgs, InsertSymbolArgs, ListFilesArgs,
    McpServer, NoArgs, OutlineArgs, ReadSourceArgs, ReferenceArgs, RenameSymbolArgs,
    ReplaceBodyArgs, SafeDeleteSymbolArgs, SearchArgs, SymbolArgs, SymbolAtPositionArgs,
    SymbolSearchArgs, TraceArgs, TypeHierarchyArgs, UndoEditArgs,
};

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
    McpServer::tool_router()
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
    // supported host sees it. Claude/Codex use `mcp__semctx__grep`; OMP
    // namespaces the marketplace server and uses `mcp__semctx_semctx_grep`.
    let is_registered_tool = |t: &str| -> bool {
        let bare = ["mcp__semctx__", "mcp__semctx_semctx_"]
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
fn every_tool_has_explicit_safety_annotations() {
    for tool in McpServer::tool_router()
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
    for name in ["mcp__semctx__grep", "mcp__semctx_semctx_grep"] {
        assert!(phantom_tools(&format!("use `{name}`"), &tools).is_empty());
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
    // Exercise EVERY message branch for both host naming styles — tier1 plus
    // all four tier2 tails (symbol, concept, literal, filename).
    for names in [
        ToolNameStyle::ClaudeCompatible,
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

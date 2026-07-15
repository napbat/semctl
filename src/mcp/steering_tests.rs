//! Drift guards for the prose surfaces that steer agents to the MCP tools.
//!
//! Three surfaces describe the tool inventory: the per-tool docs
//! (`docs/tools/*.md`, compiled in via `tool_doc`), the server-level
//! instructions (`docs/instructions/server.md`), and the Claude Code plugin
//! skill (`skills/codebase-retrieval/SKILL.md` at the repo root). The last
//! two are hand-written and drift silently — the skill sat at 8 of 23 tools
//! for a month while the registry grew. These tests pin both files to the
//! router: adding, renaming, or removing a tool without updating them fails
//! `cargo test`.

use super::McpServer;

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
        "/skills/codebase-retrieval/SKILL.md"
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
    // A tool reference is valid either bare (`grep`) or fully-qualified as the
    // host sees it (`mcp__semctx__grep`) — the PreToolUse nudge copy uses the
    // latter to disambiguate from the built-in Grep tool and shell grep.
    let is_registered_tool = |t: &str| -> bool {
        let bare = t.strip_prefix("mcp__semctx__").unwrap_or(t);
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
        "skills/codebase-retrieval/SKILL.md never mentions registered tool(s) {missing:?} — \
         update its routing table"
    );
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
        "skills/codebase-retrieval/SKILL.md backticks unknown ident(s) {phantoms:?} — \
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
fn phantom_guard_understands_mcp_prefix() {
    let tools = registered_tools();
    // Fully-qualified names map to their bare registered tool.
    assert!(phantom_tools("use `mcp__semctx__grep`", &tools).is_empty());
    // A bogus qualified name is still caught.
    assert_eq!(
        phantom_tools("use `mcp__semctx__made_up`", &tools),
        vec!["mcp__semctx__made_up".to_string()]
    );
}

#[test]
fn nudge_copy_names_no_phantom_tools() {
    use crate::commands::hook::message::{self, SearchKind};
    let tools = registered_tools();
    // Exercise EVERY message branch — tier1 plus all four tier2 tails (symbol,
    // concept, literal, filename) — so a phantom tool hiding in any single
    // branch is caught. The symbol case uses `symbol` (already allowed vocab) so
    // the dynamically-backticked identifier doesn't trip the guard itself.
    let copy = format!(
        "{} {} {} {} {}",
        message::tier1(),
        message::tier2(SearchKind::Content, 5, Some("symbol")), // Symbol tail
        message::tier2(SearchKind::Content, 5, Some("how retry works")), // Concept tail
        message::tier2(SearchKind::Content, 5, Some("foo|bar")), // Literal tail
        message::tier2(SearchKind::Filename, 5, None),          // Filename tail
    );
    let phantoms = phantom_tools(&copy, &tools);
    assert!(
        phantoms.is_empty(),
        "PreToolUse nudge copy names unknown tool(s) {phantoms:?} — a renamed/removed tool"
    );
}

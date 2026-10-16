//! Host display titles and status for the existing MCP tool results.

use rmcp::model::CallToolResult;

/// Display titles do not change the stable tool names used by clients.
pub(super) fn tool_title(name: &str) -> Option<&'static str> {
    Some(match name {
        "search_codebase" => "Search code",
        "find_definition" => "Find definitions",
        "find_references" => "Find references",
        "who_calls" => "Find callers",
        "implementations_of" => "Find implementations",
        "call_path" => "Trace a call path",
        "reaches" => "Trace outgoing data flow",
        "flows_into" => "Trace incoming data flow",
        "flows_between" => "Trace data flow between symbols",
        "trace" => "Trace a symbol",
        "grep" => "Search text",
        "file_outline" => "Show file outline",
        "expand_chunk" => "Expand source context",
        "symbol_at_position" => "Find symbol at position",
        "batch_lookup" => "Look up symbols",
        "file_tree" => "Show file tree",
        "list_files" => "List indexed files",
        "list_projects" => "List projects",
        "imports" => "Show imports",
        "symbol_edges" => "Show symbol connections",
        "external_links" => "Show external references",
        "list_domains" => "List search domains",
        "index_codebase" => "Index a codebase",
        "sync_status" => "Show index status",
        "list_codebases" => "List codebases",
        "current_context" => "Show codebase context",
        "read_source" => "Read indexed source",
        "search_symbols" => "Search symbols",
        "type_hierarchy" => "Show type hierarchy",
        "call_graph" => "Show call graph",
        "cycles" => "Find call cycles",
        "unused" => "Find unused symbols",
        "duplicates" => "Find duplicate code",
        "rename_symbol" => "Rename a symbol",
        "safe_delete_symbol" => "Delete a symbol",
        "replace_symbol_body" => "Replace a symbol body",
        "insert_before_symbol" => "Insert before a symbol",
        "insert_after_symbol" => "Insert after a symbol",
        "undo_edit" => "Undo a symbol edit",
        _ => return None,
    })
}

/// Keep the text contract intact while exposing failures to host tool cards.
/// Query failures use the same discriminator as the CLI. MCP-only failures
/// start with the called tool name, except for the initial-index status.
pub(super) fn mark_failure(name: &str, result: &mut CallToolResult) {
    let Some(text) = result.content.first().and_then(|content| content.as_text()) else {
        return;
    };
    let first_line = text.text.lines().next().unwrap_or_default();
    let tool_failure = first_line.strip_prefix(name).is_some_and(|rest| {
        [
            " unavailable — ",
            " planning failed:",
            " result render failed:",
            " refused:",
            " failed for ",
        ]
        .iter()
        .any(|prefix| rest.starts_with(prefix))
    });
    let index_failure = name == "index_codebase"
        && (first_line == "initial indexing failed"
            || first_line.starts_with("initial indexing failed for "));
    if crate::query::is_failure(&text.text) || tool_failure || index_failure {
        result.is_error = Some(true);
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::{CallToolResult, Content};

    use super::mark_failure;

    #[test]
    fn failures_keep_their_text_and_set_the_host_error_status() {
        for (name, text) in [
            ("search_codebase", "search failed: server unreachable"),
            (
                "read_source",
                "read_source failed: request either bytes or lines, not both",
            ),
            (
                "find_definition",
                "find_definition unavailable — not logged in",
            ),
            (
                "rename_symbol",
                "rename_symbol planning failed: ambiguous symbol",
            ),
            ("rename_symbol", "rename_symbol refused: source changed"),
            (
                "undo_edit",
                "undo_edit result render failed: invalid result",
            ),
            (
                "index_codebase",
                "index_codebase failed for C:\\repo: server unreachable",
            ),
            (
                "index_codebase",
                "initial indexing failed\nerror: embedding failed",
            ),
            (
                "index_codebase",
                "initial indexing failed for C:\\repo: embedding failed",
            ),
        ] {
            let content = vec![Content::text(text)];
            let mut result = CallToolResult::success(content.clone());
            mark_failure(name, &mut result);
            assert_eq!(result.is_error, Some(true), "{text}");
            assert_eq!(result.content, content);
        }
    }

    #[test]
    fn empty_results_source_text_and_freshness_warnings_remain_successful() {
        for (name, text) in [
            ("search_codebase", "no results"),
            (
                "search_codebase",
                "**1 match**\n\nsearch failed: is source text",
            ),
            (
                "sync_status",
                "last sync run job-1: failed\n  error: embedding failed",
            ),
            (
                "search_codebase",
                "src/lib.rs:1\n\n(index freshness: the last sync FAILED)",
            ),
            ("index_codebase", "initial indexing complete\npath C:\\repo"),
            ("read_source", "rename_symbol refused: this is source text"),
        ] {
            let mut result = CallToolResult::success(vec![Content::text(text)]);
            mark_failure(name, &mut result);
            assert_eq!(result.is_error, Some(false), "{text}");
        }
    }

    #[test]
    fn existing_error_status_is_preserved_without_text() {
        let mut result = CallToolResult::error(Vec::new());
        mark_failure("read_source", &mut result);
        assert_eq!(result.is_error, Some(true));
    }
}

//! The nudge copy. Static and local. Tool names are fully qualified so they
//! stay unambiguous against the built-in search tools; the exact prefix is a
//! host contract because OMP namespaces marketplace MCP servers differently
//! from Claude Code and Codex.
//!
//! The message never claims the built-in Grep/Glob "miss unopened files" (they
//! search the working tree). It argues semctl's real edge: the symbol graph,
//! semantic retrieval, ranking, cheaper focused results, indexed literal search,
//! and — for filename enumeration — the indexed file listing.

/// Whether the built-in search was over file *content* or file *names* — a Glob
/// / `find` / `Get-ChildItem -Recurse` is enumeration, and steering it toward a
/// content tool like `grep` would be wrong advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Content,
    Filename,
}

/// How the host exposes tools from the `semctx` MCP server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolNameStyle {
    /// Claude Code and Codex use `mcp__semctx__<tool>`.
    #[default]
    ClaudeCompatible,
    /// OMP namespaces a marketplace server as `semctx:semctx`, which its tool
    /// bridge sanitizes to `mcp__semctx_semctx_<tool>`.
    OmpMarketplace,
}

impl ToolNameStyle {
    const fn prefix(self) -> &'static str {
        match self {
            Self::ClaudeCompatible => "mcp__semctx__",
            Self::OmpMarketplace => "mcp__semctx_semctx_",
        }
    }
}

/// What the (content) search pattern looks like, so Tier 2 can name the right tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    /// A single bare identifier — a symbol.
    Symbol,
    /// Multiple words — a concept for semantic search.
    Concept,
    /// A regex / literal string — an indexed literal sweep.
    Literal,
    /// No usable pattern (e.g. a shell command) — stay generic.
    Unknown,
}

fn classify(pattern: Option<&str>) -> QueryKind {
    let Some(p) = pattern.map(str::trim).filter(|p| !p.is_empty()) else {
        return QueryKind::Unknown;
    };
    if is_identifier(p) {
        QueryKind::Symbol
    } else if p.split_whitespace().count() > 1 {
        QueryKind::Concept
    } else {
        QueryKind::Literal
    }
}

fn is_identifier(p: &str) -> bool {
    let mut chars = p.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Tier 1: assertive, generic. "prefer semctl" as the stated default. Names the
/// file-listing tools too, so a Glob nudge isn't steered purely at content search.
pub fn tier1(names: ToolNameStyle) -> String {
    let prefix = names.prefix();
    format!(
        "semctl — prefer the semctl tools for code in this indexed repo. For a known \
         symbol use `{prefix}find_definition` / `{prefix}find_references` / \
         `{prefix}who_calls`; for a concept use `{prefix}search_codebase`; to \
         list or explore files use `{prefix}list_files` / `{prefix}file_tree`; \
         for an exhaustive literal sweep use `{prefix}grep`. They return ranked, \
         focused results (cheaper on context) and answer graph questions raw search \
         can't. Raw grep/find stays fine for non-code or a one-off literal check. See \
         the codebase-retrieval skill."
    )
}

/// Tier 2: firm, and tailored to the kind of search and (for content) what the
/// query looks like.
pub fn tier2(
    names: ToolNameStyle,
    kind: SearchKind,
    eligible_count: u32,
    pattern: Option<&str>,
) -> String {
    let prefix = names.prefix();
    let lead = format!("semctl — {eligible_count} built-in searches this segment. ");
    let tail = match kind {
        SearchKind::Filename => format!(
            "You're enumerating files — `{prefix}list_files` / `{prefix}file_tree` \
             list and explore the indexed tree directly (filtered, paged). Reach for \
             semctl first; raw find/glob is the fallback, not the default."
        ),
        SearchKind::Content => match classify(pattern) {
            QueryKind::Symbol => {
                let sym = pattern.map(str::trim).unwrap_or_default();
                format!(
                    "`{sym}` is a symbol — `{prefix}find_definition` / \
                     `{prefix}find_references` / `{prefix}who_calls` resolve it \
                     precisely, with callers and impls, in one ranked call. Reach for semctl \
                     first; raw grep is the fallback, not the default."
                )
            }
            QueryKind::Concept => format!(
                "That reads like a concept — `{prefix}search_codebase` returns ranked, \
                 focused hits across the whole index instead of raw lines. Reach for \
                 semctl first; raw grep is the fallback, not the default."
            ),
            QueryKind::Literal | QueryKind::Unknown => format!(
                "Prefer semctl for code here — `{prefix}search_codebase` for concepts, \
                 the graph tools (`{prefix}find_definition` / `{prefix}find_references` / \
                 `{prefix}who_calls`) for a known symbol, or `{prefix}grep` for an indexed \
                 literal sweep. Raw grep is the fallback, not the default."
            ),
        },
    };
    format!("{lead}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_kinds() {
        assert_eq!(classify(Some("parse_config")), QueryKind::Symbol);
        assert_eq!(classify(Some("HttpClient")), QueryKind::Symbol);
        assert_eq!(classify(Some("where is auth handled")), QueryKind::Concept);
        assert_eq!(classify(Some(r"foo\d+\(")), QueryKind::Literal);
        assert_eq!(classify(Some("TODO|FIXME")), QueryKind::Literal);
        assert_eq!(classify(None), QueryKind::Unknown);
        assert_eq!(classify(Some("   ")), QueryKind::Unknown);
    }

    #[test]
    fn tier2_symbol_names_the_graph_tools_and_the_symbol() {
        let m = tier2(
            ToolNameStyle::ClaudeCompatible,
            SearchKind::Content,
            5,
            Some("parse_config"),
        );
        assert!(m.contains("parse_config"));
        assert!(m.contains("mcp__semctx__find_definition"));
        assert!(m.contains("5 built-in searches"));
    }

    #[test]
    fn tier2_concept_names_search_codebase() {
        let m = tier2(
            ToolNameStyle::ClaudeCompatible,
            SearchKind::Content,
            8,
            Some("how does retry work"),
        );
        assert!(m.contains("mcp__semctx__search_codebase"));
    }

    #[test]
    fn tier2_filename_routes_to_file_tools_not_grep() {
        let m = tier2(
            ToolNameStyle::ClaudeCompatible,
            SearchKind::Filename,
            5,
            None,
        );
        assert!(m.contains("mcp__semctx__list_files"));
        assert!(m.contains("mcp__semctx__file_tree"));
        // A filename nudge must never tell the agent to grep.
        assert!(!m.contains("mcp__semctx__grep"));
    }

    #[test]
    fn tier2_tails_read_as_clean_sentences() {
        // The lead ends "segment. " so each tail must start capitalized.
        for m in [
            tier2(
                ToolNameStyle::ClaudeCompatible,
                SearchKind::Content,
                5,
                Some("how does retry work"),
            ), // Concept tail
            tier2(
                ToolNameStyle::ClaudeCompatible,
                SearchKind::Content,
                5,
                Some(r"foo\("),
            ), // Literal tail
            tier2(
                ToolNameStyle::ClaudeCompatible,
                SearchKind::Filename,
                5,
                None,
            ),
        ] {
            let after = m.split("segment. ").nth(1).unwrap();
            let first = after.chars().next().unwrap();
            assert!(
                first.is_ascii_uppercase() || first == '`',
                "tail starts with '{first}': {m}"
            );
        }
    }

    #[test]
    fn messages_do_not_repeat_the_false_grep_claim() {
        let all = format!(
            "{} {} {}",
            tier1(ToolNameStyle::ClaudeCompatible),
            tier2(
                ToolNameStyle::ClaudeCompatible,
                SearchKind::Content,
                5,
                Some("x"),
            ),
            tier2(
                ToolNameStyle::ClaudeCompatible,
                SearchKind::Filename,
                5,
                None,
            )
        );
        let lc = all.to_lowercase();
        assert!(!lc.contains("unopened"));
        assert!(!lc.contains("already read"));
        assert!(!lc.contains("files you haven't"));
    }

    #[test]
    fn messages_name_only_real_tools() {
        for names in [
            ToolNameStyle::ClaudeCompatible,
            ToolNameStyle::OmpMarketplace,
        ] {
            let all = format!(
                "{} {} {}",
                tier1(names),
                tier2(names, SearchKind::Content, 5, Some("x")),
                tier2(names, SearchKind::Filename, 5, None)
            );
            for bare in [
                "find_definition",
                "find_references",
                "who_calls",
                "search_codebase",
                "grep",
                "list_files",
                "file_tree",
            ] {
                assert!(
                    all.contains(&format!("{}{bare}", names.prefix())),
                    "missing {bare} for {names:?}"
                );
            }
        }
    }

    #[test]
    fn omp_marketplace_uses_namespaced_server_prefix() {
        let message = tier1(ToolNameStyle::OmpMarketplace);
        assert!(message.contains("`mcp__semctx_semctx_search_codebase`"));
        assert!(!message.contains("mcp__semctx__"));
    }
}

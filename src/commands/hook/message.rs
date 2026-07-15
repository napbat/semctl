//! The nudge copy. Static and local — no server call. Tool names are
//! fully-qualified (`mcp__semctx__*`) so they are unambiguous against the
//! built-in `Grep` tool and shell `grep`; the `steering_tests` phantom guard is
//! extended to accept that prefix so this copy can't drift from the registry.
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
pub fn tier1() -> String {
    "semctl — prefer the semctl tools for code in this indexed repo. For a known \
     symbol use `mcp__semctx__find_definition` / `mcp__semctx__find_references` / \
     `mcp__semctx__who_calls`; for a concept use `mcp__semctx__search_codebase`; to \
     list or explore files use `mcp__semctx__list_files` / `mcp__semctx__file_tree`; \
     for an exhaustive literal sweep use `mcp__semctx__grep`. They return ranked, \
     focused results (cheaper on context) and answer graph questions raw search \
     can't. Raw grep/find stays fine for non-code or a one-off literal check. See \
     the codebase-retrieval skill."
        .to_string()
}

/// Tier 2: firm, and tailored to the kind of search and (for content) what the
/// query looks like.
pub fn tier2(kind: SearchKind, eligible_count: u32, pattern: Option<&str>) -> String {
    let lead = format!("semctl — {eligible_count} built-in searches this segment. ");
    let tail = match kind {
        SearchKind::Filename => "You're enumerating files — `mcp__semctx__list_files` \
             / `mcp__semctx__file_tree` list and explore the indexed tree directly \
             (filtered, paged). Reach for semctl first; raw find/glob is the \
             fallback, not the default."
            .to_string(),
        SearchKind::Content => match classify(pattern) {
            QueryKind::Symbol => {
                let sym = pattern.map(str::trim).unwrap_or_default();
                format!(
                    "`{sym}` is a symbol — `mcp__semctx__find_definition` / \
                     `mcp__semctx__find_references` / `mcp__semctx__who_calls` resolve it \
                     precisely, with callers and impls, in one ranked call. Reach for semctl \
                     first; raw grep is the fallback, not the default."
                )
            }
            QueryKind::Concept => "That reads like a concept — `mcp__semctx__search_codebase` \
                 returns ranked, focused hits across the whole index instead of raw lines. \
                 Reach for semctl first; raw grep is the fallback, not the default."
                .to_string(),
            QueryKind::Literal | QueryKind::Unknown => "Prefer semctl for code here — \
                 `mcp__semctx__search_codebase` for concepts, the graph tools \
                 (`mcp__semctx__find_definition` / `mcp__semctx__find_references` / \
                 `mcp__semctx__who_calls`) for a known symbol, or `mcp__semctx__grep` for an \
                 indexed literal sweep. Raw grep is the fallback, not the default."
                .to_string(),
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
        let m = tier2(SearchKind::Content, 5, Some("parse_config"));
        assert!(m.contains("parse_config"));
        assert!(m.contains("mcp__semctx__find_definition"));
        assert!(m.contains("5 built-in searches"));
    }

    #[test]
    fn tier2_concept_names_search_codebase() {
        let m = tier2(SearchKind::Content, 8, Some("how does retry work"));
        assert!(m.contains("mcp__semctx__search_codebase"));
    }

    #[test]
    fn tier2_filename_routes_to_the_file_tools_not_grep() {
        let m = tier2(SearchKind::Filename, 5, None);
        assert!(m.contains("mcp__semctx__list_files"));
        assert!(m.contains("mcp__semctx__file_tree"));
        // A filename nudge must never tell the agent to grep.
        assert!(!m.contains("mcp__semctx__grep"));
    }

    #[test]
    fn tier2_tails_read_as_clean_sentences() {
        // The lead ends "segment. " so each tail must start capitalized.
        for m in [
            tier2(SearchKind::Content, 5, Some("how does retry work")), // Concept tail
            tier2(SearchKind::Content, 5, Some(r"foo\(")),              // Literal tail
            tier2(SearchKind::Filename, 5, None),
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
            tier1(),
            tier2(SearchKind::Content, 5, Some("x")),
            tier2(SearchKind::Filename, 5, None)
        );
        let lc = all.to_lowercase();
        assert!(!lc.contains("unopened"));
        assert!(!lc.contains("already read"));
        assert!(!lc.contains("files you haven't"));
    }

    #[test]
    fn messages_name_only_real_tools() {
        let all = format!(
            "{} {} {}",
            tier1(),
            tier2(SearchKind::Content, 5, Some("x")),
            tier2(SearchKind::Filename, 5, None)
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
                all.contains(&format!("mcp__semctx__{bare}")),
                "missing {bare}"
            );
        }
    }
}

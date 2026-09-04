//! Typed MCP tool inputs and edit-action output rendering.

use rmcp::schemars;

use crate::client;

// Each tool's arg struct derives JsonSchema (the MCP host reads it to
// learn the parameters) and Deserialize (rmcp fills it from the call).
// Field-level `///` docs become per-parameter descriptions in the
// schema; struct-level `///` docs would leak into the tool's input
// schema `description`, so keep struct-level notes as plain `//`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Codebase to search: an id or an indexed local directory path. Omit for
    /// the launch/current codebase. A local path is watched while this MCP runs.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Natural-language query.
    pub query: String,
    /// Max hits to return. Defaults to 20; focused discovery usually needs 5–8.
    /// The server's total result-content budget still bounds the response.
    pub top_k: Option<u32>,
    /// Restrict to these registered domain ids. Empty / omitted = all.
    pub domains: Option<Vec<String>>,
    /// Ranking bias: `"code"` demotes documentation (markdown), `"docs"` demotes
    /// code. Omit for the unbiased hybrid ranking.
    pub prefer: Option<String>,
    /// Restrict to these chunk kinds — `function`, `container`, or `block`.
    /// Omit for every kind.
    pub kinds: Option<Vec<String>>,
    /// Return full enclosing-symbol bodies instead of 4-line snippets. Defaults
    /// false. Expand only a focused hit set and use those bodies without rereading
    /// the same files; the server result budget applies across expanded hits.
    pub expand: Option<bool>,
    /// Server scope lens: `local`, `personal`, `organization`, or `global`.
    /// Mutually exclusive with `codebase_ids`.
    pub scope: Option<String>,
    /// Explicit visible codebase ids to search. Mutually exclusive with `scope`.
    pub codebase_ids: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Exact symbol name to look up.
    pub symbol: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReferenceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name to look up.
    pub symbol: String,
    /// Optional grammar namespace: `Type`, `Value`, `Macro`, or `Module`.
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CallPathArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol name the call chain starts at.
    pub from: String,
    /// Exact symbol name the call chain should reach.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowFromArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The external boundary a value enters from — a library-call moniker
    /// (`env/var`, a crate path) or a substring of one.
    pub from: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowToArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The external boundary a value leaves to — a library-call moniker
    /// (`fs/write`, a crate path) or a substring of one.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlowBetweenArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// The source external boundary a value enters from.
    pub from: String,
    /// The destination external boundary a value leaves to.
    pub to: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TraceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Exact symbol name to centre the neighbourhood on.
    pub symbol: String,
    /// How many call-graph hops out to include. Defaults to 1 (direct callers
    /// and callees).
    pub depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// The pattern to search for — a **regular expression** by default (Rust
    /// `regex`-crate syntax, matched per line; no look-around or backreferences),
    /// so `fn \w+\(` finds function definitions. Set `literal: true` to match it
    /// as an exact substring instead, e.g. to search `.unwrap()` without escaping.
    pub pattern: String,
    /// Match `pattern` as a literal substring instead of a regex — every
    /// character, including `. * ( ) [ ] \`, is matched verbatim (no escaping).
    /// Defaults to false. Use it for exact code like `foo.bar()` or `Vec<T>`
    /// that would otherwise need escaping and risk accidental regex matches.
    pub literal: Option<bool>,
    /// Case-insensitive matching. Defaults to false.
    pub ignore_case: Option<bool>,
    /// Optional path substring to narrow which files are searched.
    pub path: Option<String>,
    /// Max matches to return, 1–1000. Defaults to 100.
    pub max: Option<u32>,
}

/// Whether a *regex-mode* grep pattern must run through the regex engine, or can
/// take the server's faster trigram-accelerated literal path. A pattern with no regex
/// metacharacter matches identically either way (regex `test` == literal
/// `test`), so it takes the fast path; anything using a metacharacter needs the
/// engine. Pure optimization: it only applies in the default regex mode — an
/// explicit `literal: true` always takes the literal path regardless.
pub(super) fn pattern_needs_regex(pattern: &str) -> bool {
    pattern.contains(|c: char| {
        matches!(
            c,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        )
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OutlineArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Codebase-relative path of the file to outline (e.g. `server/Startup.cs`).
    pub path: String,
    /// Maximum grammar nesting depth to return. Omit for every depth.
    pub max_depth: Option<u32>,
    /// Restrict entries to grammar symbol kinds. Omit for every kind.
    pub kinds: Option<Vec<String>>,
    /// Include each declaration's exact body. Defaults to false.
    pub include_body: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSourceArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Codebase-relative source path.
    pub path: String,
    /// Strong content hash to pin. A stale revision is rejected atomically.
    pub revision: Option<String>,
    /// 0-based byte-range start; requires `byte_end`.
    pub byte_start: Option<u64>,
    /// 0-based, end-exclusive byte-range end.
    pub byte_end: Option<u64>,
    /// 1-based line-range start; requires `line_end`.
    pub line_start: Option<u32>,
    /// 1-based inclusive line-range end.
    pub line_end: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolSearchArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Declaration name or qualified-name pattern.
    pub query: String,
    /// `Exact`, `Prefix`, `Substring`, `Glob`, or `Fuzzy`. Defaults to Substring.
    pub mode: Option<String>,
    /// Grammar symbol kinds to retain.
    pub kinds: Option<Vec<String>>,
    /// Optional codebase-relative path prefix.
    pub path_prefix: Option<String>,
    /// Optional detected project name.
    pub project: Option<String>,
    /// Optional language id.
    pub language: Option<String>,
    /// Maximum results, 1–500. Defaults to 50.
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TypeHierarchyArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact or qualified type identity.
    pub symbol: String,
    /// `Supertypes`, `Subtypes`, or `Both`. Defaults to Both.
    pub direction: Option<String>,
    /// Relation hops, 1–16. Defaults to 4.
    pub depth: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CallGraphArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact or qualified seed symbol.
    pub symbol: String,
    /// Call hops, 1–10. Defaults to 2.
    pub depth: Option<u32>,
    /// `Callers`, `Callees`, or `Both`. Defaults to Both.
    pub direction: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalysisPageArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Zero-based page. Defaults to 0.
    pub page: Option<u32>,
    /// Rows per page, 1–500. Defaults to 100.
    pub page_size: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// New identifier spelling.
    pub new_name: String,
    /// Include grammar-classified comments containing the old spelling.
    pub include_comments: Option<bool>,
    /// Include grammar-classified string literals containing the old spelling.
    pub include_strings: Option<bool>,
    /// Include unresolved textual candidates. Unsafe unless reviewed.
    pub include_unresolved_text: Option<bool>,
    /// Permit uncertain candidates. Defaults to false.
    pub allow_uncertain: Option<bool>,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SafeDeleteSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// Permit uncertain dynamic sites. Defaults to false.
    pub allow_uncertain: Option<bool>,
    /// Permit a public declaration only when no durable consumers are known.
    pub allow_public_without_known_consumers: Option<bool>,
    /// Configured reflection/dynamic-use patterns to check conservatively.
    pub reflection_patterns: Option<Vec<String>>,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceBodyArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target.
    pub target: client::api::SymbolTargetRequest,
    /// Replacement body source, including the grammar-owned delimiters.
    pub replacement: String,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertSymbolArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Qualified identity or path/line/column target declaration.
    pub target: client::api::SymbolTargetRequest,
    /// Complete declaration source to insert.
    pub source: String,
    /// Explicitly approve the server plan's bounded formatter step, if any.
    pub run_formatter: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UndoEditArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Edit id returned by a completed symbolic edit action.
    pub edit_id: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditActionOutcome<'a> {
    edit_id: &'a str,
    operation: &'a str,
    changed_files: &'a [crate::editing::AppliedFile],
    already_applied: bool,
    already_undone: bool,
    watcher_active: bool,
    sync_state: &'a str,
}

pub(super) fn render_edit_action_outcome(
    outcome: &crate::editing::ApplyOutcome,
) -> std::result::Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&EditActionOutcome {
        edit_id: &outcome.plan_id,
        operation: &outcome.operation,
        changed_files: &outcome.changed_files,
        already_applied: outcome.already_applied,
        already_undone: outcome.already_undone,
        watcher_active: outcome.watcher_active,
        sync_state: &outcome.sync_state,
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExpandArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative path of the file (e.g. `server/Startup.cs`).
    pub path: String,
    /// First line of the range (1-based, inclusive).
    pub line_start: u32,
    /// Last line of the range (1-based, inclusive).
    pub line_end: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolAtPositionArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Codebase-relative path of the file (e.g. `server/Startup.cs`).
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// Optional 1-based column. With it, resolves the identifier under the cursor
    /// to its definition (go-to-definition); without it, the innermost enclosing
    /// definition (hover).
    pub column: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Exact symbol names to resolve in one call.
    pub symbols: Vec<String>,
    /// Return references instead of definitions for each symbol. Defaults to false.
    pub references: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListFilesArgs {
    /// Codebase id or indexed local directory path. Omit for the current codebase.
    pub codebase: Option<String>,
    /// Which copy to read: `"checkout"` (default) answers about the working
    /// tree this MCP is running in, including edits that are not committed or
    /// pushed; `"canonical"` answers about what the project publishes — the
    /// branch the server pulls. Use `canonical` to ask what is on the trunk
    /// rather than in front of you. Ignored outside a checkout, where the two
    /// are the same thing.
    pub copy: Option<String>,
    /// Optional case-insensitive substring; only files whose codebase-relative
    /// path contains it are listed (e.g. `src/auth` or `.rs`),
    /// searched across the whole catalog. Omit to list every indexed file.
    pub path: Option<String>,
    /// Zero-based page to return when not filtering. Defaults to 0; the result
    /// footer tells you when to request the next page. Ignored when `path` is set.
    pub page: Option<u32>,
    /// Rows per page, 1–1000. Defaults to 1000 — the whole list in one call for
    /// a normal repo. Lower it to scroll a very large catalog page by page.
    pub page_size: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexCodebaseArgs {
    /// Local directory to register and index. Omit for the launch/current
    /// directory. Calling this tool is the explicit indexing opt-in; agents must
    /// ask the user before calling it. A first-ever index waits for embedding to
    /// complete before retrieval tools are released.
    pub path: Option<String>,
}

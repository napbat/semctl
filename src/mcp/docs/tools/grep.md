Regex code search (with an exact-`literal` opt-in) over the codebase's indexed file content — the exact-match counterpart to `search_codebase`'s semantic retrieval. Scans whole files (not just chunked regions) and returns every matching line as `path:line: text`.

Use this for exact strings and patterns: an error message, a config key, a TODO, a specific identifier or call shape, a regex. Unlike `search_codebase` it's exhaustive (capped), not ranked by relevance.

## Parameters

- `pattern` (required) — the pattern to find. A **regular expression** by default (Rust `regex`-crate syntax, matched per line), so `fn \w+\(` finds function definitions and metacharacters are active. Set `literal: true` to match it as an exact substring instead.
- `literal` — match `pattern` as an exact substring: every character (including `. * ( ) [ ] \`) is taken verbatim, so nothing needs escaping and there are no accidental regex matches. Default false. Use it for exact code like `.unwrap()`, `foo(bar)`, or `Vec<T>`.
- `ignore_case` — case-insensitive matching. Default false.
- `path` — optional path substring to narrow which files are scanned (e.g. `server/` or `.rs`).
- `max` — max matches to return, 1–1000. Default 100.

## Regex syntax

Patterns use **Rust `regex`-crate syntax** (RE2-style, linear-time), matched **per line**:

- Supported: char classes `[a-z]`, quantifiers `* + ? {n,m}`, groups + alternation `(a|b)`, anchors `^ $`, word boundary `\b`, and escapes `\w \d \s`. Escape literals you mean literally: `\(`, `\.`.
- Line-oriented: `^` and `$` anchor the start/end of a **line**, and `.` never crosses a newline — a pattern cannot span multiple lines.
- **Not supported** (these error out, not "no match"): look-around `(?=...)` / `(?<=...)` and backreferences `\1`. Rewrite without them.

Example: find function definitions → `{ "pattern": "fn \\w+\\(" }`. To search for text containing metacharacters without escaping, pass `literal: true`: `{ "pattern": ".unwrap()", "literal": true }`.

## When to use this vs alternatives

- **grep** (this tool): exact string / regex, "find all occurrences of X".
- **search_codebase**: conceptual / natural-language queries when you don't know the exact text.
- **find_definition / find_references**: a known symbol — faster and precise.

## Notes

- Searches the indexed snapshot, so brand-new local edits aren't reflected until the codebase re-syncs (see `sync_status`).
- Results are capped by `max` — raise it or narrow with `path` when completeness matters on a large codebase.

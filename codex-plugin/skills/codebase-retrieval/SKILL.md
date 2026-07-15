---
name: codebase-retrieval
description: Use in any repo where the semctx MCP tools are attached, for any task that needs to locate, understand, trace, or verify code before answering or editing — "where is X", "how does X work", "find usages / references", "who calls X", "what implements Y", "trace a flow / call path", "search code or docs", "list files", or any "find code in this repo" moment. Route through semctx before file reads or shell search, and handle the degraded states (still indexing, stale hits, logged out, unsupported language).
---

# codebase-retrieval

When the semctx MCP tools are attached, use them FIRST for code discovery —
before direct file reads or shell search (`rg`, `grep`, `find`,
`Get-ChildItem -Recurse`). The index is server-side hybrid retrieval (dense +
lexical, reranked) plus a symbol graph with call and data-flow edges:
cross-file structure raw search can't see, at a fraction of the context cost of
raw reads. Fall back to a direct read only for a file's exact current bytes —
especially before an edit, or after a stale hit.

Names below are the semctx MCP tools — call whatever exact name your host lists
them under; a bare name here (e.g. `grep`) is always the semctx tool, never the
shell command. Use each tool's live schema for its arguments; don't invent
parameters.

## Route

| Need | semctx tool |
| --- | --- |
| Fuzzy / conceptual — "how does X work" | `search_codebase` — raise `top_k` for more hits; `expand: true` for full symbol bodies; `prefer: "docs"` or `"code"` |
| Definition / resolved usages of an exact name | `find_definition` / `find_references` |
| Callers / implementations | `who_calls` / `implementations_of` |
| How A reaches B | `call_path`; `trace` for a one-call neighbourhood (definition + callers + callees) |
| Value / data flow | `reaches` (forward), `flows_into` (backward), `flows_between` (witness path) |
| Every literal occurrence, incl. strings / comments / docs | `grep` over indexed content |
| A file's shape / more context around a hit | `file_outline` / `expand_chunk` |
| What's at `path:line` | `symbol_at_position` |
| Many exact symbols at once | `batch_lookup` (up to 256 per call) |
| Orientation | `list_files`, `file_tree`, `list_projects`, `list_domains` |
| File-to-file / import-to-definition structure | `imports`, then `symbol_edges` |
| A dependency defined outside this repo | `external_links` |
| Index / job state | `sync_status` |

## Priority rules

- Start with a semctx tool for any find-code step. File reads and shell search
  are the fallback (below), not the default.
- For an identifier-like exact name in a graph language (Rust, C#, Go,
  TypeScript / JS), try the graph tools first — `find_definition` /
  `find_references` / `who_calls` / `implementations_of` — not search.
- Resolving many exact symbols? Use one `batch_lookup`, not serial
  `find_definition` calls.
- Need EVERY occurrence? Use `grep`, not `find_references`: `find_references`
  returns resolved code references only — it skips strings, comments, markdown,
  repeat uses within a chunk, and any symbol defined outside this repo.
- Language outside the graph set (Python, Java, C++, ...)? The graph tools
  return nothing there — use `search_codebase` + `grep`.
- If a prompt hook already injected likely-relevant hits, pull detail on those
  (`expand_chunk`, `file_outline`) before issuing a fresh search.

## Degraded states — read the signal, don't guess

- Empty results right after attach -> call `sync_status`: `queued` / `running`
  means the index is still building; retry shortly instead of concluding the
  repo isn't indexed.
- A hit flagged stale (edited since indexed), or a freshness warning -> read
  that file for current bytes. The MCP server auto-syncs edits, so don't tell
  the user to run `semctx index` unless there's no MCP session or the codebase
  is pinned read-only.
- A tool answers "not logged in - run `semctx login`" -> answer the immediate
  question with the fallback tools, then end your reply with one line: semctx is
  logged out, and `semctx login` restores it (tools recover on retry, no
  reconnect needed).
- `find_definition` / `find_references` return "Did you mean" candidates ->
  retry one before falling back.
- `who_calls` / `implementations_of` empty in a graph language does NOT mean no
  callers / impls — cross-check with `grep` before you conclude that.

## Fall back to file reads / shell search only when

- No semctx tools are attached (plugin not installed or disabled).
- You need a file's exact current bytes to edit it — hits are indexed
  snapshots, and stale ones are flagged.
- The target is excluded from the index: gitignored, `.semctxignore` matches,
  lockfiles, build output, generated / minified files, test fixtures, files
  over 1 MB. Markdown docs ARE indexed — search them with `prefer: "docs"`.

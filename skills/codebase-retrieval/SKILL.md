---
name: codebase-retrieval
description: Use when answering codebase questions in a semctx-indexed repo — "how does X work", "where is X defined", "who calls X", "what implements Y", "find references to Z", "trace through", or any other "find code in this repo" task — and when semctx tools return stale, empty, or erroring results. Routes retrieval through the semctx MCP tools before host-side Read / Grep / Glob and covers the degraded states (still indexing, stale hits, not logged in, unsupported language).
---

# codebase-retrieval

When the semctx MCP server is attached, any "find code in this repo" question
goes through the semctx tools BEFORE host `Read` / `Grep` / `Glob`. The index
is server-side hybrid retrieval (dense + lexical, fused and reranked) plus a
symbol graph with call and data-flow edges — cross-file structure host tools
can't see, at a fraction of the context cost of raw reads.

## Route by question

| You want | Call |
| --- | --- |
| fuzzy / conceptual — "how does X work" | `search_codebase` — raise `top_k` (default 20) for more; `expand: true` returns full symbol bodies (skips the follow-up read); `prefer: "docs"` for markdown-first, `"code"` for implementation-first |
| where is X defined / where is it used | `find_definition` / `find_references` (exact, case-sensitive names) |
| who calls X / what implements Y | `who_calls` / `implementations_of` |
| how does A reach B | `call_path`; one-call neighbourhood (definition + callers + callees) → `trace` |
| where does a value flow | `reaches` (forward), `flows_into` (backward), `flows_between` (witness path) |
| every literal occurrence, incl. strings/comments | `grep` — semctx's own, over indexed content (regex, ignore_case, path filter) |
| a file's shape / more context around a hit | `file_outline` / `expand_chunk` |
| what is at path:line | `symbol_at_position` |
| many symbols at once | `batch_lookup` (≤256 per call) — one round-trip, not N `find_definition` calls |
| orientation | `list_files`, `file_tree`, `list_projects`, `list_domains` |
| file→file / import→definition structure | `imports`, then `symbol_edges` |
| a dependency defined outside this repo | `external_links` |
| index job state | `sync_status` |

Parameters and details live in each tool's own description — read them, don't guess.

## Rules

- Start with a semctx tool for any find-code question; host tools are the
  fallback (below), not the default. Batch independent lookups in one message.
- Exact symbol, defined in this repo, in a graph language (Rust, C#, Go,
  TypeScript/JS) → symbol-graph tools (`find_definition` / `find_references` /
  `who_calls`), not search.
- Use `grep` instead of `find_references` when you need EVERY occurrence:
  `find_references` returns resolved code references only — it skips strings,
  comments, markdown, repeat uses within a chunk, and any symbol whose
  definition lives outside this repo (std / dependencies return nothing).
- Repo language outside the graph set (Python, Java, C++, …)? The symbol-graph
  tools return nothing there — use `search_codebase` + `grep`.
- A prompt hook may already have injected likely-relevant hits — pull detail
  on those (`expand_chunk`, `file_outline`) before issuing a fresh search.

## Degraded states — read the signals, don't guess

- Empty results right after attach → `sync_status`: `queued` / `running`
  means the index is still building; retry shortly instead of concluding the
  repo isn't indexed.
- A hit flagged `⚠ stale (edited since indexed)`, or a freshness footer →
  `Read` that file for current bytes. The MCP server auto-syncs edits
  (filesystem watcher + periodic re-sync); do not tell the user to run
  `semctx index` unless there is no MCP session, or the codebase is pinned
  read-only.
- A tool answers "not logged in — run `semctx login`" → answer the immediate
  question with host tools, and end your reply with one line telling the user
  semctx is logged out and `semctx login` restores it (tools recover on
  retry, no reconnect needed).
- `find_definition` / `find_references` misses return "Did you mean"
  candidates — retry with one before falling back.
- `who_calls` / `implementations_of` empty on a graph language ≠ no callers —
  cross-check with `grep`.

## Fall back to host Read / Grep / Glob only when

- No semctx tools are attached at all (plugin not installed or disabled).
- You need a file's exact current bytes to edit it — hits are indexed
  snapshots, and stale ones are flagged.
- The target is excluded from the index: gitignored, `.semctxignore` matches,
  lockfiles, build output, generated/minified files, test fixtures, files
  over 1 MB. Markdown docs ARE indexed — search them with `prefer: "docs"`.

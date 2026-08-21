---
name: codebase-retrieval
description: Use when answering codebase questions in a semctx-indexed repo — "how does X work", "where is X defined", "who calls X", "what implements Y", "find references to Z", "trace through", or any other "find code in this repo" task — and when semctx tools return stale, empty, or erroring results. Routes missing repository evidence to semantic and graph tools while preserving fresh conversation context, current known-file reads, and narrow local checks.
---

# codebase-retrieval

When the semctx MCP server is attached, begin with the freshest relevant
evidence already available. Use semctx for repository discovery, unknown
locations, cross-file relationships, symbol graphs, and broad indexed searches.
Existing context can support an answer directly; host `Read` / `Grep` / `Glob`
fit current bytes at a known path or a narrow file-scoped check.

## Route by question

| You want | Call |
| --- | --- |
| fuzzy / conceptual — "how does X work" | `search_codebase` — start with snippets and a focused `top_k` (usually 5–8); the server bounds total result content; use `expand: true` only for the most relevant hit or small set, then reason from those bodies; `prefer: "docs"` or `"code"`; use `scope` or `codebase_ids` for an authorized multi-codebase lens |
| declaration-name discovery | `search_symbols` — exact/prefix/substring/glob/fuzzy qualified-name search with kind/path/project/language filters |
| where is X defined / where is every resolved use | `find_definition` / `find_references` (exact, case-sensitive names; references include exact occurrences, read/write, namespace, kind, and resolved identity) |
| who calls X / what implements Y | `who_calls` / `implementations_of` |
| how does A reach B | `call_path`; compact neighbourhood → `trace`; complete bounded caller/callee graph → `call_graph` |
| super/subtype relations | `type_hierarchy` — declared/structural origin, external targets, bounded direction/depth |
| call cycles / unused definitions / exact duplicates | `cycles` / `unused` / `duplicates` (unused results include reason + completeness caveat; duplicates include hashes) |
| where does a value flow | `reaches` (forward), `flows_into` (backward), `flows_between` (witness path) |
| every literal occurrence, incl. strings/comments | `grep` — semctx's own, over indexed content (regex, ignore_case, path filter) |
| a file's nested shape / more context around a hit | `file_outline` (depth/kind/body controls) / `expand_chunk` |
| exact source bytes from a remote-only or revision-pinned codebase | `read_source` — request a focused line/byte range; use host `Read` for current bytes at a known local path |
| what is at path:line | `symbol_at_position` |
| many symbols at once | `batch_lookup` (≤256 per call) — one round-trip, not N `find_definition` calls |
| orientation / effective binding | `list_codebases`, `current_context`, `list_files`, `file_tree`, `list_projects`, `list_domains` |
| file→file / import→definition structure | `imports`, then `symbol_edges` |
| a dependency defined outside this repo | `external_links` |
| index job state | `sync_status` |
| explicitly index an unindexed repo after user opt-in | `index_codebase` |
| perform a symbolic edit | `rename_symbol`, `safe_delete_symbol`, `replace_symbol_body`, `insert_before_symbol`, or `insert_after_symbol` — immediate local actions that internally consume the server's grammar-validated plan and use the normal approval path |
| reverse a symbolic edit | `undo_edit` — restore hash-guarded private preimages using the returned edit id |

Parameters and details live in each tool's own description — read them, don't guess.

Every codebase-scoped tool accepts an optional codebase selector. Omit it for
the launch/current repo; pass either an immutable codebase id or an indexed local
directory path for cross-repo work. A path-based call keeps that checkout watched
for the rest of the MCP session. A previously indexed repo already has consent
for retrieval and watcher activation, so use it without asking again; that consent
does not authorize edits outside the user's requested repository set. Only when a
repo has never been indexed should you tell the user and call `index_codebase`
after explicit opt-in. That first-ever index is gated:
retrieval/catalog/graph tools wait until embedding finishes successfully, while
`sync_status` remains available for progress. Later re-syncs do not block use of
the last complete index.

## Rules

- Use relevant context directly when it is sufficient and fresh; retrieve the
  missing evidence rather than reacquiring material already available.
- Choose semctx for repository discovery, graph relationships, and broad indexed
  coverage. Batch independent exact lookups; avoid overlapping semantic queries.
- Exact symbol, defined in this repo, in a graph language (Rust, C#, Go,
  TypeScript/JS) → symbol-graph tools (`find_definition` / `find_references` /
  `who_calls`), not search.
- Semantic search responses share a server-enforced result-content budget.
  Start unexpanded, refine from ranked snippets, and expand only the relevant hit
  or small set. An expanded body normally replaces a follow-up file read.
- Use `grep` instead of `find_references` when you need EVERY occurrence:
  `find_references` returns every resolved code occurrence (including repeated
  same-line uses) but still skips strings, comments, markdown, and any symbol whose
  definition lives outside this repo (std / dependencies return nothing).
- Symbolic edit tools are immediate local mutations. Call one only when the user
  requested that edit and let the host enforce its normal write/destructive
  approval. The server plan stays internal to the action; report the returned
  changed files and edit id. Use `undo_edit` only while its recorded postimage
  hashes still match.
- Cross-repository retrieval supplies evidence only. Modify another checkout or
  open a PR there only when it belongs to the user-requested edit scope.
- Repo language outside the graph set (Python, Java, C++, …)? The symbol-graph
  tools return nothing there — use `search_codebase` + `grep`.
- A prompt hook may already have injected likely-relevant hits. Treat them as
  leads when additional repository evidence is needed, and pull focused detail
  with `expand_chunk` / `file_outline` rather than issuing a fresh broad search.

## Degraded states — read the signals, don't guess

- Empty results right after attach → `sync_status`: `queued` / `running`
  means the index is still building; retry shortly instead of concluding the
  repo isn't indexed.
- A hit flagged `⚠ stale (edited since indexed)`, or a freshness footer →
  `Read` that file for current bytes. The MCP server auto-syncs edits
  (filesystem watcher + periodic re-sync); do not tell the user to run
  `semctx index` while a known local checkout is active. If only a codebase id
  is known, pass its indexed local path as the codebase selector to activate
  watching.
- A tool answers "not logged in — run `semctx login`" → answer the immediate
  question with host tools, and end your reply with one line telling the user
  semctx is logged out and `semctx login` restores it (tools recover on
  retry, no reconnect needed).
- `find_definition` / `find_references` misses return "Did you mean"
  candidates — retry with one before falling back.
- `who_calls` / `implementations_of` empty on a graph language ≠ no callers —
  cross-check with `grep`.

## Use host Read / Grep / Glob when

- The relevant code is already present and fresh enough for the task; continue
  from that evidence without another retrieval call.
- A known local file or range needs current working-tree bytes.
- A narrow literal or filename check targets one known file.
- Semctx tools are unavailable or not attached at all (plugin not installed,
  disabled, or degraded).
- You need a file's exact current bytes to edit it — hits are indexed
  snapshots, and stale ones are flagged.
- The target is excluded from the index: gitignored, `.semctxignore` matches,
  lockfiles, build output, generated/minified files, test fixtures, or files over
  1 MB. Markdown docs ARE indexed — search them with `prefer: "docs"`.

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
| fuzzy / conceptual — "how does X work" | `search_codebase` — start with snippets; `expand: true` only for the most relevant hits, then reason from those bodies; `prefer: "docs"` or `"code"`; `scope` / `codebase_ids` for a multi-codebase lens |
| declaration-name discovery | `search_symbols` — exact/prefix/substring/glob/fuzzy qualified-name search with kind/path/project/language filters |
| where is X defined / where is every resolved use | `find_definition` / `find_references` (exact, case-sensitive names; references include exact occurrences, read/write, namespace, kind, and resolved identity) |
| who calls X / what implements Y | `who_calls` / `implementations_of` |
| how does A reach B | `call_path`; compact neighbourhood → `trace`; complete bounded caller/callee graph → `call_graph` |
| super/subtype relations | `type_hierarchy` — declared/structural origin, external targets, bounded direction/depth |
| call cycles / unused definitions / exact duplicates | `cycles` / `unused` / `duplicates` (unused results include reason + completeness caveat; duplicates include hashes) |
| where does a value flow | `reaches` (forward), `flows_into` (backward), `flows_between` (witness path) |
| every literal occurrence, incl. strings/comments | `grep` — repo-wide, over indexed content (regex, ignore_case, path filter) |
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
after explicit opt-in. That first-ever index runs in the background:
`index_codebase` returns at once and never waits for embedding. Retrieval,
catalog, and graph tools never serve a partial index. They wait at most 5
seconds, then fail with a "still running" error. Call `sync_status` every 60
seconds to follow the first-index phase (registering, syncing, embedding,
ready, or failed), and use local Read/Grep meanwhile. Status reports the latest
local scan/upload step when available, without a fixed line layout. Missing
server counters are unknown; named reported counts are not a processed/total
ratio. If the phase is failed, inspect and correct the reported cause, then call
`index_codebase` for the same path to retry. Later re-syncs do not block use of
the last complete index.

## Rules

- Use relevant context directly when it is sufficient and fresh; retrieve the
  missing evidence rather than reacquiring material already available.
- Choose semctx for repository discovery, graph relationships, and broad indexed
  coverage. Batch independent exact lookups; avoid overlapping semantic queries.
- A symbol defined in this repo, its callers, a call path, implementations, or
  dependencies, in a graph language (Rust, C#, Go, TypeScript/JS, C++) → the
  graph tool from the table above, not search or `grep`.
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
- C++ needs build context (a CMakeLists.txt or compile database) and can miss
  member declarations and calls — confirm an empty C++ result with `grep`.
  C++ inheritance is in `type_hierarchy`, not `implementations_of`.
- Language without a pack (Java, for example)? The symbol-graph tools return
  nothing — use `search_codebase` + `grep`.
- A prompt hook may already have injected likely-relevant hits. Treat them as
  leads when additional repository evidence is needed, and pull focused detail
  with `expand_chunk` / `file_outline` rather than issuing a fresh broad search.

## Use it in proportion to the task

- Decide what is still unknown before calling anything. Map only what you will
  change or rely on: `trace` or `call_graph` for entry points whose behavior
  changes, `who_calls` for each function whose contract changes, and
  `implementations_of` / `type_hierarchy` for interfaces you touch. Skip mapping
  that your task brief or earlier results already supply.
- Moves, renames and deletes need `find_references` on every affected symbol;
  cleanup of a whole area adds `unused` and `duplicates`. Ordinary feature work
  does not need area-wide scans.
- Batch: one `batch_lookup` instead of many `find_definition` calls, one
  `call_graph` instead of chained `who_calls`. When docs or specifications would
  crowd out code in `search_codebase`, pass `kinds` (for example function) or
  `prefer` code.
- Run `sync_status` once at the start of a task that relies on graph results,
  and note the snapshot it reports.

## Which source wins

- Current bytes, compiler or language-server diagnostics and tests outrank the
  index. When they disagree with a semctx answer, trust them and say so.
- C++: the graph can miss members, calls through macros and code behind
  preprocessor conditions the index was not configured with. For an exhaustive
  reference list, a rename or a delete, use `grep` or a language server on the
  matching compile database as the authority; use semctx for reach, concept
  search and links across languages, build files and docs.
- Rust, TypeScript/JS, Go and C#: graph answers for in-repo symbols are reliable;
  macros, string literals and docs still need `grep`.
- Build membership (which target compiles a file, which test registers it) and
  build-configuration switches are not graph edges: search the build files.

## Stale views while you edit

- Answers describe the last synced snapshot of an indexed checkout. A scratch or
  isolated checkout that was never indexed has no index of its own: query the
  indexed source checkout read-only and treat the answers as pre-edit state.
  After you move or rename a symbol, re-check with local Grep in your checkout.
- After edits in an indexed checkout, the watcher catches up; check
  `sync_status` before trusting graph results about code you just changed.

## Report on the tool

semctl is experimental, and your notes are its test data. When a task used
semctx, end the final report with a short "semctl notes" block, or append to the
shared notes file the task names. Give one line per notable query: tool,
query or symbol, result, what Grep, Read or the compiler showed, and a verdict
(saved work, stale, wrong, empty, slow). Name any capability you reached for
that did not exist. Leave the block out if you did not use semctx; do not pad it.

## Degraded states — read the signals, don't guess

- Empty results right after attach → `sync_status`: a first-index phase of
  registering, syncing, or embedding, or a job that is `queued` / `running`,
  means the index is still building; check again in 60 seconds instead of
  concluding the repo isn't indexed.
- A tool fails → the error ends with a `next:` line. Follow it. Retry once only
  when it says the call is retryable. Otherwise use local Read/Grep for that
  request. Do not repeat the same failing call in a loop.
- Source binding checks a local byte snapshot against the ready content-plane
  full-file hash, then checks exact snippet bytes and supplied byte/line
  coordinates in that snapshot. It does not verify semantic symbol/focus
  identity or guarantee that the file stays unchanged. Changed local bytes,
  mismatched ranges, and unavailable verification remain distinct. Native
  diagnostics stay visible. Use host Read/Grep for current bytes and locations
  when binding is not established; check `sync_status` for background work or
  failures. Verification does not request a reindex. The MCP server auto-syncs
  active checkouts through the filesystem watcher and periodic reconciliation;
  do not tell the user to run `semctx index` while a known local checkout is
  active. If only a codebase id is known, pass its indexed local path as the
  codebase selector to activate watching. Inspect reported watcher and retry
  state rather than assuming that a watcher is active or a failure is retryable.
- An expanded search hit marked `[not expanded: <reason> — …]` is a chunk, not
  the complete symbol. Follow the reported reason. For `GraphLoading`, search
  again later. For other reasons, use `expand_chunk` or `file_outline`.
- A `search_codebase` or `grep` result that reports a content budget cut is
  incomplete. Follow its `next:` line before you conclude a match is absent.
- A tool reports that semctl is not logged in → answer the immediate
  question with host tools, and end your reply with one line telling the user
  semctl is logged out and `semctl auth login` restores it (tools recover on
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
- Semctx tools are unavailable (plugin not installed, disabled, or degraded).
- The target is excluded from the index: gitignored, `.semctlignore` or legacy
  `.semctxignore` matches, lockfiles, build output, generated/minified files,
  test fixtures, or files over 1 MB. Markdown docs ARE indexed — search them with
  `prefer: "docs"`.

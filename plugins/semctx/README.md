# semctx coding-agent plugin

This directory is the shared plugin package. Agent-neutral components live once
at their conventional root locations:

- `skills/` contains reusable agent skills.
- `.mcp.json` declares the shared `semctl mcp` server. Claude/Codex manifests
  reference it; OMP discovers it for linked and npm-installed packages.
- `hooks/hooks.json` contains the lifecycle-hook subset supported by every
  current host.

Each coding agent keeps only the adapter material that cannot be shared:

- its required discovery manifest (`.codex-plugin/plugin.json`,
  `.claude-plugin/plugin.json`, or OMP's `package.json`);
- its marketplace metadata; and
- its host-specific lifecycle adapter, install/update commands, and configuration
  syntax.

To add another coding agent, point its marketplace at this plugin root and its
adapter at `skills/` and the shared hooks wherever its plugin format allows. OMP
instead loads `adapters/omp/index.ts`: its native events invoke `semctl hook` and
inject the returned context without duplicating retrieval policy in TypeScript.
The OMP `tool_call` event returns `PreToolUse` guidance as passive
`additionalContext` for that tool call. The adapter maps OMP tool inputs to the
Claude-shaped wire contract: the `glob` pattern moves from `path` to `pattern`,
a single-file `grep` line selector (`src/lib.rs:10-20`) is removed, and a `bash`
`cwd` becomes the hook `cwd`. `ast_grep` (`pat`) and `find` (`query`) are
repository content searches, so they map to `Grep` like `grep` does. A search
of OMP internal URLs (`omp://`,
`local://`, `skill://`) or web URLs is not repository discovery, so the adapter
does not send it to `semctl hook`.

The OMP adapter reads the live tool registry to distinguish a package server
(`mcp__semctx_*`) from a marketplace server (`mcp__semctx_semctx_*`). Rust owns
the corresponding guidance. When neither is mounted, hooks suppress MCP advice
but retain clear/compact resets, so guidance can recover after MCP reconnects.

Tool presentation comes from the shared MCP server. Search results lead with a
match count, numbered ranks and raw hybrid scores. Scores are query-relative,
not confidence; weaker matches retain their separate heading. `batch_lookup`
uses one table in caller order, keeping unmatched symbols and every location.
Table cells escape untrusted delimiters.

Source and outline bodies use language-tagged fences. Filename suffixes are
display hints, not claims about semantic indexing support; unknown grammars
remain readable as plain text. Mermaid and SVG source stays literal code rather
than becoming a diagram or image. `read_source` keeps the selected range and
source origin before the body, with its full revision and encoding afterward.
Source text retains its original bytes, including CRLF and embedded fences.

`sync_status` leads with the current failure or phase. It separates persisted
catalog state, current local work, and the last recorded server job. Named
embedded/deleted/failed and planned counts are not a processed/total ratio.
Missing counters remain unknown. A completed or zero-file job does not prove
checkout freshness. Job labels use Done, Running, Pending, and Failed. Complete
native diagnostics remain in the original result. Status is a snapshot, not live
push progress or an inferred server percentage or elapsed-time measurement.
Correct the reported cause before retrying a failed first index. Local failures
retain the reported retry schedule when one exists.

Search source binding compares a bounded local byte snapshot with the ready
content-plane full-file hash. Bound hits also require exact snippet bytes and
agreement with supplied byte/line coordinates in that snapshot. Binding does not
verify semantic symbol/focus identity or guarantee that bytes stay unchanged.
Changed local bytes, mismatched ranges, and unavailable verification remain
distinct, with native diagnostics preserved. Use host Read/Grep for current
locations when binding is not established. Verification does not start a reindex;
active local checkouts use their watcher and periodic reconciliation.
`current_context` lists exact indexed-copy identities, canonical status, and the
matching checkout. Codebase graph state is not a claim about each copy's
freshness. Tool names and argument schemas stay stable. Failed operations set
`isError` and retain their explanation.

### OMP native inspector

The adapter renders `search_codebase`, `read_source`, `batch_lookup`, and
`sync_status` inside their original OMP tool cards. Calls and results stay in the
normal transcript, including calls routed through `xd://`. There is no
above-editor result widget, second history, or replacement MCP tool. Each card
owns its selection and expansion state; concurrent calls cannot replace one
another's results.

Inline presentation requires the rendering-only `registerToolRenderer` extension
API. The adapter feature-detects it; hosts without it retain their native MCP
cards rather than installing a floating-widget fallback. OMP owns execution,
schemas, approvals, cancellation, status glyphs, elapsed time, and full-result
expansion. The original MCP text and result details are unchanged.

Plain-text search results put the exact query above a compact ranked table,
without per-match Markdown headings or a separate weak-match section. Source
previews, raw scores, copy/domain identity, and verification diagnostics remain
below the table. Text score bars use the same nonnegative, query-relative scale
as the native renderer; an unavailable scale stays unknown.

Replacing the MCP executable does not reload an already-running OMP extension.
OMP 18.12.1's `/restart` relaunches the process and resumes the saved conversation,
which loads the updated adapter. Session-history reload, `/reload-plugins`, and
`/mcp reload` do not re-evaluate extension modules. Finish or preserve active work
before restarting, and verify the loaded backend and live surface, not just
configured paths. Stock hosts need the rendering API before inline presentation
can activate.

The renderer consumes versioned, typed `_meta.semctlDisplay` data, not parsed
Markdown or model-facing `structuredContent`. The payload is limited to 32 KiB
and at most 32 total hits per call. Overflow omits the payload rather than
silently showing a partial result. Search metadata includes the nullable
`indexWarning`; compact results retain a warning icon with the original
freshness diagnostic on hover. A missing or malformed warning field falls back
to the complete native result, so older producer metadata cannot hide a warning.
Missing/invalid metadata, wrong origins, errors, partial results, and
cancellation also retain native rendering.

The exact query appears in the native call header. Match rows align rank,
unlabeled relative-score bars, file/location, and symbol columns. The public
native table API accepts text cells, not embedded meters, per-cell actions, or
row selection; composed rows preserve those interactions without a dashboard
border or fill. Show-more controls reveal additional rows. Below 36 columns or
without the required native primitives, the original native result body is used.
ANSI and noninteractive output retain the original renderer.

Score bars are query-relative, not confidence; hover exposes exact values.
Verification icons distinguish bound, changed-local, range-mismatch, and
unverified/not-probed states. Their hover retains the reason and the
byte/coordinate-only scope; semantic symbol/focus identity is not verified.
A compact state label replaces the icon when icons are unavailable. Batch
results retain caller order and misses; batch source locations are not probed
for verification. Source opening requires a bound path inside the selected
checkout for the same codebase. Numbering uses supplied valid coordinates;
indexed focus marks require bound source and a focus inside the displayed window.
Each card previews at most 12 search-source lines or 16 `read_source` lines and
states when more exist. Native full-result expansion preserves all returned
bytes; returning to compact view retains selection. Presentation makes no
additional source requests, automatically combined calls, or token-saving claim.

Nested Eval calls currently retain OMP's native trace presentation, not these
custom result cards. Eval stores summaries rather than canonical child result
metadata. A separate follow-up needs a bounded, per-run presentation contract at
`eval/js/context-manager.ts` / `tool-bridge.ts`, with explicit persistence for
restored cards, errors and cancellation. Do not infer parentage from whichever
Eval is active, parse scripts, or restore an above-editor result widget. Direct
`batch_lookup` and parallel MCP calls already use the normal-feed renderers.

### OMP layout sync indicator

A separate `setStatus` indicator reports producer-backed coordinator snapshots
from `notifications/semctl/sync_status`. The version-1 notification carries the
existing coordinator status, an initial reset, or a detached-root reset. It does
not introduce another indexing state machine, polling timer, or progress owner.
The deepest observed checkout containing the session directory supplies the
indicator; unrelated checkouts do not replace it. Upload counts appear only when
the producer supplies valid completed/total counts. No percentage is invented.

Idle `watching` describes the observed watcher, not graph freshness or a completed
index. Search freshness warnings and `sync_status` remain separate evidence.
Session shutdown and connection resets clear observed status. An abrupt transport
loss cannot deliver a reset; its last snapshot may remain until reconnect or
session shutdown. The indicator does not establish transport liveness.

Native layout, hover details, expansion, narrow widths, and lifecycle behavior
need acceptance in the actual OMP/Tern host. Source support alone is not runtime
acceptance. MCP stdout remains JSON-RPC, never ANSI escapes or Tern
surface-protocol commands.

Do not manually copy shared skills; when a host requires a projected format,
generate it from the canonical skill and verify parity in CI.

Then add the host in four registries:

1. implement the Rust `Host` adapter and append it to `hosts()` in
   `src/commands/install/mod.rs`;
2. add its command builder and optional preflight to `HOST_ADAPTERS` in
   `scripts/run_skill_evals.py`;
3. add the host id to the skill's `evals.json`; and
4. add a host adapter to `HOST_ADAPTERS` in `tests/hosts/hosts.ts`. The adapter
   loads this plugin from the checkout, isolates the host configuration, and
   maps each search intent to a native tool call.

A host is supported only when install, status, update, and uninstall behavior
are implemented, its host integration tests pass, and its eval session passes.
MCP-only agents should launch
`semctl mcp`; agents that support the Agent Skills layout should consume the
canonical skill directly. Host-native rules or instruction files are generated
projections, not new sources of truth.

The session hook loads `src/mcp/docs/instructions/server.md` once per context
segment. Repeated startup, resume, prompt, and tool events do not reload it.
Clear and compaction events reset delivery. A prompt hook restores the manual
when startup was missed or a `PostCompact` event could not return context.
Delivery requires a session id and writable session state. The manual is local
and does not require a server connection.

The MCP server leaves `ServerInfo.instructions` unset because hosts can prepend
that field to every tool description. Each tool retains its own documentation,
schema, and annotations. MCP-only clients do not receive the session manual
automatically. Keep selection advice and required caveats in each tool's
documentation so those clients can still use the tools. The retrieval skill
remains available to hosts that support Agent Skills.

The MCP server exposes symbolic edits as immediate, approved checkout actions.
Each action consumes the server-generated plan internally at semctl's verified
local mutation boundary; raw plans are not MCP tools. Rename, delete, body
replacement, insertion, and `undo_edit` all carry write/destructive annotations.
Keep those annotations, tool documentation, the server manual, and the skill in sync.
The steering tests reject missing docs, phantom tools, and safety drift.

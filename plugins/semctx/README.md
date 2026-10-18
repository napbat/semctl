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

The semctl adapter adds one replace-in-place native inspector above the editor
through OMP's existing public extension APIs. This is semctl plugin code, not an
OMP modification or a replacement MCP tool. The original MCP result stays in the
transcript, including its text, errors, approvals, and cancellation behavior.
OMP still controls its built-in MCP cards and output-size limits.

Search results in that original transcript put the exact query above a compact
ranked table, without per-match Markdown headings or a separate weak-match
section. Source previews, raw scores, copy/domain identity and verification
diagnostics remain below the table. Text score bars use the same nonnegative,
query-relative scale as the native inspector; an unavailable scale stays unknown.

Replacing the MCP executable does not reload an already-running OMP extension.
OMP 18.12.1's `/restart` relaunches the process and resumes the saved conversation,
which loads the updated adapter. Session-history reload, `/reload-plugins` and
`/mcp reload` do not re-evaluate extension modules. Finish or preserve active work
before restarting, and verify the new live surface rather than only its install path.

The inspector consumes versioned, typed `_meta.semctlDisplay` data. It does not
parse Markdown or use `structuredContent` to add display data to model context.
The payload is limited to 32 KiB and at most 32 total hits per call. Overflow
omits the display payload instead of silently showing a partial result; the
original result remains available. The widget retains only the latest requested
call, without a numbered history or result tabs. Earlier results remain in the
original transcript. It supports display payloads from `search_codebase`,
`read_source`, `batch_lookup`, and `sync_status`; other semctl calls replace the
current view with an unavailable notice.
Display data reuses source evidence already fetched by the query. The inspector
does not fetch more source, combine calls automatically, or establish token
savings.

In-flight call authority is tracked separately, including across resets. The
observer closes after 64 unfinished calls rather than forget an old call's
identity or grow without bound; a fresh host restores it. This affects only the
inspector, not MCP execution or the original transcript.

The exact search query appears above the matches, with a two-line preview and
the complete query on hover. Match rows align rank, unlabeled relative-score
bars, file/location, and symbol columns. This is not a `TspTable` with embedded meters.
The public table API accepts text cells; it does not expose meter nodes,
per-cell titles/actions, or per-row selection. Composed rows retain native
meters, score titles, and source selection without a dashboard border or fill.
At widths below 36 columns or without native nodes, the inspector shows a compact
ASCII lifecycle/count summary; complete results remain in the MCP transcript.

Score bars are query-relative, not confidence; hover exposes their exact values.
Native verification icons
distinguish bound, changed-local, range-mismatch, and unverified/not-probed
states. Their title/hover retains the precise reason and the byte/coordinate-only
scope; semantic symbol/focus identity is not verified. A compact state label
replaces the icon when native icons are unavailable. Batch results retain
caller order and misses; batch source locations are not probed for verification.
Native source opening requires a bound path inside the selected checkout for the
same codebase. Source numbering uses supplied valid coordinates. Indexed focus
marks require bound source and a focus inside the displayed window.
The fixed inspector previews at most 12 search-source lines or 16 `read_source`
lines and states when more exist. The original MCP card owns full-source
expansion and preserves the complete returned bytes. There is no second source
expander in the composer widget.
Status uses the supplied snapshot and named counters, not a server-plan
percentage. Spinner and elapsed time follow actual execution events, not
background indexing estimates. Noninteractive contexts do not show the widget.
Missing or invalid metadata and errors replace the current view. Cancellation
finalizes the current call, and late results cannot replace a newer request.
Session resets remove the widget. A plain rendering fallback remains available.

Native layout, theme glyphs, hover details, expansion, narrow widths, and
cancellation/reset behavior require acceptance in the actual OMP/Tern host.
Source-level support alone does not establish that runtime acceptance.
MCP stdout remains JSON-RPC, never terminal escape sequences or Tern
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

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

The adapter renders search, source, symbol lookup, reference, graph-analysis,
and sync-status results inside their original OMP tool cards. Calls and results
stay in the normal transcript, including calls routed through `xd://`. There is
no above-editor result widget, second history, or replacement MCP tool. Each
card owns its expansion state; concurrent calls cannot replace its results.

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
Markdown or model-facing `structuredContent`. Each payload is limited to 32 KiB.
Match lists allow at most 32 total hits per call. Overflow omits their payload
rather than silently showing a partial result. Graph previews have separate,
explicit limits described below. Search metadata includes the nullable
`indexWarning`; compact results show the original freshness diagnostic as visible
warning text. A missing or malformed warning field falls back
to the complete native result, so older producer metadata cannot hide a warning.
Missing/invalid metadata, wrong origins, errors, partial results, and
cancellation also retain native rendering.

The exact query appears in the native `Code search` header. Each match uses a
native file chip with a file icon, filename, line range, symbol, and verification
state. Chips use the same native small gap as Read results. Directory names
stay quiet. Filenames use native link styling when a safe checkout link is
available. Narrow chips keep one path line; hover and `Details` retain the
symbol and reference context. The native tool-header disclosure opens the
entire result. No second result-list disclosure appears between chips.
Hosts without the required native primitives use the original result body.
ANSI and noninteractive output retain the original renderer.

All match chips start folded. Select a chip to open or close its source preview
in place. Multiple chips can stay open for comparison. Each open chip has a
`Details` disclosure for its full location, qualified symbol, reference
context, and codebase identity.

Every scored match has a small relevance bar. The adapter uses native meters.
Text bars appear only when meters are unavailable.
Match chips, graph controls, and selected sources use the same
40 by 4 pixel rail. Hover the bar to see rank and score. Expanded results do not repeat
scores. A bar shows strength relative to the highest score in its original
result set, not confidence. A neutral bar marks scores that do not support that
ratio. Matches without scores have no bar or reserved score space.

Verification icons distinguish bound, changed-local, range-mismatch, and
unverified/not-probed states. Hover and accessible labels explain the state and
the available diagnostic. The reason appears before the verification limits.
Open chips do not repeat verification prose. Binding checks source bytes and
coordinates; it does not verify semantic symbol or focus identity.
A compact glyph replaces the icon when renderer icons are unavailable.
Definition, reference, caller, implementation, symbol-position, source-expansion,
and batch lookups use the same compact chips. Reference context remains
available without invented relevance scores. Batch results retain caller order
and misses.
Lookup source locations are not probed.
Filename links and `Open file` navigate to the current checkout file.
They require matching, nonempty codebase identities and a relative path inside
the selected checkout. They do not use indexed line fragments. Non-bound links
explain that the indexed location remains unverified. Source numbering uses
supplied valid coordinates. Indexed focus marks still require bound source and
a focus inside the displayed window.

Embedded source previews use bounded line windows and state when more lines
exist. Full tool-header expansion shows the complete returned source for
`read_source` and opened match chips. Folding the tool header hides all matches.
Reopening it restores open chips.
Presentation makes no additional source requests, automatically combined calls,
or token-saving claim.

Nested Eval calls currently retain OMP's native trace presentation, not these
custom result cards. Eval stores summaries rather than canonical child result
metadata. A separate follow-up needs a bounded, per-run presentation contract at
`eval/js/context-manager.ts` / `tool-bridge.ts`, with explicit persistence for
restored cards, errors and cancellation. Do not infer parentage from whichever
Eval is active, parse scripts, or restore an above-editor result widget. Direct
`batch_lookup` and parallel MCP calls already use the normal-feed renderers.

### OMP graph overview

Use `/semctl graph`, or select `Project graph` in the status view. The overview,
analysis selector, relationship details, and source controls are native OMP/Tern
UI. There is no graph HTTP listener, browser launcher, or viewer asset route.

The default indexed overview includes catalog files, directories, detected
projects, reported workspace membership, resolved file imports, and resolved
symbol bindings. Isolated files remain visible. This is the indexed structure,
not a guarantee that local source bytes or every runtime dependency are current.

The graph uses native dots, labels, and link segments. Each dot has its own
hover tooltip and selection target. Select a group to inspect its members.
Select a node to focus its neighbors and supplied source preview. Crossing a
link updates the group context. All groups and nodes remain reachable when
the overview combines smaller groups. Relationship details retain cycles,
shared targets, parallel links, and self-links. Position is not an embedding
distance or similarity score.

Concept colors are inferred from resolved symbol bindings, with imports as
weaker support. Frequently shared identifiers receive less weight. These are
evidence-backed communities, not a semantic guarantee. Unsupported paths remain
neutral. Tern owns brief arrival and hover transitions. Observed activity fades
in and out without moving the graph. Idle graphs have no animation loop.
Reduced-motion mode disables these effects without hiding observations.

The analysis selector also provides projects, calls and paths, symbol
neighborhoods, value-flow boundaries and witnesses, type hierarchies, cycles,
duplicate groups, unused definitions, file imports, symbol bindings, external
links, and file outlines. Lookup views use compact, independently expandable
matches with the same score bars and diagnostic tooltips as the feed.

Graph-based feed results show a compact native dot graph directly.
Node and link counts remain in the tool header. Disconnected and single-node
results retain their native node controls. `Relationships` exposes the complete
supplied link list. Hover graph notes for explanatory limits. Partial graphs
remain visibly marked. Ordinary links are solid, imports are dashed, and
project associations are dotted. The layout uses the existing force engine.

Search and lookup results keep their compact match chips. A separate `Graph`
control appears when at least two distinct supplied locations can be shown.
This includes references, callers, definitions, implementations, declaration
search, batch lookups, and expanded source context. The graph groups returned
matches by file and codebase. These links show locations, not inferred calls,
dependencies, or embedding similarity. Graph selection and open match chips
survive collapse and reopen. Opening a source preview does not fetch more data.

Graphs require native `el` elements and scoped stylesheets. OMP transports
`NativeNode.styles`; Tern draws the elements and owns their pointer targets,
tooltips, and CSS transitions. The adapter checks both host and terminal
support. Older hosts keep native node navigation instead of unstyled elements.
There are no SVG graph images, extension-driven animation frames, or web
viewer assets. The current pointer protocol supports selection, not free
dragging of graph nodes.

Concept colors stay consistent across match chips, graph nodes, and the live
popup. Colors require a validated overview with the same codebase and exact
path. Unknown, foreign, and unassigned paths stay neutral. The adapter loads one
cached overview after the first observed semctx call. Manual refresh and a
producer completion can refresh that catalog. Requests share one in-flight
lookup. The adapter does not poll the catalog or infer embedding distances.

Ordinary MCP graph metadata retains at most 64 nodes, 128 edges, 32 source
previews, and 32 KiB. It never retains an edge without both endpoints. Large
previews can be omitted with a caveat. The explicit local indexed overview uses
the same graph shape with limits of 12,000 nodes, 48,000 edges, and 3 MiB.
Import and symbol-binding retrieval each have a 20,000-edge limit. Oversize or
incomplete responses and invalid structure fail explicitly instead of returning
a partial overview as complete. The native navigator pages large node lists.

Opening or refreshing an analysis executes `semctl graph --json <analysis>` in
the current directory. This read-only CLI mode returns `{text, display}`.
`graph overview` combines catalog, project, import, and resolved symbol-binding
queries. The other analyses use the existing dispatcher. The configured CLI
server or tenant can differ from a custom MCP configuration.

Requests cancel on close, replacement, or checkout change. Errors and original
CLI text remain accessible in the native surface. Opening a node does not fetch
source. The overview does not start a model turn or indexing job.

Observed semctx requests with a matching explicit codebase ID can highlight
exact node identifiers, symbols, or paths. Default and directory-scoped requests
can highlight targets after returned metadata establishes a matching codebase
for that MCP server and checkout. Other requests remain in the activity summary.
They cannot imply activity in a differently configured CLI.
The active highlight ends when the correlated request finishes. Embedding
activity is aggregate producer evidence, not a per-file animation: the
notification does not identify active files.
The native composer status opens the same status view when selected. The
`Project graph` action opens the overview without starting a model turn.

#### Live overview popup

Use `/semctl graph live` or select `Live popup` in the status view.
The command toggles a non-modal native popup. The composer stays usable.
Refresh and close preserve the current draft and keyboard focus.
The popup requires an OMP build with non-modal overlay support.
Older hosts retain `/semctl graph` when native custom UI is available.

The popup groups the current project map into at most 16 displayed groups.
Each supplied node belongs to one group. An explicit aggregate holds smaller
groups. The preview draws at most 48 representative cross-group link bundles.
It retains direction, relation kind, internal link counts, and omission counts.
Expand the group list for member paths. Select `Overview` for the full map.

The popup keeps one compact activity row for observed requests and
producer-backed progress. Hover over the row for session request counts and
producer details. A fixed-width slot holds the progress ring or status icon.
Exact, codebase-matched request targets highlight their groups. Validated
returned sources receive a brief success halo. A failed targeted request
receives an error halo. These effects expire without moving the graph or
changing its concept colors. Reduced-motion mode uses static temporary halos.
Status and counter updates do not rebuild the graph.
The popup retains producer history so an old completed embedding job cannot
fill the progress ring after a newer upload. It does not invent per-file
embedding activity or a reranking phase.
Refresh keeps the last map visible. Closing the popup does not start a model
turn. Checkout and session changes close it and discard the previous context.

### OMP layout sync indicator

A separate `setStatus` indicator reports producer-backed coordinator snapshots
from `notifications/semctl/sync_status`. The version-1 notification carries the
existing coordinator status, an initial reset, or a detached-root reset. It does
not introduce another indexing state machine, polling timer, or progress owner.
The deepest observed checkout containing the session directory supplies the
indicator; unrelated checkouts do not replace it.

The composer reserves one leading mark and the fixed label `semctl`. Hosts with
structured extension-status support draw a native progress circle at the reported
percentage. Text-only terminals use one Unicode progress mark. The circle and
static status icons share one keyed slot. Unknown totals do not spin.
Numeric progress fills over 120 ms and returns to a muted mark after 420 ms.
The transition stops. Reduced motion skips it. Errors cancel it immediately.
Hover and status details retain exact producer counts throughout the transition.

A full circle requires observed embedding completion and an active watcher.
Routine preparing, scanning, and planning retain the last completed-job ring,
with an explicit checking description. New uploads invalidate that completion
until a newer job is observed. Pending work, errors, and unavailable evidence
do not invent success. This is not a live source-freshness guarantee.

Select the native composer indicator, or use `/semctl` or `/semctl status`, to
inspect the latest received snapshot in a read-only fullscreen view.
The view shows the phase, checkout, watcher, available diagnostics, and next step.
`/semctl details` also includes the retained
coordinator JSON.
Scroll with the arrow or page keys. Close with Enter, Escape, or `Close`.
Select `Project graph` or press G to open the relationship overview.
Status inspection does not start a model turn, fetch source, request indexing,
or retry.
Hosts without the custom view use a notification. Hosts without command
registration keep the indicator alone.

Exact upload and embedding counts appear in the inspection view when the
producer supplies valid completed/total counts. No percentage is invented.
A producer retry delay appears as paused, not as a successful index.
The coordinator reuses its existing 500 ms job waiter for later meaningful sync
jobs. One replaceable later-job observer reports current embedding progress;
superseded jobs cannot publish completion for a newer job. No-op syncs add no
job polling. The adapter has no polling timer.

Idle is informational, not graph freshness or a completed index. Search
freshness warnings and `sync_status` remain separate evidence. Producer resets
clear completion and progress evidence. Previously visible checkouts retain a
static unknown mark instead of flashing out of the composer. Session shutdown
clears the slot. An abrupt transport loss cannot deliver a reset; its last
snapshot may remain until reconnect or shutdown. The indicator does not establish
transport liveness.

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

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

The extension uses OMP's exported MCP manager to adapt native presentation hooks
on the existing semctx tools. It leaves ANSI renderers and execution unchanged.
Exact raw server and operation identities select the renderer. Tool refreshes
receive the same presentation. Retained cards keep their state after reconnects.
OMP still owns schemas, approvals, cancellation, status glyphs, elapsed time,
and full-result expansion. The original MCP text and result details are unchanged.
This integration does not require an OMP source patch or a custom executable.

Plain-text search results put the exact query above a compact ranked table,
without per-match Markdown headings or a separate weak-match section. Source
previews, raw scores, copy/domain identity, and verification diagnostics remain
below the table. Text score bars use the same query-relative scale as native
relevance rails. A result set with only nonnegative scores uses the ratio to its
highest score. A result set with any negative score uses the range from its
lowest to highest score, with a 10% floor for the lowest score.
Equal scores in a signed result set, missing scores, and non-finite scores keep
an unknown scale. The MCP text table shows `—` when the scale is unavailable.

Replacing the MCP executable does not reload an already-running OMP extension.
Use `/restart` in an idle session to load the updated adapter. Session-history
reload, `/reload-plugins`, and `/mcp reload` do not re-evaluate extension modules.
Finish or preserve active work before restarting. Verify the loaded backend
and live surface, not just configured paths.

The renderer consumes versioned, typed `_meta.semctlDisplay` data, not parsed
Markdown or model-facing `structuredContent`. Each payload is limited to 32 KiB.
Match lists allow at most 32 total hits per call. Overflow omits their payload
rather than silently showing a partial result. Graph previews have separate,
explicit limits described below. Search metadata includes the nullable
`indexWarning`. A warning badge with the text `index warning` in the native tool
header shows it. The badge tooltip keeps the original freshness diagnostic.
A missing or malformed warning field falls back
to the complete native result, so older producer metadata cannot hide a warning.
Missing/invalid metadata, wrong origins, errors, partial results, and
cancellation also retain native rendering.
When the server returns a partial graph, the card shows the server warning
above the result. A search budget notice appears before the matches.
An open match distinguishes an expanded symbol from a chunk when the server
reports the expansion state.

The exact query appears in the native `Code search` header. Each match uses a
native file chip with a file icon, linked filename, line range, and verification
state. Rounded chips form one column in rank order. All chips take the width of
the widest chip, so relevance bars and verification marks align at the right
edge. Chip labels retain the complete returned path, including directories.
Only the available row width can truncate a path. Hover or open Details to read
the full location. Filenames use native link styling when a safe checkout link is available.
Hover and expanded previews retain the symbol and
reference context. An open preview occupies its own row.
The native tool-header disclosure opens the entire result.
A muted badge in the native tool header shows the codebase identity.
The badge text is the display name. Canonical copies add `· canonical`.
The badge tooltip shows the codebase ID, checkout path, and copy.
Complete results start with the match chips.
No second result-list disclosure appears between chips.
Hosts without the required native primitives use the original result body.
ANSI and noninteractive output retain the original renderer.

All match chips start folded. Select a chip to open or close its source preview
in place. Multiple chips can stay open for comparison. Each open chip has a
`Details` disclosure for its full location, qualified symbol, reference
context, and codebase identity.
Match chips fade and rise in with a short stagger on first appearance.
Reduced-motion mode disables this arrival effect.

Every scored match has a small relevance bar. The adapter uses native meters.
Text bars appear only when meters are unavailable.
Match chips, graph controls, and selected sources use the same
40 by 4 pixel rail. Relevance rails sit 6 pixels from the location text.
They use a rounded track and an accent fill.
The fill eases when its value changes.
Reduced-motion mode disables this transition.
Hover the bar to see rank and score. Expanded results do not repeat
scores. A result set with only nonnegative scores uses the ratio to its highest
score. A result set with any negative score, such as reranker logits, uses each
score's position between the lowest and highest result scores.
The lowest score receives a 10% floor.
Neither scale indicates confidence.
Range-bar tooltips say
“Bar: position between lowest and highest result scores · not confidence”.
When a result set has equal signed scores, a missing score, or a non-finite
score, its scored matches keep a solid neutral rail, not a dotted or animated
progress indicator. An unavailable ratio also keeps a neutral rail.
Matches without scores have no bar or reserved score space.
The tooltip retains the raw score when available and explains a missing scale.

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
Filename links navigate to the current checkout file. No duplicate file action
appears below the chip. Links require matching, nonempty codebase identities and
a relative path inside the selected checkout. They do not use indexed line
fragments. Non-bound links explain that the indexed location remains unverified.
Source numbering uses supplied valid coordinates. Native code blocks highlight
the reported focus line, or the supplied match range when no focus is reported.
Highlights stay inside the returned snippet and do not verify the current
checkout. Invalid or missing snippet coordinates produce no highlight.

Embedded source previews use bounded line windows and state when more lines
exist. Full tool-header expansion shows the complete returned source for
`read_source` and opened match chips. Compact results show the first six
matches. The native tool header reveals every returned match. Folding restores
the compact preview. Reopening restores each chip's source selection.
Result-card presentation makes no additional source requests.
It does not combine calls automatically or claim token savings.

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
shared targets, parallel links, and self-links.

The overview title has the checkout name below it. `Partial graph` and
`Relationship layers loading` are native badges. Their tooltips explain the
limit. The line under the map shows the group and route counts. A right-aligned
hint tells you to select a dot for details.

Concept map is the default layout for profiles without a saved choice.
It combines documentation concepts, shared terms, and recorded links.
The overview arranges its groups in regions. A region holds at least two drawn
groups, in this order of precedence:

1. A documentation family: the naming documents share a folder, such as
   `Architecture`.
2. A home folder: most member files of each group are under the same top
   folder, such as `hv/` or `tools/`. When one folder would hold more than 70%
   of the groups, the next folder level splits it.
3. Linked terms: the groups have recorded links and share at least one
   identifier term.

A group's home folder is the deepest folder that holds at least 60% of its
member folders. The overview draws at most 6 regions, largest first.
Each region is one island. The islands are placed by their similarity, and
the layout packs them so that they do not overlap. Inside an island, related
groups sit side by side. Dot labels reserve room when the canvas allows it.
Force also uses lexical similarity and family cohesion.
Terms come from paths, labels, resolved symbol names, concept headings, and the
headings of each catalog Markdown file. Hex content digests in file names are
not terms.
Term frequency and inverse document frequency weight the terms.
Classical multidimensional scaling supplies stable coordinates.
These vectors are not the server's semantic embeddings.
The client has no endpoint for retrieving stored embedding vectors.
Position does not prove a relationship or indicate confidence.
Input order does not change geometry.
Activity, selection, indexing state, scores, and theme do not move nodes.

Each group shows up to three key terms. A key term is frequent in the member
files of that group and rare in the other groups. Words of the group label are
not repeated. Key terms are identifier evidence, not a summary of behavior.
They appear in group rows, in the selected-group inspector, and in dot hovers.

Select a group to open it. The open group splits into subconcepts:

1. Child folders: members in several child folders split by folder. Folders
   beyond the detail budget fold into one last `N other folders` subconcept.
2. Content clusters: members of one folder split into clusters. A child folder
   that holds at least 70% of the members, and at least 12 members, also splits
   into clusters. A cluster joins files with related names, Markdown headings,
   recorded links, and semantic affinity. Words that every member shares, such
   as a common manual title, do not separate clusters. A cluster label starts
   with its child folder name, or with `files` for the direct files of the open
   folder, followed by its distinctive terms.

Calm shows at most 6 subconcepts, Balanced 8, and Detailed 12. A group with more
than 24 nodes draws one dot per subconcept. Select a subconcept dot or row to
open the next level. A group with 24 nodes or fewer draws its members inside
tinted subconcept territories. The `← Overview` control and the breadcrumb return
to each open level. A refresh keeps the open levels when they still exist.

On a wide window, the full graph page puts the group or subconcept list beside
the drawing. The list scrolls on its own and keeps the drawing in view. A
narrow window puts the list below the drawing. Each row shows the name and the
member count on its first line, and the key terms and naming document on a
quieter second line. The summary below the drawing counts the drawn groups and
relationships. While relationship layers load, one badge says so above the
drawing, and the page footer does not repeat it.

Semantic affinity comes from semctx search profiles. The server gives no
embedding vectors, so the adapter uses its hybrid search ranking. For an open
level with 2 to 64 files, the adapter runs
`semctl graph --json affinity -- <paths>` once. The command describes each file
with one search: its Markdown headings, its outline names, or its file name.
Two files are related when their ranked result files overlap, or when one file
appears in the results for the other. A search can return related files without
returning the file that supplied its query. This does not establish whether the
file is indexed. The adapter measures each pair against the median pair of the
level so that common results do not join unrelated groups.
The line below the breadcrumb distinguishes returned matches, empty searches,
and searches that could not run while the backend loaded. Partial results keep
their usable pairs. A later request for the same level can retry a loading
result after ten seconds. No timer starts another search. A failed request
shows a warning. The level can still use terms and recorded links.
A level with more than 64 files shows `Placed by shared terms and links`. Its
hover names the size bound and tells you to open a subconcept.

Concept map and Force can draw soft tinted territories behind their members.
A documentation family has a solid border. A home-folder region has a dashed
border and a monospace folder label. When no region exists, shared identifier
terms can name lexical families.
The compact live popup hides territory labels.
A territory is omitted if it would cover an unrelated node or most of the canvas.
Territory tints use the shared palette. They are not confidence indicators.
Radial, Layers, and Components do not draw territories.

Overview group dots scale with member count. The compact popup uses a smaller
size range. Aggregated group routes use four weight tiers.
Each higher tier makes a route thicker and more opaque.
Ungrouped result graphs keep their previous dot sizes.
The folded remainder (`N other groups`) is a hollow dashed dot in the lower
right corner. It joins unrelated small groups, so the map draws no routes to it
and gives it no label priority. Its links stay in the inspector counts.

Every graph drawing sits in a soft canvas well and is centered in it.
Uncolored dots use a soft neutral tone. Every dot has a thin ring in the canvas
color. The ring separates the dot from links. A hovered dot grows slightly.
Links are thin, low-contrast lines. In the compact live popup, arrowheads appear
only on links of the selected or hovered node.
Containment and project-association routes stay faint until selected or hovered.

Documentation concepts take precedence over reference communities.
The adapter reads catalog Markdown files from the local checkout.
Document titles name concepts. Explicit paths and distinctive identifiers connect
the documents to code files. Supported reference and folder evidence can extend
an assignment. Broad index documents and ambiguous mentions do not seed concepts.
At least three code files must support a concept.
Source-directory groups remain where concept evidence is insufficient.
A source-directory group shows the color of its home folder, also when its
members are in different subfolders.
Duplicate structure labels include their project or path context.
Group rows show the source document.
Concept cues distinguish direct mentions, reference support, and folder support.
Assignments are map aids, not dependency declarations or semantic guarantees.
Documentation concepts remain available when embedding states are pending or
failed. Reference-only communities still require a complete overview.
The reader uses at most 256 catalog Markdown files, 1 MiB per file, and 16 MiB
in total. It skips unreadable files and paths outside the checkout.
It does not read files through links that leave the checkout.
This step makes no embedding requests, network searches, or model calls.
Tern owns arrival, movement, and hover transitions.
Nodes keep their identity across layout changes and refreshes.
They glide to new positions when geometry changes.
Routes and territories fade in after nodes settle. A refresh that produces the
same geometry keeps the routes and territories in place and does not replay the
fade.
Dots arrive with a short staggered scale-in.
Labels fade in after their dot.
Observed activity fades in and out without moving the graph.
Idle graphs have no animation loop.
Reduced-motion mode and Motion Off disable these effects without hiding
observations.

The analysis selector also provides projects, calls and paths, symbol
neighborhoods, value-flow boundaries and witnesses, type hierarchies, cycles,
duplicate groups, unused definitions, file imports, symbol bindings, external
links, and file outlines. Lookup views use compact, independently expandable
matches with the same score bars and diagnostic tooltips as the feed.
On the standalone page, press E or use the `Show more` control to see rows
beyond the first six. Batch misses keep their place in request order. Use the
arrow keys to select a match and press Enter to open its full returned source.
Folding the list preserves open previews. In a plain terminal, use PgUp and
PgDn to scroll the original result and its returned source.

The compact toolbar opens analyses, appearance, refresh, diagnostics, and close.
Analyses use native tabs for Project, Symbols, Calls, Flow & types, Quality,
and Source. The chooser is titled `Choose an analysis`. Each analysis shows the
input that it needs, and the result status footer is hidden while the chooser
is open. The map, chooser, and diagnostics share a stable viewing area.
Switching views keeps the selected node and source preview.
A refresh that changes the map keeps the open group, its open subconcepts, and
the selected file when they still exist. Group rows show key terms below the
group name.
Use A for analyses, S for appearance, I for diagnostics, and R to refresh.
Escape returns from the chooser or diagnostics to the result. Escape on the
result closes the page. If a request fails before returning a result, the page
shows the CLI error in the result pane. Details retains the original output
and producer diagnostics.

The live popup and graph page show a native spinner and a shimmer label while
the first map loads. A refresh button shows a native spinner only during a
refresh that you start.
The graph page footer uses a native spinner while loading or working.
Reduced-motion mode and Motion Off use static icons and labels instead.

Graph-based feed results show a compact native dot graph directly.
Node and link counts remain in the tool header. A partial or loading graph with
no links says `relationships unavailable` instead of `0 links`. Disconnected and
single-node results retain their native node controls. `Browse N relationships`
exposes the complete supplied link list, and `Browse N nodes` lists the nodes of
a graph without links. Hover graph notes for explanatory limits. Partial graphs
remain visibly marked. Ordinary links are solid, imports are dashed, and
project associations are dotted. Curved links share one continuous pattern.
Import dashes stay at least 8 pixels long. Association marks stay at least
4 pixels long. Short links remain single connectors instead of dense fragments.
The saved layout choice also applies to these graphs.

Search and lookup results keep their compact match chips. A separate graph icon
appears when at least two distinct supplied locations can be shown.
This includes references, callers, definitions, implementations, declaration
search, batch lookups, and expanded source context.
The icon sits at the right of the first match row, below the match count. The graph groups returned
matches by file and codebase. These links show locations, not inferred calls,
dependencies, or embedding similarity. Graph selection and open match chips
survive collapse and reopen. Opening a source preview does not fetch more data.

Graphs use Tern's native `el` elements and surface-scoped stylesheets.
The extension checks the terminal's advertised capabilities and sends its styles
through OMP's public terminal writer. Tern draws the elements and owns pointer
targets, tooltips, and transitions. No patched node or status API is required.
Tooltips use short headings and compact facts.
Most hovers have at most three lines of 88 characters each.
Progress and diagnostic summaries can use four lines.
Dot hovers show identity and state instead of every neighboring link.
Route hovers show up to two evidence entries and report omitted entries.
Group hovers show counts, catalog state, and the source document when available.
The existing node and group controls retain the complete supplied details.
Terminals without these capabilities keep native node navigation.
There are no SVG graph images, extension-driven animation frames, or web
viewer assets. The current pointer protocol supports selection, not free
dragging of graph nodes.

Concept colors stay consistent across match chips, graph nodes, and the live
popup. Colors require a validated overview with the same codebase and exact
path. Source-directory colors fill unsupported assignments.
The adapter loads one cached overview after the first observed semctx call.
Manual refresh and changed producer job observations can refresh that catalog.
Requests share one in-flight lookup. Local document reads run after the first
map paint. Stale or cancelled reads do not replace the active concept snapshot.
Placement does not use the server's semantic embeddings.

Ordinary MCP graph metadata retains at most 64 nodes, 128 edges, 32 source
previews, and 32 KiB. It never retains an edge without both endpoints. Large
previews can be omitted with a caveat. The explicit local indexed overview uses
the same graph shape with limits of 12,000 nodes, 48,000 edges, and 3 MiB.
Import and symbol-binding retrieval each have a 20,000-edge limit. Invalid
structure and responses above the display limits fail explicitly. Available
layers and valid pages remain visible when another layer is loading or a page
is incomplete. The overview marks that snapshot as partial and lists the
unavailable layers. The native navigator pages large node lists.

Opening or refreshing an analysis executes `semctl graph --json <analysis>` in
the current directory. This read-only CLI mode returns `{text, display}`.
`graph overview` combines catalog, project, import, and resolved symbol-binding
queries. Pending or failed embeddings do not prevent it from showing available
structure. A typed `GraphLoading` or `FileLoading` response marks an unavailable
layer without waiting for that layer to finish. Authentication, transport, and
invalid-response errors still fail the request. The other analyses use the
existing dispatcher. The configured CLI server or tenant can differ from a
custom MCP configuration.

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
The catalog can separately report a file as pending, embedded, or failed.
A small status pip at the lower right of a dot shows that evidence without
replacing concept colors. A failed pip uses the error color. A processing pip
uses the accent color. A pending pip is hollow.
On an overview group dot, the failed pip gets stronger as the failed share of
members grows. The three steps are below 25%, from 25% to 60%, and above 60%.
The group dot tooltip states how many members failed.
Overview group rows use a compact native stacked meter for catalog indexing
states: ready, processing, pending, failed, and unknown.
Exact counts remain in the tooltip.
Hosts without meters retain the text summary.
Unknown values remain unknown;
missing legacy state remains unreported. Aggregate job progress never assigns
a state to individual files.
The native composer status opens the same status view when selected. The
`Project graph` action opens the overview without starting a model turn.
The composer shows static marks for idle, pending, warning, and failed states.
Reduced motion suppresses indeterminate animation. Hosts without meters keep
the status action and show progress with a fixed-width mark.

#### Live overview popup

Use `/semctl graph live` or select `Live popup` in the status view.
The command toggles a non-modal native popup. The composer stays usable.
Refresh and close preserve the current draft and keyboard focus.
Native icon buttons open the full overview, configure appearance, refresh the
map, and close the popup.
Each button has a hover tooltip and an accessible name.
The grip at the upper left changes the popup size.
Click to enlarge it. Shift+click to shrink it.
Both controls wrap through Small, Medium, and Large.
Double-click the title to switch between Small and Large.
The appearance card also provides a Popup size row.
Tern does not expose continuous drag-resize events.
The size persists in the OMP profile.

| Size | Width | Maximum height |
| --- | ---: | ---: |
| Small | 320 px | 420 px |
| Medium | 480 px | 560 px |
| Large | 680 px | 720 px |

The viewport limits both dimensions.
When a new Tern session centers the composer, the popup fits its drawing
above the draft without changing the shape of routes. A wide popup places the
selected group and the group directory beside the drawing. A narrow popup
stacks them below it. Labels without enough room remain available on hover.

The popup title is `Project map`. A subline shows `N files · M groups`.
Hover the subline for node, link, and drawn-route counts.
A warning icon after the subline marks a partial map. Hover it for the
partial-map limits.
A slim stacked indexing bar shares the header with the file and group counts. Its label shows `N% ready`.
This value is the ready share of the files that report a catalog state.
Files without a reported state do not lower it.
The bar tooltip shows the exact counts. The map sits in the canvas well below
the bar.
The refresh button shows a spinner and stays disabled only while a refresh that
you started from the popup is active. Background map reads do not change the button.
The toolbar and status footer stay visible while you scroll the graph and group list.
The extension mounts the popup through stock OMP's widget API. It does not take
keyboard focus or create a separate terminal page. Status, details, and the full
graph use bounded native OMP popup cards on the same surface.
After updating the extension, finish active work and use `/restart` to reload it.
An MCP reconnect does not reload extension code.

The popup groups the current project map into bounded displayed groups.
Each supplied node belongs to one group. An explicit aggregate holds smaller
groups. The detail setting controls the number of displayed groups and routes:

| Detail | Maximum groups | Maximum cross-group routes |
| --- | ---: | ---: |
| Calm | 12 | 24 |
| Balanced (default) | 16 | 48 |
| Detailed | 24 | 72 |

The preview retains direction, relation kind, internal link counts, and omission counts.
Select a dot or group row to inspect it. The inspector shows member counts
and incoming, outgoing, and internal link counts from the full supplied map, including parallel
links and routes omitted from the drawing. Partial or loading maps stay qualified.
With a selection, the second row also shows the key terms of the group.
With no selection, the inspector shows a one-line legend. It explains that each
dot is a group sized by its files (`group`) and line weight (`links`). A tint entry appears only when territories are
drawn. Its label names the drawn kind: `concepts` for documentation families,
`folders` for home-folder regions, and `shared terms` for lexical families.
Several kinds show `regions`. Hover a legend item for its meaning.
The row below the legend shows `Most links:` chips for individual groups.
The folded remainder is excluded from this ranking.
The small popup shows up to two chips. Medium and Large show up to three.
The chips are hidden while the group list is open, because the list names every group.
Select a chip to select its group.
With a selection, a `Linked:` label precedes chips for the neighboring groups with the most observed links.
Use the previous and next controls to see the other neighboring groups
without changing the selected group.
The neighbor count includes both directions. Hover shows each direction separately.
The group list follows the map: each region has a heading with its territory
tint and group count, and its groups follow by size. An `Outside regions`
heading precedes the other groups. The folded remainder stays last.
In Small and Medium popups, the open group list replaces the drawing.
Its heading changes to `Back to map · N groups`; select it to return to the drawing.
In the Large popup, the list scrolls beside the drawing below its heading.
The group list separates names, member counts, indexing states, and request activity.
Medium and Large popups also show the key terms of each group.
A group row shows its catalog indexing states as a small stacked bar while some
files are not ready. Hosts without meters show a badge with the count of the most urgent state.
Hover a group for counts, key terms, region, and its source document.
Use the expand icon for the full map. The popup hides while the full map is open.
Selection follows group identity across refreshes. A delayed action cannot select
a different group because the list order changed.
When a map request fails, the `Diagnostics` disclosure retains the full error.
The hover summary stays short.

The popup footer uses one row below the group list.
The row contains a fixed progress ring, a percentage, a separate warning badge,
and request counters. Percentage digits animate when motion is allowed.
When the map reports indexing states, the ring and the `N% ready` label show the
ready share. This is the same share as the header bar. Nodes without a
reported state do not count. The hover names this measure and also shows the
producer-reported job progress.
Without reported states, the ring shows the producer-reported progress.
A dash means that the producer has not reported a current percentage.
Producer phases appear in the tooltip. If relationship layers are loading, the
footer says `Loading graph relationships…` even when all reported files are ready. A failed refresh says
`Showing last map` while the retained map is visible. The warning badge marks
a current problem.
The badge sits beside the ring in its own space.
Unknown and pending progress stay static.
Small `↑` and `↓` counters show observed requests sent and returned in this
session. Hover the counters for request totals.
Hover the circle or warning badge for the current state and
diagnostic. Long producer errors use a bounded preview; the full error remains
in the status or overview diagnostics view.
Map refresh errors do not erase separately reported producer progress.
The full graph page shows its status footer only while a request runs, a
problem exists, or observed requests exist. An idle footer is omitted.
Current requests take precedence over older request receipts, including their
halos and hover descriptions. Exact, codebase-matched request targets use the
native positive green cue. Their halos use a slow, subtle opacity change after
a short delay. Validated returned sources receive a brief green halo.
A failed targeted request receives an error halo. Receipt halos fade without
growing. Status pips do not spin.
Activity does not change category colors, node positions, or the settled labels.
Only deliberate selection can change label priority.
Reduced-motion mode uses static temporary halos.
Status and counter updates do not rebuild the graph.
The popup retains producer history so an old completed embedding job cannot
fill the progress ring after a newer upload. It does not invent per-file
embedding activity or a reranking phase.
Refresh keeps the last map visible, including when the refresh fails.
A map read that returns the same output keeps the current map. The popup does not
rebuild it. Equal doc concepts do not rebuild the map either.
Reopening the popup retries an initial failed lookup. A new embedding job or a
changed job state refreshes the map at once while the popup is open.
Progress counts of the same job start one map read after 30 seconds.
More progress in that interval does not start more reads.
While the popup is open, a typed loading response schedules another map read
after 10 seconds. Each unchanged loading response doubles the interval, to a
maximum of 60 seconds. A changed response, a refresh that you start, or a
reopened popup sets the interval to 10 seconds again.
These reads stop when loading clears, a request fails, or the popup closes.
Pending or failed file states alone do not start a polling loop.
Closing the popup does not start a model turn. Checkout and session changes
close it and discard the previous context.

##### Graph appearance

Select the appearance icon in the live popup or project page. The compact native
settings card keeps the graph visible. Changes update the preview immediately
without another graph request or a model turn. Color applies to every graph view.
Layout, detail, labels, links, and motion apply to the live popup and project page.
Popup size applies only to the live popup.

- **Layout:** Concept map, Force, Radial, Layers, or Components.
  Concept map places documented concept families and related terms near each
  other. Force uses lexical identifier similarity and recorded links.
  Radial uses graph-distance rings. When the largest map has at least three
  nodes and every other component is smaller, isolated nodes sit on one outer
  ring around that map. That ring does not show a distance. An isolated node has
  no recorded relation. Layers uses directed ranks, keeps cycles
  adjacent, and reduces link crossings. Crowded ranks wrap to fit.
  Components separates weakly connected sets into blocks. Rows within each
  block do not imply direction or hierarchy. Layout does not add relationships.
  Nodes keep their identity and glide to new positions across layout changes
  and refreshes. Routes and territories fade in after nodes settle.
  Dots arrive with a short staggered scale-in.
  Labels fade in after their dot.
- **Color:** Shared uses one codebase palette for overview and tool-result
  graphs. Validated concepts take precedence. Source directories supply stable
  fallback colors, including while concept evidence is unavailable.
  Structure uses the shared directory palette without concept assignments.
  Neutral removes category colors but retains status and selection cues.
  A source-directory group with members in several subfolders shows the color
  of its home folder. Other mixed or unidentified groups, and the folded
  remainder, stay neutral. Color is not a similarity or confidence score.
  Palette changes do not move nodes.
- **Detail:** Calm, Balanced, or Detailed. Folded groups retain their member
  counts. The full overview still exposes every supplied node.
- **Labels:** Key, More, or On hover. Label placement avoids collisions.
  Documented concepts and large groups take priority over incidental hubs.
  Tooltips and group exploration remain available in every mode.
- **Links:** Soft or Clear contrast. Direction and relation styles do not change.
- **Motion:** System follows the terminal's reduced-motion preference.
  Off disables graph animation, spinner rotation, shimmer, and animated
  percentage digits. Reduced-motion mode also disables these effects.
  Activity updates do not move nodes.
- **Popup size:** Small, Medium, or Large. The header grip changes the same setting.

Use the arrow keys or Enter to change choices. Contextual help explains the
selected choice. Escape closes the settings card.
Closing the card preserves the composer draft.
Preferences are saved in `<OMP agent directory>/semctl/graph-appearance.json`.
The default path is `~/.omp/agent/semctl/graph-appearance.json`.
Named OMP profiles use their own agent directory. Save failures keep the active
choices in memory and expose a Retry action. Reset defaults restores the default
appearance; it does not change indexed state.
Appearance files use version 2. Version 1 files retain all saved choices and
start with the Small popup. Loading an old file does not rewrite it.

### OMP layout sync indicator

A persistent composer widget reports producer-backed coordinator snapshots
from `notifications/semctl/sync_status`. The version-1 notification carries the
existing coordinator status, an initial reset, or a detached-root reset. It does
not introduce another indexing state machine, polling timer, or progress owner.
The deepest observed checkout containing the session directory supplies the
indicator; unrelated checkouts do not replace it.

The composer indicator sits above the native composer controls. Its compact
button uses the native interface font and keeps a fixed width and height.
It does not consume space in the host's status row.
The widget, progress circle, and `semctl` label stay mounted across notifications.
When the extension has a cached project map for the checkout, the progress
circle shows the ready share of that map. This is the same share as the live
popup header. Nodes without a reported state do not count.
Without a cached map, or without reported states, the circle shows the
producer-reported progress.
The hover names the measure, for example `Ring: 224 of 1617 files ready (13%)`.
The widget does not start a map read to get this share. The map loads after the
first observed semctx call. During embedding, progress observations of the same
job refresh the cached map at most once in 30 seconds, also while the popup is closed.
Tone, symbol, and label text stay producer-driven.
Active work with an unknown total uses a slow, subtle opacity change.
Queued work, warnings, errors, and idle states do not animate continuously.
Completion returns the button to a quieter appearance with a brief fade.
Reduced-motion mode disables these effects. No animation invents progress.
Plain terminals retain one Unicode progress mark.
Hover and status details retain the exact producer counts.
The indicator's context menu also opens details, the project graph, or the live popup.

With producer progress, a full circle requires observed embedding completion and an active watcher.
With a ready share, a full circle requires that every reported node is ready.
Routine preparing, scanning, and planning retain the last completed-job ring,
with an explicit checking description. New uploads invalidate that completion
until a newer job is observed. Pending work, errors, and unavailable evidence
do not invent success. This is not a live source-freshness guarantee.

Select the native composer indicator, or use `/semctl` or `/semctl status`, to
inspect the latest received snapshot in a bounded, read-only native popup.
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

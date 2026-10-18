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

`sync_status` leads with the current failure or phase, then separates persisted
catalog state from the last recorded server job. Counts include failed files;
a completed or zero-file job does not prove checkout freshness. Job labels use
Done, Running, Pending and Failed, without colored emoji. Native diagnostics
remain complete. This is a status snapshot, not an invented live percentage or
elapsed-time measurement.

Local stale-file warnings remain next to the location. `current_context` lists
exact indexed-copy identities, canonical status and the matching checkout.
Codebase graph state is not a claim about each copy's freshness. Tool names and
argument schemas stay stable. Display titles are available to clients that
support MCP titles. Failed operations set `isError` and retain their explanation.

OMP uses its built-in MCP tool cards, including when hosted in Tern. Keep
`mcp.renderMarkdownResults` enabled to render headings, tables and source fences.
OMP controls the card title, colors and collapsed preview length. Its native
Show all control expands the returned payload; semctl does not shorten source
text to create the preview or add a competing expander. Host output-size limits
still apply. Nested code-block headers may occupy the collapsed preview, so expand
source cards to inspect highlighted code. The lifecycle adapter does not register
duplicate tools or replace result text. OMP draws its script-status glyphs through
a native component that its MCP Markdown renderer does not expose; the plain
checkmark beside Done is not that themed component.
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

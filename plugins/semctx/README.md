# semctx coding-agent plugin

This directory is the shared plugin package. Agent-neutral components live once
at their conventional root locations:

- `skills/` contains reusable agent skills.
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
Do not manually copy shared skills; when a host requires a projected format,
generate it from the canonical skill and verify parity in CI.

Then add the host in three registries:

1. implement the Rust `Host` adapter and append it to `hosts()` in
   `src/commands/install/mod.rs`;
2. add its command builder and optional preflight to `HOST_ADAPTERS` in
   `scripts/run_skill_evals.py`; and
3. add the host id to the skill's `evals.json`.

A host is supported only when install, status, update, and uninstall behavior
are implemented and its eval session passes. MCP-only agents should launch
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

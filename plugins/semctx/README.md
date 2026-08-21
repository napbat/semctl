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
inject the returned context. When an enabled semctx MCP tool is exposed either
top-level or through OMP's `xd://` catalog, the adapter also appends a short
host-specific routing block after OMP's base prompt. That block resolves OMP's
generic LSP-first rule; the complete retrieval policy remains in the canonical
skill. Do not manually copy shared skills; when a host requires a projected
format, generate it from the canonical skill and verify parity in CI.

The OMP adapter intentionally composes with, rather than replaces, OMP's native
LSP and memory systems. Indexed repository discovery and graph/flow questions
route to semctx first; diagnostics, hover, code actions, formatting, and live
edit validation stay with LSP. The adapter does not forward native LSP actions
to the shell hook; its per-turn routing block resolves the host-level precedence
conflict before tool selection. The non-blocking drift nudge watches only broad
built-in search. Every semctx MCP call is forwarded as silent compliance so the
shared policy can cool immediate reminders and re-arm after a bounded broad-search
streak; a newer semctx call also invalidates any older in-flight nudge. Ordinary
rename remains LSP-first; semctx symbolic edits are appropriate when guarded
transactions, safe-delete analysis, or undo are specifically useful.
Orientation and prompt hits use custom messages (excluded from OMP's
retained user/assistant memory transcript), and nudges use a one-provider-call
`context` injection. Do not save indexed snippets through `ctx.memory`: the
server index is the freshness boundary, while OMP memory is for durable
conversation knowledge.

OMP's general-purpose `task` subagent inherits the parent MCP manager, skills,
and extension paths, so it receives this integration. Some bundled specialist
agents (`scout`, `reviewer`, `librarian`, and `security-reviewer`) declare a
fixed built-in tool allowlist that omits MCP tools; use `task` for delegated
semctx-backed discovery unless that upstream agent definition is explicitly
extended with semctx tool names.

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

The MCP server exposes symbolic edits as immediate, approved checkout actions.
Each action consumes the server-generated plan internally at semctl's verified
local mutation boundary; raw plans are not MCP tools. Rename, delete, body
replacement, insertion, and `undo_edit` all carry write/destructive annotations.
Keep those annotations and the canonical skill/server tool lists in sync—the
steering tests reject missing docs, phantom tools, and safety drift.

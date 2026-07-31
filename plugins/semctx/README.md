# semctx coding-agent plugin

This directory is the shared plugin package. Agent-neutral components live once
at their conventional root locations:

- `skills/` contains reusable agent skills.
- `hooks/hooks.json` contains the lifecycle-hook subset supported by every
  current host.

Each coding agent keeps only the adapter material that cannot be shared:

- its required discovery manifest (for example `.codex-plugin/plugin.json` or
  `.claude-plugin/plugin.json`);
- its marketplace metadata; and
- its host-specific install/update commands and configuration syntax.

To add another coding agent, point its marketplace at this plugin root and its
adapter at `skills/` and the shared hooks wherever its plugin format allows. Add
an `adapters/<host>/` directory only when its required files have no conventional
location at the plugin root. Do not manually copy shared skills; when a host
requires a projected format, generate it from the canonical skill and verify
parity in CI.

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

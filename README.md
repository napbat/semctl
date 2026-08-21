# semctl

`semctl` is the command-line client for the **semctx** code-intelligence server —
semantic + exact code search and a precise symbol/call/value-flow graph over your
repositories, exposed both as a CLI and as an [MCP](https://modelcontextprotocol.io)
stdio server for AI coding tools (Claude Code, Codex).

It's a thin HTTP+JSON client: all indexing and retrieval happen server-side, so
`semctl` links no engine code and has an entirely public dependency graph.

## Install

The one-liner downloads the prebuilt binary from the latest GitHub release,
verifies its checksum, and hands off to `semctl install` — which puts the binary
on your PATH and wires it into the AI tools it finds. No Rust toolchain needed.

**Linux / macOS** (shell):

```sh
curl -fsSL https://raw.githubusercontent.com/napbat/semctl/main/install-cli.sh | sh
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/napbat/semctl/main/install-cli.ps1 | iex
```

### From source

For a platform without a prebuilt binary — or if you'd rather build it — install
with cargo (needs a [Rust toolchain](https://rustup.rs)):

```sh
cargo install --git https://github.com/napbat/semctl --locked semctl
```

Then sign in and index a repo:

```sh
semctl install      # interactive: integrates Claude Code, Codex CLI, and Oh My Pi
semctl auth login   # OIDC device-code sign-in
semctl index        # register + sync the current repo for indexing
```

## Commands

| Command                                               | What it does                                                                                                          |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `semctl search <query>`                               | Cross-domain semantic search.                                                                                         |
| `semctl index`                                        | Register the current Git worktree (or selected non-Git folder) and sync its files.                                    |
| `semctl graph …`                                      | Exact code intelligence plus symbol search, type hierarchy, call graphs, cycles, unused, duplicates, and rich outlines. |
| `semctl edit …`                                       | Grammar-native rename/delete/body/insertion planning and verified local apply/undo.                                   |
| `semctl files …`                                      | File catalog (`tree` / filtered `list`) and revision-pinned line/byte source reads.                                    |
| `semctl inspect …`                                    | Projects/domains plus visible codebases and the effective server/tenant/index/graph context.                          |
| `semctl mcp`                                          | Run as an MCP stdio server (launched by the host, not by hand).                                                       |
| `semctl install`                                      | Add/remove the editor/agent integrations.                                                                             |
| `semctl uninstall`                                    | Reverse `install`: unwire the tools, remove from PATH, delete the binary (`--purge` also drops config + credentials). |
| `semctl upgrade`                                      | Update the `semctl` binary in place.                                                                                  |
| `semctl auth login` / `logout` / `whoami` / `tenants` | Account & session.                                                                                                    |

## Grammar-native editing

The server's edit operations are planners: they resolve one qualified or
positional symbol through semctx's grammar graph, reparse and re-resolve the
proposed postimages, then return a JSON `WorkspaceEditPlan`. They never write a
repository. For example:

```sh
semctl edit rename --path src/lib.rs --line 42 --column 8 NewName > plan.json
semctl edit apply plan.json
semctl edit undo <plan-id>
```

`edit apply` is the only normal mutation boundary. Before touching the bound
checkout it refreshes the server's checkout lease and graph generation,
canonicalizes every planned path, verifies every preimage, edit range, and
expected postimage, and stages the whole multi-file plan with rollback
sidecars. Reapplying an unchanged completed plan is idempotent. Undo succeeds
only while all current files still match the retained postimage hashes.

Private preimages live only under the local config directory's `edit-history/`
folder and are never sent back to the server. A plan-supplied formatter is not
run unless `--run-formatter` is explicit; formatter commands, arguments, and
paths are bounded to the approved plan.

The MCP surface exposes edits as immediate actions. `rename_symbol`,
`safe_delete_symbol`, `replace_symbol_body`, `insert_before_symbol`, and
`insert_after_symbol` ask the server for its internal plan and consume it inside
the same call; callers never shuttle plan JSON through MCP. Each action is
write/destructive in the host approval UI, returns an `editId`, and can be
reversed with the hash-guarded `undo_edit` action. Read-only additions include
`list_codebases`, `current_context`, `read_source`, `search_symbols`,
`type_hierarchy`, `call_graph`, `cycles`, `unused`, and `duplicates`; existing
reference/search/outline tools expose the richer occurrence, scope, and nesting
options instead of gaining aliases.

### Coming from the old `semctx` CLI

The first `semctl install` automatically retires a previous `semctx` install: it
migrates your config + credentials to `~/.config/semctl`, removes the old binary
and its PATH entry, and re-points the Claude Code / Codex plugin at semctl's
source (the plugin itself stays the same). A `cargo install`'d `semctx` is left
for you to remove with `cargo uninstall semctx-cli`.

## Updating

```sh
semctl upgrade
```

For a binary installed the standard way (the install script), `upgrade` downloads
the latest release and self-replaces in place. If `semctl` was installed with
`cargo install`, it's cargo-managed — `upgrade` detects that and tells you to
re-run `cargo install … --force` instead of clobbering it.

## Configuration

Config lives at `~/.config/semctl/config.toml` (credentials in a sibling
`0600 credentials.json`); `XDG_CONFIG_HOME` overrides the base. A non-secret
`installation-id` in the same directory lets the server distinguish concurrent
local checkouts without receiving a host name or local path. Local codebases are
mapped by canonical checkout path, not guessed from Git remote or folder name:
two clones with the same display name therefore get separate UUIDs and opaque,
collision-safe slugs. An older `~/.config/semctx/` install is read as a fallback
so a rename doesn't force a re-login.

`sync-cache/` stores non-secret per-checkout file stamps, hashes, and content-
filter decisions so repeated `semctl index` runs do not reread unchanged files.
Source contents are never written to this cache, and `semctl uninstall --purge`
removes it with the rest of the semctl config directory.

| Setting         | Flag         | Env               |
| --------------- | ------------ | ----------------- |
| Server base URL | `--server`   | `SEMCTX_SERVER`   |
| Active tenant   | `--tenant`   | `SEMCTX_TENANT`   |
| Active codebase | `--codebase` | `SEMCTX_CODEBASE` |

Login validates the saved active tenant against the new account's memberships
and automatically selects a sole membership. If a saved tenant later becomes
invalid, an authenticated request repairs it the same way and retries once.
Explicit `--tenant` / `SEMCTX_TENANT` overrides are never replaced; with
multiple memberships, run `semctl auth tenants` in an interactive terminal to
pick by number, or choose non-interactively with
`semctl auth tenants --switch <slug>`.

With nothing configured, `semctl` talks to the hosted server at
`https://semctx.napbat.ca`.

### Agent integration data flow

The MCP server sends retrieval queries and opted-in repository content to the
configured semctx server. When trusted Claude/Codex hooks or the native Oh My Pi
extension are enabled, retrieval-shaped user prompts are also sent to `semctl
hook` for a bounded, best-effort candidate search. The OMP extension runs
in-process but delegates retrieval, authentication, and nudge state to the
`semctl` subprocess; it does not make network requests itself. Set
`SEMCTX_HOOK_DISABLE=1` to disable all hook behavior or
`SEMCTX_NUDGE_DISABLE=1` to disable only shell-search reminders.

Observed semctx use cools further reminders for the active prompt. A new prompt
or context reset re-arms guidance immediately; within one prompt, three
consecutive broad built-in searches re-arm it by default. Tune that streak with
`SEMCTX_NUDGE_REARM_BROAD`, and tune the existing escalation ladder with
`SEMCTX_NUDGE_GRACE`, `SEMCTX_NUDGE_COOLDOWN`, and `SEMCTX_NUDGE_MAX` (default
cap: four successful nudges per clear/compact segment).

## Development

Run `prek install` once to install the repository's pre-commit hook. The complete
local/CI-equivalent gate is:

```sh
prek run --all-files
```

The configured hooks run:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
cargo test --workspace
bun test plugins/semctx/adapters/omp/index.test.ts
```

`plugins/semctx/` is the shared plugin root for every supported coding agent.
Its `skills/codebase-retrieval/` has one physical source. Claude/Codex consume
the shared hook manifest; OMP loads `adapters/omp/index.ts`, which maps native
lifecycle events onto the same `semctl hook` wire contract. See
[`plugins/semctx/README.md`](plugins/semctx/README.md) before adding another
agent integration. Do not copy or symlink shared skills.

The deterministic hook and OMP-extension cases run under the normal Rust and Bun
test commands. To record fresh Codex, Claude, and OMP tool-event traces for the
model-level golden prompts:

```sh
python scripts/run_skill_evals.py --host all
```

CI (`.github/workflows/ci.yml`) runs the same checks — clippy is `pedantic`, and
warnings (including broken doc links) fail the build.

Releases are cut by **bumping the version**: set a new `version` in `Cargo.toml`
(e.g. `0.1.0` → `0.1.1`) and push to `main`. `.github/workflows/release.yml`
detects the new version, builds the per-platform binaries, and publishes them as
release `v<version>`; `semctl upgrade` then picks it up. Pushes that don't change
the version are a no-op.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

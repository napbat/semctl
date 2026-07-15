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
semctl install      # interactive: adds the MCP server + skill + hooks to Claude Code / Codex
semctl auth login   # OIDC device-code sign-in
semctl index        # register + sync the current repo for indexing
```

## Commands

| Command | What it does |
| --- | --- |
| `semctl search <query>` | Cross-domain semantic search. |
| `semctl index` | Register the current folder as a codebase and sync its files. |
| `semctl graph …` | Exact code intelligence: definitions, references, callers, implementations, call/value-flow paths. |
| `semctl files …` | The codebase's file catalog (`tree` / filtered `list`). |
| `semctl inspect …` | Detected `projects` graph and registered `domains`. |
| `semctl mcp` | Run as an MCP stdio server (launched by the host, not by hand). |
| `semctl install` | Add/remove the editor/agent integrations. |
| `semctl uninstall` | Reverse `install`: unwire the tools, remove from PATH, delete the binary (`--purge` also drops config + credentials). |
| `semctl upgrade` | Update the `semctl` binary in place. |
| `semctl auth login` / `logout` / `whoami` / `tenants` | Account & session. |

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
`0600 credentials.json`); `XDG_CONFIG_HOME` overrides the base. An older
`~/.config/semctx/` install is read as a fallback so a rename doesn't force a
re-login.

| Setting | Flag | Env |
| --- | --- | --- |
| Server base URL | `--server` | `SEMCTX_SERVER` |
| Active tenant | `--tenant` | `SEMCTX_TENANT` |
| Active codebase | `--codebase` | `SEMCTX_CODEBASE` |

With nothing configured, `semctl` talks to the hosted server at
`https://semctx.napbat.ca`.

## Development

```sh
cargo build
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items
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

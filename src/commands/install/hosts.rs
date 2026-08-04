//! The concrete `Host` implementations for Claude Code, Codex CLI, and Oh My
//! Pi. The generic framework — the [`Host`] trait, interactive picker, and
//! reconcile loop — lives in the parent module; this file contains only the
//! host drivers and their per-CLI quirks.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use super::{Host, HostStatus};
use crate::term::print_hint;

/// Claude Code, wired via its own plugin system so semctl arrives as the full
/// plugin (MCP server + `codebase-retrieval` skill + prompt-context/PreToolUse
/// hooks), not a bare MCP server. We drive the `claude` CLI rather than editing
/// config by hand, so the marketplace auto-update path keeps working. The plugin
/// invokes `semctl` by bare name, so `run` first puts it on PATH (see
/// [`super::ensure_semctl_on_path`]).
pub struct ClaudeCode;

const MARKETPLACE_SLUG: &str = "napbat/semctl"; // owner/repo for `marketplace add`
const MARKETPLACE_NAME: &str = "semctx"; // marketplace name for `marketplace update`
const PLUGIN: &str = "semctx@semctx"; // <plugin>@<marketplace>

impl Host for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn status(&self) -> Result<HostStatus> {
        #[derive(serde::Deserialize)]
        struct Entry {
            id: String,
        }
        // `claude plugin list --json` is the stable, machine-readable probe.
        let out = match Command::new("claude")
            .args(["plugin", "list", "--json"])
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HostStatus::Unavailable(
                    "`claude` CLI not on PATH — install Claude Code".into(),
                ));
            }
            Err(e) => return Err(e).context("run `claude plugin list`"),
        };
        if !out.status.success() {
            return Ok(HostStatus::Unavailable(
                "`claude plugin list` failed".into(),
            ));
        }

        let entries: Vec<Entry> =
            serde_json::from_slice(&out.stdout).context("parse `claude plugin list --json`")?;
        if entries.iter().any(|e| e.id == PLUGIN) {
            Ok(HostStatus::Installed)
        } else {
            Ok(HostStatus::NotInstalled)
        }
    }

    fn install(&self) -> Result<()> {
        // `marketplace add` errors if already registered, and is a no-op if the
        // manifest is already on disk (so it won't re-fetch a changed one). Both
        // are fine — tolerate `add`, then force a refresh with `update`.
        let _ = claude(&["plugin", "marketplace", "add", MARKETPLACE_SLUG]);
        let _ = claude(&["plugin", "marketplace", "update", MARKETPLACE_NAME]);
        claude_checked(&["plugin", "install", PLUGIN, "--scope", "user"])
    }

    fn update(&self) -> Result<()> {
        // Refresh the marketplace manifest from its git source first, so
        // `plugin update` can see a new version / edited skill / hook / MCP
        // config. Tolerate the marketplace refresh failing (offline, transient);
        // `plugin update` then just no-ops on the cached manifest.
        let _ = claude(&["plugin", "marketplace", "update", MARKETPLACE_NAME]);
        claude_checked(&["plugin", "update", PLUGIN, "--scope", "user"])
    }

    fn uninstall(&self) -> Result<()> {
        // `-y` skips the prune confirmation (required when stdout isn't a TTY).
        claude_checked(&["plugin", "uninstall", PLUGIN, "--scope", "user", "-y"])
    }

    fn manual_install_hint(&self) -> Option<String> {
        Some(format!(
            "From inside Claude Code:\n    /plugin marketplace add {MARKETPLACE_SLUG}\n    /plugin install {PLUGIN}"
        ))
    }

    fn remove_legacy(&self) {
        // The old plugin + marketplace carry the same names as the current ones
        // but were sourced from the previous repo; drop both so `install` re-adds
        // the marketplace from the current slug. Tolerate absence.
        let _ = claude(&["plugin", "uninstall", PLUGIN, "--scope", "user", "-y"]);
        let _ = claude(&["plugin", "marketplace", "remove", MARKETPLACE_NAME]);
    }
}

/// Run a `claude` subcommand without attaching Claude's terminal UI to the
/// caller's TTY. Claude probes terminal capabilities when stdout/stderr are
/// terminals; some terminals answer those probes on the shared input queue
/// after Claude exits, leaving escape replies for the user's shell to execute.
/// Capturing both streams suppresses the probes, then replaying them preserves
/// the command's diagnostics.
fn claude(args: &[&str]) -> Result<std::process::ExitStatus> {
    let mut command = Command::new("claude");
    command.args(args);
    run_claude_command(command).context("run `claude` — is the CLI on PATH?")
}

fn run_claude_command(mut command: Command) -> Result<std::process::ExitStatus> {
    let out = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    std::io::stdout()
        .write_all(&out.stdout)
        .context("write `claude` stdout")?;
    std::io::stderr()
        .write_all(&out.stderr)
        .context("write `claude` stderr")?;
    Ok(out.status)
}

/// Like [`claude`] but treats a non-zero exit as an error.
fn claude_checked(args: &[&str]) -> Result<()> {
    let st = claude(args)?;
    if !st.success() {
        bail!("`claude {}` failed ({st})", args.join(" "));
    }
    Ok(())
}

/// `OpenAI` Codex CLI, wired via its plugin + marketplace system — a near-mirror
/// of the Claude path — so semctl arrives as the full plugin (MCP server +
/// `codebase-retrieval` skill + hooks), not a bare MCP server. We drive the
/// `codex plugin` CLI rather than editing `config.toml` by hand. The plugin
/// invokes `semctl` by bare name, so `run` first puts the binary on PATH (see
/// [`super::ensure_semctl_on_path`]).
///
/// One host difference from Claude: Codex does not auto-trust plugin-bundled
/// hooks — the MCP server and skill work immediately, but the prompt-context and
/// nudge hooks stay dormant until the user runs `/hooks` in Codex and trusts
/// them. `install` prints that one manual step.
pub struct Codex;

const CODEX_MARKETPLACE_SLUG: &str = "napbat/semctl"; // owner/repo for `marketplace add`
const CODEX_PLUGIN: &str = "semctx@semctx"; // <plugin>@<marketplace>

impl Host for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn label(&self) -> &'static str {
        "Codex CLI"
    }

    fn status(&self) -> Result<HostStatus> {
        #[derive(serde::Deserialize)]
        struct Entry {
            // Optional: a malformed/renamed entry for some *other* plugin must not
            // fail the whole parse (and thus flip us to Unavailable).
            #[serde(rename = "pluginId")]
            plugin_id: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct List {
            #[serde(default)]
            installed: Vec<Entry>,
        }
        // `is_on_path` (not a bare spawn) is the presence check: on Windows npm
        // ships `codex` as `codex.cmd`/`codex.ps1` with no `codex.exe`, and
        // `Command::new("codex")` would report it missing.
        if !is_on_path("codex") {
            return Ok(HostStatus::Unavailable(
                "`codex` CLI not on PATH — install Codex CLI".into(),
            ));
        }
        // `codex plugin list --json` → { installed: [{ pluginId, .. }], .. }.
        // Every failure path below degrades to `Unavailable`, never `Err`: one
        // host's odd/old/broken CLI must not abort the whole `semctl install`.
        let Ok(out) = tool_cmd("codex")
            .args(["plugin", "list", "--json"])
            .output()
        else {
            return Ok(HostStatus::Unavailable(
                "`codex` CLI could not be run — check the Codex install".into(),
            ));
        };
        if !out.status.success() {
            // Also catches a Codex too old to have `plugin` subcommands.
            return Ok(HostStatus::Unavailable(
                "`codex plugin list` failed — update Codex CLI".into(),
            ));
        }

        // An unexpected shape (Codex version skew) → Unavailable, not a hard error.
        let Ok(list) = serde_json::from_slice::<List>(&out.stdout) else {
            return Ok(HostStatus::Unavailable(
                "unexpected `codex plugin list --json` output — update Codex CLI".into(),
            ));
        };
        if list
            .installed
            .iter()
            .any(|e| e.plugin_id.as_deref() == Some(CODEX_PLUGIN))
        {
            Ok(HostStatus::Installed)
        } else {
            Ok(HostStatus::NotInstalled)
        }
    }

    fn install(&self) -> Result<()> {
        // `marketplace add` errors if already registered; tolerate it, then force
        // a snapshot refresh with `upgrade` before installing the plugin.
        let _ = codex(&["plugin", "marketplace", "add", CODEX_MARKETPLACE_SLUG]);
        let _ = codex(&["plugin", "marketplace", "upgrade"]);
        codex_checked(&["plugin", "add", CODEX_PLUGIN])?;
        codex_hook_trust_notice();
        Ok(())
    }

    fn update(&self) -> Result<()> {
        // Refresh the marketplace snapshot from git, then re-add to pick up a new
        // version / edited skill / hook / MCP config (Codex has no `plugin
        // update`; re-`add` reinstalls from the upgraded snapshot).
        let _ = codex(&["plugin", "marketplace", "upgrade"]);
        codex_checked(&["plugin", "add", CODEX_PLUGIN])?;
        codex_hook_trust_notice();
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        codex_checked(&["plugin", "remove", CODEX_PLUGIN])
    }

    fn manual_install_hint(&self) -> Option<String> {
        Some(format!(
            "Install Codex CLI, then:\n    codex plugin marketplace add {CODEX_MARKETPLACE_SLUG}\n    codex plugin add {CODEX_PLUGIN}\n    then run /hooks in Codex to trust the semctl hooks"
        ))
    }

    fn remove_legacy(&self) {
        // Drop the old plugin + marketplace (same names, previous-repo source) so
        // `install` re-adds the marketplace from the current slug. Tolerate a
        // Codex too old to have these subcommands, or nothing to remove.
        let _ = codex(&["plugin", "remove", CODEX_PLUGIN]);
        let _ = codex(&["plugin", "marketplace", "remove", MARKETPLACE_NAME]);
    }
}

/// Oh My Pi, wired through its marketplace so semctl arrives as the shared MCP
/// server + retrieval skill plus the native OMP lifecycle extension. OMP may be
/// installed through Bun/npm, so every invocation goes through [`tool_cmd`].
pub struct Omp;

#[derive(serde::Deserialize)]
struct OmpPluginEntry {
    scope: Option<String>,
}

#[derive(serde::Deserialize)]
struct OmpPluginSummary {
    id: Option<String>,
    scope: Option<String>,
    #[serde(default)]
    entries: Vec<OmpPluginEntry>,
}

#[derive(serde::Deserialize)]
struct OmpPluginList {
    #[serde(default)]
    marketplace: Vec<OmpPluginSummary>,
}

fn omp_user_plugin_installed(json: &[u8]) -> serde_json::Result<bool> {
    let list: OmpPluginList = serde_json::from_slice(json)?;
    Ok(list.marketplace.iter().any(|plugin| {
        plugin.id.as_deref() == Some(PLUGIN)
            && (plugin.scope.as_deref() == Some("user")
                || plugin
                    .entries
                    .iter()
                    .any(|entry| entry.scope.as_deref() == Some("user")))
    }))
}

impl Host for Omp {
    fn id(&self) -> &'static str {
        "omp"
    }

    fn label(&self) -> &'static str {
        "Oh My Pi"
    }

    fn status(&self) -> Result<HostStatus> {
        if !is_on_path("omp") {
            return Ok(HostStatus::Unavailable(
                "`omp` CLI not on PATH — install Oh My Pi".into(),
            ));
        }
        let Ok(out) = tool_cmd("omp").args(["plugin", "list", "--json"]).output() else {
            return Ok(HostStatus::Unavailable(
                "`omp plugin list` could not be run — check the Oh My Pi install".into(),
            ));
        };
        if !out.status.success() {
            return Ok(HostStatus::Unavailable(
                "`omp plugin list` failed — update Oh My Pi".into(),
            ));
        }
        let Ok(installed) = omp_user_plugin_installed(&out.stdout) else {
            return Ok(HostStatus::Unavailable(
                "unexpected `omp plugin list --json` output — update Oh My Pi".into(),
            ));
        };
        Ok(if installed {
            HostStatus::Installed
        } else {
            HostStatus::NotInstalled
        })
    }

    fn install(&self) -> Result<()> {
        // `marketplace add` rejects an existing source; tolerate it, then refresh
        // the catalog so a re-run always sees the current plugin version.
        let _ = omp(&["plugin", "marketplace", "add", MARKETPLACE_SLUG]);
        let _ = omp(&["plugin", "marketplace", "update", MARKETPLACE_NAME]);
        omp_checked(&["plugin", "install", PLUGIN, "--scope", "user"])
    }

    fn update(&self) -> Result<()> {
        let _ = omp(&["plugin", "marketplace", "update", MARKETPLACE_NAME]);
        omp_checked(&["plugin", "upgrade", "--scope", "user", PLUGIN])
    }

    fn uninstall(&self) -> Result<()> {
        omp_checked(&["plugin", "uninstall", "--scope", "user", PLUGIN])
    }

    fn manual_install_hint(&self) -> Option<String> {
        Some(format!(
            "Install Oh My Pi, then:\n    omp plugin marketplace add {MARKETPLACE_SLUG}\n    \
             omp plugin install {PLUGIN} --scope user"
        ))
    }
}

fn omp(args: &[&str]) -> Result<std::process::ExitStatus> {
    tool_cmd("omp")
        .args(args)
        .status()
        .context("run `omp` — is the CLI on PATH?")
}

fn omp_checked(args: &[&str]) -> Result<()> {
    let st = omp(args)?;
    if !st.success() {
        bail!("`omp {}` failed ({st})", args.join(" "));
    }
    Ok(())
}

/// Print the one manual step Codex needs after install: trusting the plugin's
/// hooks. The MCP server and skill are live immediately; the prompt-context and
/// nudge hooks stay dormant until trusted. (The `hooks` feature is on by default,
/// so there's nothing else to enable.)
fn codex_hook_trust_notice() {
    print_hint(
        "Codex won't run plugin hooks until you trust them: open Codex and run `/hooks`, \
         then trust the semctl hooks. The MCP tools and codebase-retrieval skill already \
         work; the prompt-context and search-nudge hooks activate once trusted.",
    );
}

/// Run a `codex` subcommand, inheriting stdio so its progress shows through.
/// Goes via [`tool_cmd`] so a Windows npm-shim `codex` launches correctly.
fn codex(args: &[&str]) -> Result<std::process::ExitStatus> {
    tool_cmd("codex")
        .args(args)
        .status()
        .context("run `codex` — is the CLI on PATH?")
}

/// Whether `program` resolves on PATH. On Windows this honors PATHEXT (and the
/// npm `.ps1` shim), so an npm-installed CLI counts — `Command::new(program)`
/// alone only finds a real `program.exe`, so a `.cmd`/`.ps1`-only install would
/// look missing.
fn is_on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let names = candidate_names(program);
    std::env::split_paths(&path).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
}

/// The filenames `program` might have on disk. On Windows an npm CLI is a
/// `.cmd`/`.ps1` shim (no `.exe`), so include the PATHEXT extensions and `.ps1`.
#[cfg(windows)]
fn candidate_names(program: &str) -> Vec<String> {
    let mut names = vec![program.to_string()];
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    for ext in exts.split(';').filter(|e| !e.is_empty()) {
        names.push(format!("{program}{}", ext.to_ascii_lowercase()));
    }
    names.push(format!("{program}.ps1")); // npm shim; `.ps1` isn't in PATHEXT
    names
}

#[cfg(not(windows))]
fn candidate_names(program: &str) -> Vec<String> {
    vec![program.to_string()]
}

/// A `Command` for a CLI that may be installed as a Windows npm shim. npm ships
/// clis as `foo.cmd`/`foo.ps1` (no `foo.exe`), which `Command::new("foo")` can't
/// spawn — `CreateProcess` only appends `.exe`. On Windows we route through
/// `cmd /C foo …` so the shim resolves via PATHEXT; elsewhere we spawn directly.
/// (Args here are fixed literals with no shell metacharacters, so `cmd /C` is
/// safe.)
#[cfg(windows)]
fn tool_cmd(program: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(program);
    c
}

#[cfg(not(windows))]
fn tool_cmd(program: &str) -> Command {
    Command::new(program)
}

/// Like [`codex`] but treats a non-zero exit as an error.
fn codex_checked(args: &[&str]) -> Result<()> {
    let st = codex(args)?;
    if !st.success() {
        bail!("`codex {}` failed ({st})", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::IsTerminal;
    use std::path::{Path, PathBuf};

    fn repo(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }
    fn json(rel: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(repo(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {rel}: {e}"))
    }

    #[test]
    fn claude_child_observes_non_terminal_stdio() {
        if std::env::var_os("SEMCTX_TEST_CLAUDE_CHILD").is_none() {
            return;
        }
        assert!(!std::io::stdin().is_terminal());
        assert!(!std::io::stdout().is_terminal());
        assert!(!std::io::stderr().is_terminal());
    }

    #[test]
    fn claude_runner_detaches_child_from_terminal() {
        let mut child = Command::new(std::env::current_exe().expect("current test binary"));
        child.args([
            "--exact",
            "commands::install::hosts::tests::claude_child_observes_non_terminal_stdio",
            "--nocapture",
        ]);
        child.env("SEMCTX_TEST_CLAUDE_CHILD", "1");

        let status = run_claude_command(child).expect("run child test");
        assert!(status.success(), "child saw terminal-attached stdio");
    }

    #[test]
    fn omp_status_requires_a_user_scope_install() {
        let user = br#"{"npm":[],"marketplace":[{"id":"semctx@semctx","scope":"user","entries":[{"scope":"user"}]}]}"#;
        assert!(omp_user_plugin_installed(user).expect("parse user install"));

        let project = br#"{"npm":[],"marketplace":[{"id":"semctx@semctx","scope":"project","entries":[{"scope":"project"}]}]}"#;
        assert!(
            !omp_user_plugin_installed(project).expect("parse project install"),
            "a project-only install must not satisfy semctl's user-scope wiring"
        );

        let unrelated =
            br#"{"npm":[],"marketplace":[{"id":"other@tools","scope":"user","entries":[]}]}"#;
        assert!(!omp_user_plugin_installed(unrelated).expect("parse unrelated install"));
    }

    // Drift guard for the shared plugin root and thin host adapters. Shared
    // skills/hooks live once; each host manifest may point at its own wire
    // formats without growing another package tree.
    #[test]
    fn agent_plugin_manifests_share_root_assets() {
        let market = json(".agents/plugins/marketplace.json");
        let claude_market = json(".claude-plugin/marketplace.json");
        let plugin = json("plugins/semctx/.codex-plugin/plugin.json");
        let claude_plugin = json("plugins/semctx/.claude-plugin/plugin.json");
        let omp_package = json("plugins/semctx/package.json");

        // marketplace name + plugin name must compose to exactly what install()
        // and status() use (`semctx@semctx`), and the slug must be the git source.
        let market_name = market["name"].as_str().expect("marketplace name");
        let plugin_name = market["plugins"][0]["name"].as_str().expect("plugin name");
        assert_eq!(
            format!("{plugin_name}@{market_name}"),
            CODEX_PLUGIN,
            "marketplace/plugin names must compose to CODEX_PLUGIN"
        );
        assert_eq!(
            plugin["name"].as_str().expect("plugin.json name"),
            plugin_name,
            "plugin.json name must match the marketplace entry"
        );
        assert_eq!(
            plugin["version"], claude_plugin["version"],
            "Codex and Claude packages should release the shared integration together"
        );
        assert_eq!(
            plugin["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "agent plugin manifests must use the semctl release version"
        );
        assert_eq!(
            omp_package["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "OMP and semctl must release together"
        );
        assert_eq!(
            omp_package["name"], plugin["name"],
            "OMP package must keep the shared plugin name"
        );
        let omp_extension = omp_package["omp"]["extensions"][0]
            .as_str()
            .expect("OMP extension entry");
        assert!(
            repo("plugins/semctx").join(omp_extension).is_file(),
            "OMP extension -> {omp_extension} must exist"
        );
        assert_eq!(
            market["plugins"][0]["policy"]["authentication"], "ON_INSTALL",
            "marketplace entries must declare their authentication timing"
        );
        assert_eq!(
            market["plugins"][0]["source"]["path"], "./plugins/semctx",
            "the Codex marketplace must point at the shared plugin root"
        );
        assert_eq!(
            claude_market["plugins"][0]["source"], "./plugins/semctx",
            "the Claude marketplace must point at the shared plugin root"
        );
        assert_eq!(CODEX_MARKETPLACE_SLUG, "napbat/semctl");

        // Both hosts consume the same physical skill/hook tree and declare the
        // same MCP server inline. Future hosts can add thin adapters without
        // growing another package tree.
        let base = repo("plugins/semctx");
        let skills = plugin["skills"].as_str().expect("Codex plugin skills");
        let claude_skills = claude_plugin["skills"]
            .as_str()
            .expect("Claude plugin skills");
        assert_eq!(skills, claude_skills, "host skill paths must stay shared");
        assert!(base.join(skills).is_dir(), "skills -> {skills} must exist");
        assert!(
            base.join("hooks/hooks.json").is_file(),
            "hosts must discover the shared default hooks/hooks.json"
        );
        assert_eq!(
            plugin["mcpServers"], claude_plugin["mcpServers"],
            "hosts that accept inline MCP maps should share the same declaration"
        );
        assert_eq!(
            plugin["mcpServers"]["semctx"]["command"].as_str(),
            Some("semctl"),
            "the shared MCP declaration must run semctl"
        );
        for key in [
            "displayName",
            "shortDescription",
            "longDescription",
            "developerName",
            "category",
            "capabilities",
            "websiteURL",
            "defaultPrompt",
        ] {
            assert!(
                !plugin["interface"][key].is_null(),
                "plugin.json interface.{key} is required"
            );
        }
    }
}

//! `semctl install` — reconcile semctl's integration into each supported host.
//!
//! Declarative, not additive: the checklist shows the *desired* end-state — a
//! checked host gets installed, an unchecked one uninstalled if present. The
//! binary owns all the wiring; there's no external shell installer to keep in
//! sync.
//!
//! Host-agnostic: add a host by implementing [`Host`] and registering it in
//! [`hosts()`]. The picker and reconcile loop don't care which host they drive.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use inquire::{InquireError, MultiSelect};

use crate::config;
use crate::term::{ok, print_hint, say, warn};

mod hosts;
mod selfpath;

use hosts::{ClaudeCode, Codex, Omp};
use selfpath::BinaryRemoval;

/// How this `semctl` binary got onto the machine — decides whether `upgrade`
/// self-replaces it in place, and whether `uninstall` deletes it or defers to
/// the tool that manages it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// A prebuilt binary from a GitHub release (the install script, or a prior
    /// `semctl upgrade`). Release builds stamp `SEMCTL_DIST=github-release` at
    /// compile time — see `.github/workflows/release.yml`.
    Release,
    /// Installed by `cargo install` — lives in the cargo bin dir. Updated by
    /// re-running cargo, not by an in-place self-replace.
    Cargo,
    /// A local `cargo build`, a manual copy, or a distro package.
    Other,
}

/// Detect how the running binary was installed.
pub fn install_kind() -> InstallKind {
    if option_env!("SEMCTL_DIST") == Some("github-release") {
        InstallKind::Release
    } else if running_from_cargo_bin() {
        InstallKind::Cargo
    } else {
        InstallKind::Other
    }
}

/// Whether the running executable sits in `$CARGO_HOME/bin` (default
/// `~/.cargo/bin`) — where `cargo install` places binaries.
fn running_from_cargo_bin() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(dir) = exe.parent() else {
        return false;
    };
    let Some(cargo_bin) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
        .map(|c| c.join("bin"))
    else {
        return false;
    };
    // Canonicalize both so a symlinked cargo home / `..` doesn't cause a false miss.
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    norm(dir) == norm(&cargo_bin)
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Hosts to enable, e.g. `semctl install claude`. Reconciles to exactly this
    /// set — hosts you omit are uninstalled if present. Leave empty for the
    /// interactive picker.
    pub hosts: Vec<String>,

    /// Enable every available host (non-interactive).
    #[arg(long, conflicts_with_all = ["none", "hosts"])]
    pub all: bool,

    /// Disable every host — uninstall all present integrations (non-interactive).
    #[arg(long, conflicts_with_all = ["all", "hosts"])]
    pub none: bool,
}

/// A tool semctl can wire itself into (an editor, an agent CLI, …). Every method
/// takes `&self` so the set is trivially object-safe (`Box<dyn Host>`).
pub trait Host {
    /// Stable machine id — also the positional arg the user types. Kebab-case.
    fn id(&self) -> &'static str;
    /// Human label for the checklist.
    fn label(&self) -> &'static str;
    /// Current wiring state. `Unavailable` means we can't manage it on this
    /// machine (e.g. the host's own CLI isn't installed); the picker greys it out
    /// and reconcile falls back to printing [`Host::manual_install_hint`].
    fn status(&self) -> Result<HostStatus>;
    /// Wire semctl in. Must be idempotent — reconcile only calls it when the host
    /// reports `NotInstalled`, but re-runs shouldn't break.
    fn install(&self) -> Result<()>;
    /// Bring an already-installed integration up to date. Reconcile calls this on
    /// every run for an installed, still-desired host, so it must be idempotent
    /// and a no-op when current. Default no-op (for hosts where "installed"
    /// implies "current"); override when the host pulls from a mutable remote
    /// (e.g. a plugin marketplace).
    fn update(&self) -> Result<()> {
        Ok(())
    }
    /// Remove semctl's wiring. Called only when the host reports `Installed`.
    fn uninstall(&self) -> Result<()>;
    /// Copy-pasteable manual steps, shown when the host is `Unavailable` but the
    /// user asked to enable it (e.g. the host CLI is missing so we can't do it).
    fn manual_install_hint(&self) -> Option<String> {
        None
    }
    /// Retire this host's *legacy* `semctx` wiring — the old plugin and its
    /// marketplace registration (same names as now, but sourced from the previous
    /// repo) — so a fresh [`install`](Host::install) re-adds it from semctl's
    /// source. Best-effort, idempotent, default no-op; called only during the
    /// one-time legacy migration.
    fn remove_legacy(&self) {}
}

#[derive(Clone)]
pub enum HostStatus {
    Installed,
    NotInstalled,
    /// Can't be managed here; carries a one-line reason for the user.
    Unavailable(String),
}

/// The registry. Append a host here and the rest of the command picks it up.
fn hosts() -> Vec<Box<dyn Host>> {
    vec![Box::new(ClaudeCode), Box::new(Codex), Box::new(Omp)]
}

/// Refresh every integration that is already installed.
///
/// This function does not install a missing integration or remove an existing
/// integration. It uses the same reconcile path as [`run`], so each host keeps
/// one update implementation for plugin, hook, and MCP configuration changes.
pub(crate) fn refresh_installed() -> Result<usize> {
    let hosts = hosts();
    refresh_installed_hosts(&hosts)
}

fn refresh_installed_hosts(hosts: &[Box<dyn Host>]) -> Result<usize> {
    let snap = snapshot_hosts(hosts);
    let desired = snap
        .iter()
        .filter(|(_, status)| matches!(status, HostStatus::Installed))
        .map(|(host, _)| host.id().to_string())
        .collect();

    reconcile(
        &snap,
        &desired,
        "No installed agent integrations to update.",
    )
}

/// Snapshot every host once. A failed probe must not block another host.
fn snapshot_hosts(hosts: &[Box<dyn Host>]) -> Vec<(&dyn Host, HostStatus)> {
    hosts
        .iter()
        .map(|host| {
            let status = host.status().unwrap_or_else(|error| {
                HostStatus::Unavailable(format!("status check failed — {error:#}"))
            });
            (host.as_ref(), status)
        })
        .collect()
}

pub fn run(args: &InstallArgs) -> Result<()> {
    let hosts = hosts();

    // Retire any previous `semctx` install first, so the snapshot below reflects
    // the post-migration state and reconcile re-adds from semctl's source.
    migrate_legacy(&hosts);

    let snap = snapshot_hosts(&hosts);

    // Reject unknown host names before doing anything.
    if !args.hosts.is_empty() {
        let known: HashSet<&str> = snap.iter().map(|(h, _)| h.id()).collect();
        for want in &args.hosts {
            if !known.contains(want.as_str()) {
                let list = known.iter().copied().collect::<Vec<_>>().join(", ");
                bail!("unknown host {want:?} (known: {list})");
            }
        }
    }

    let Some(desired) = resolve_desired(args, &snap)? else {
        return Ok(()); // cancelled
    };

    // The hosts wire a plugin that invokes `semctl` by bare name, so the binary
    // must be resolvable on PATH. `--all` is also the release installers'
    // non-interactive bootstrap path: install the binary even when no supported
    // host is currently available.
    if args.all || !desired.is_empty() {
        ensure_semctl_on_path();
    }

    reconcile(&snap, &desired, "Nothing to do — no tools selected.")?;
    Ok(())
}

/// One-time retirement of a previous `semctx` install (the CLI's old name) so
/// semctl fully supersedes it. Gated on legacy artifacts, so it's a no-op once
/// nothing old remains. Best-effort: every step is reported, none is fatal.
fn migrate_legacy(hosts: &[Box<dyn Host>]) {
    let legacy = config::legacy_present()
        || selfpath::legacy_binary().is_some()
        || selfpath::legacy_path_block_present();
    if !legacy {
        return;
    }
    say("Retiring a previous semctx install…");

    // Carry ~/.config/semctx over to ~/.config/semctl once, then drop it.
    match config::migrate_from_legacy() {
        Ok(true) => ok("migrated config from ~/.config/semctx"),
        Ok(false) => {}
        Err(e) => warn(&format!("couldn't migrate config: {e:#}")),
    }
    match config::remove_legacy() {
        Ok(true) => ok("removed ~/.config/semctx"),
        Ok(false) => {}
        Err(e) => warn(&format!("couldn't remove ~/.config/semctx: {e:#}")),
    }

    // The old binary and the PATH entry it added.
    match selfpath::remove_legacy_binary() {
        Ok(Some(p)) => ok(&format!("removed legacy {}", p.display())),
        Ok(None) => {}
        Err(e) => warn(&format!("couldn't remove the legacy binary: {e:#}")),
    }
    match selfpath::remove_legacy_from_user_path() {
        Ok(true) => ok("removed the old `semctx install` PATH entry"),
        Ok(false) => {}
        Err(e) => warn(&format!("couldn't update PATH: {e:#}")),
    }

    // The old plugin + marketplace, so reconcile re-adds from semctl's source.
    for h in hosts {
        h.remove_legacy();
    }

    // A cargo-installed old semctx is cargo's to remove — just point at it.
    if let Some(p) = selfpath::legacy_cargo_binary() {
        print_hint(&format!(
            "A cargo-installed semctx remains at {} — remove it with `cargo uninstall semctx-cli`",
            p.display()
        ));
    }
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Also delete the semctl config and stored credentials (`~/.config/semctl`).
    #[arg(long)]
    pub purge: bool,
}

/// `semctl uninstall` — the inverse of [`run`]: unwire every host integration,
/// remove semctl from PATH, and delete the installed binary. `--purge` also
/// removes config + credentials. Every step is best-effort and reported; one
/// failure never aborts the rest (so it's infallible — nothing to bubble up).
pub fn run_uninstall(args: &UninstallArgs) {
    // 1. Unwire every host integration that's currently installed.
    for h in hosts() {
        match h.status() {
            Ok(HostStatus::Installed) => match h.uninstall() {
                Ok(()) => ok(&format!("removed the {} integration", h.label())),
                Err(e) => warn(&format!(
                    "couldn't remove the {} integration: {e:#}",
                    h.label()
                )),
            },
            Ok(_) => {} // not installed, or unmanageable here — nothing to undo
            Err(e) => warn(&format!("couldn't check {}: {e:#}", h.label())),
        }
    }

    // 2. Remove the PATH entry `install` added.
    match selfpath::remove_dir_from_user_path() {
        Ok(true) => ok("removed semctl from your PATH (restart shells to apply)"),
        Ok(false) => {}
        Err(e) => warn(&format!("couldn't update PATH: {e:#}")),
    }

    // 3. Optionally purge config + credentials.
    if args.purge {
        match config::remove_all() {
            Ok(true) => ok("removed ~/.config/semctl (config + credentials)"),
            Ok(false) => {}
            Err(e) => warn(&format!("couldn't remove config: {e:#}")),
        }
    }

    // 4. Delete the installed binary at the canonical location (self-deleting if
    //    it's the one running).
    match selfpath::remove_installed_binary() {
        Ok(BinaryRemoval::Removed { target } | BinaryRemoval::RemovedRunning { target }) => {
            ok(&format!("removed {}", target.display()));
        }
        Ok(BinaryRemoval::NotFound) => {}
        Err(e) => warn(&format!("couldn't remove the installed binary: {e:#}")),
    }

    // 5. A cargo-installed copy lives in the cargo bin dir, which cargo manages —
    //    point the user at cargo rather than deleting it out from under it.
    if install_kind() == InstallKind::Cargo {
        print_hint(
            "This semctl was installed with cargo — remove that copy with: cargo uninstall semctl",
        );
    }

    say("semctl uninstalled.");
}

/// Put the running binary on PATH (see [`selfpath`]) and report what happened.
/// Best-effort: a PATH problem is warned with a manual hint, never fatal — the
/// plugin still wires up, it just won't launch until `semctl` is resolvable.
fn ensure_semctl_on_path() {
    use selfpath::Outcome;
    match selfpath::ensure_on_path() {
        Ok(Outcome::Installed {
            target,
            copied,
            on_path_now,
            shadowed,
        }) => {
            let lead = if copied {
                format!("Installed semctl to {}", target.display())
            } else {
                format!("semctl is installed at {}", target.display())
            };
            if on_path_now {
                ok(&format!("{lead} (on your PATH)"));
            } else {
                ok(&format!("{lead}; added its directory to PATH"));
                print_hint("Restart your shell (and Claude Code) so the new PATH takes effect.");
            }
            if let Some(other) = shadowed {
                warn(&format!(
                    "another semctl on your PATH ({}) may take precedence — remove it or \
                     reorder PATH so the freshly installed one is used",
                    other.display()
                ));
            }
        }
        Ok(Outcome::InstalledPathManual { target, error }) => {
            warn(&format!(
                "Installed semctl to {}, but couldn't update PATH — {error}",
                target.display()
            ));
            if let Some(dir) = target.parent() {
                print_hint(&format!(
                    "Add {} to your PATH so the plugin's MCP server and hooks can launch.",
                    dir.display()
                ));
            }
        }
        Err(e) => {
            warn(&format!(
                "Couldn't put semctl on PATH automatically — {e:#}"
            ));
            if let Some(dir) = selfpath::bin_dir() {
                print_hint(&format!(
                    "Ensure `semctl` is on PATH (e.g. copy this binary into {}).",
                    dir.display()
                ));
            }
        }
    }
}

/// Resolve the target end-state (ids that should be installed). `Ok(None)` means
/// the user cancelled the interactive picker — do nothing.
fn resolve_desired(
    args: &InstallArgs,
    snap: &[(&dyn Host, HostStatus)],
) -> Result<Option<HashSet<String>>> {
    let available = |s: &HostStatus| !matches!(s, HostStatus::Unavailable(_));

    if args.all {
        // Enable everything we can. Unavailable hosts can't be driven, so they're
        // skipped rather than failed — but still surface how to do them by hand.
        note_unavailable(snap);
        return Ok(Some(
            snap.iter()
                .filter(|(_, s)| available(s))
                .map(|(h, _)| h.id().to_string())
                .collect(),
        ));
    }
    if args.none {
        return Ok(Some(HashSet::new()));
    }
    if !args.hosts.is_empty() {
        return Ok(Some(args.hosts.iter().cloned().collect()));
    }

    // Interactive picker — needs a TTY.
    if !std::io::stdin().is_terminal() {
        bail!(
            "no TTY for the interactive picker — pass host names, --all, or --none \
             (e.g. `semctl install claude`)"
        );
    }

    // Surface anything we can't manage before the prompt, with manual steps.
    note_unavailable(snap);

    let choices: Vec<Choice> = snap
        .iter()
        .filter(|(_, s)| available(s))
        .map(|(h, _)| Choice {
            id: h.id(),
            label: h.label(),
        })
        .collect();
    if choices.is_empty() {
        println!("No supported tools available to configure.");
        return Ok(None);
    }
    let defaults: Vec<usize> = choices
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            snap.iter()
                .any(|(h, s)| h.id() == c.id && matches!(s, HostStatus::Installed))
        })
        .map(|(i, _)| i)
        .collect();

    match MultiSelect::new("Wire semctl into which tools?", choices)
        .with_default(&defaults)
        .with_help_message("↑↓ move · space toggle · enter apply · esc cancel")
        .prompt()
    {
        Ok(sel) => Ok(Some(sel.into_iter().map(|c| c.id.to_string()).collect())),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            println!("Cancelled — no changes.");
            Ok(None)
        }
        Err(e) => Err(e).context("interactive picker"),
    }
}

/// Drive each host toward the desired state, printing what happens. Tolerant per
/// host: one failure is reported and counted but doesn't abort the others.
fn reconcile(
    snap: &[(&dyn Host, HostStatus)],
    desired: &HashSet<String>,
    idle_message: &str,
) -> Result<usize> {
    let mut acted = 0usize;
    let mut failed = 0usize;

    for (h, st) in snap {
        let want = desired.contains(h.id());
        match (want, st) {
            (true, HostStatus::Installed) => {
                // Already present — reconcile still refreshes it so a changed
                // plugin (new version / edited skill / hook / MCP config) lands.
                say(&format!("Updating {}…", h.label()));
                match h.update() {
                    Ok(()) => ok(&format!("{}: up to date", h.label())),
                    Err(e) => {
                        warn(&format!("{}: update failed — {e:#}", h.label()));
                        failed += 1;
                    }
                }
                acted += 1;
            }
            (false, HostStatus::NotInstalled | HostStatus::Unavailable(_)) => {}
            (true, HostStatus::NotInstalled) => {
                say(&format!("Installing {}…", h.label()));
                match h.install() {
                    Ok(()) => ok(&format!("{}: installed", h.label())),
                    Err(e) => {
                        warn(&format!("{}: install failed — {e:#}", h.label()));
                        failed += 1;
                    }
                }
                acted += 1;
            }
            (false, HostStatus::Installed) => {
                say(&format!("Removing {}…", h.label()));
                match h.uninstall() {
                    Ok(()) => ok(&format!("{}: removed", h.label())),
                    Err(e) => {
                        warn(&format!("{}: uninstall failed — {e:#}", h.label()));
                        failed += 1;
                    }
                }
                acted += 1;
            }
            (true, HostStatus::Unavailable(reason)) => {
                warn(&format!("{}: can't install — {reason}", h.label()));
                if let Some(hint) = h.manual_install_hint() {
                    print_hint(&hint);
                }
                failed += 1;
            }
        }
    }

    if failed > 0 {
        bail!("{failed} host(s) could not be configured");
    }
    if acted == 0 {
        println!("{idle_message}");
    }
    Ok(acted)
}

/// Print a note + manual steps for every host we can't manage on this machine.
fn note_unavailable(snap: &[(&dyn Host, HostStatus)]) {
    for (h, s) in snap {
        if let HostStatus::Unavailable(reason) = s {
            warn(&format!("{} — unavailable ({reason})", h.label()));
            if let Some(hint) = h.manual_install_hint() {
                print_hint(&hint);
            }
        }
    }
}

/// A picker row. `Display` is the label; we carry the id to map the selection
/// back to a host.
struct Choice {
    id: &'static str,
    label: &'static str,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Default)]
    struct Calls {
        install: Rc<Cell<usize>>,
        update: Rc<Cell<usize>>,
        uninstall: Rc<Cell<usize>>,
    }

    struct FakeHost {
        id: &'static str,
        status: HostStatus,
        calls: Calls,
        update_fails: bool,
    }

    impl Host for FakeHost {
        fn id(&self) -> &'static str {
            self.id
        }

        fn label(&self) -> &'static str {
            self.id
        }

        fn status(&self) -> Result<HostStatus> {
            Ok(self.status.clone())
        }

        fn install(&self) -> Result<()> {
            self.calls.install.set(self.calls.install.get() + 1);
            Ok(())
        }

        fn update(&self) -> Result<()> {
            self.calls.update.set(self.calls.update.get() + 1);
            if self.update_fails {
                bail!("simulated update failure");
            }
            Ok(())
        }

        fn uninstall(&self) -> Result<()> {
            self.calls.uninstall.set(self.calls.uninstall.get() + 1);
            Ok(())
        }
    }

    fn fake_host(
        id: &'static str,
        status: HostStatus,
        update_fails: bool,
    ) -> (Box<dyn Host>, Calls) {
        let calls = Calls::default();
        (
            Box::new(FakeHost {
                id,
                status,
                calls: calls.clone(),
                update_fails,
            }),
            calls,
        )
    }

    #[test]
    fn refresh_updates_only_installed_integrations() {
        let (installed, installed_calls) = fake_host("installed", HostStatus::Installed, false);
        let (missing, missing_calls) = fake_host("missing", HostStatus::NotInstalled, false);
        let (unavailable, unavailable_calls) = fake_host(
            "unavailable",
            HostStatus::Unavailable("host CLI is absent".to_owned()),
            false,
        );
        let hosts = vec![installed, missing, unavailable];

        let refreshed = refresh_installed_hosts(&hosts).expect("refresh should succeed");

        assert_eq!(refreshed, 1);
        assert_eq!(installed_calls.update.get(), 1);
        assert_eq!(installed_calls.install.get(), 0);
        assert_eq!(installed_calls.uninstall.get(), 0);
        for calls in [missing_calls, unavailable_calls] {
            assert_eq!(calls.update.get(), 0);
            assert_eq!(calls.install.get(), 0);
            assert_eq!(calls.uninstall.get(), 0);
        }
    }

    #[test]
    fn refresh_attempts_every_installed_integration_after_a_failure() {
        let (failing, failing_calls) = fake_host("failing", HostStatus::Installed, true);
        let (working, working_calls) = fake_host("working", HostStatus::Installed, false);
        let hosts = vec![failing, working];

        let error = refresh_installed_hosts(&hosts).expect_err("one refresh should fail");

        assert!(
            error
                .to_string()
                .contains("1 host(s) could not be configured")
        );
        assert_eq!(failing_calls.update.get(), 1);
        assert_eq!(working_calls.update.get(), 1);
    }
}

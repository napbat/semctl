//! Put the running `semctl` binary on PATH so the Claude Code plugin's bare
//! `semctl mcp` / `semctl hook` commands resolve to it. The plugin manifest
//! (fetched from git by the marketplace) can't carry a per-user absolute path,
//! so it invokes `semctl` by bare name — which only works if `semctl` is on the
//! PATH Claude Code spawns subprocesses with.
//!
//! We install the running binary into a chosen, deterministic user-scope
//! directory and add that directory to the persistent user PATH. User-scope (no
//! admin), cross-platform, idempotent, and best-effort: the caller reports the
//! outcome and never treats a PATH failure as fatal to the plugin wiring.
//! Platform arms dispatch on `cfg!(windows)` (not `#[cfg]`) so every branch
//! compiles and is checked on every host.
//!
//! Care is taken with the two genuinely dangerous operations:
//! - **Windows PATH** is edited via the *raw* registry (`DoNotExpandEnvironment-
//!   Names`, preserving `REG_EXPAND_SZ`), never `[Environment]::Set…`, which
//!   would expand `%VAR%` entries and rewrite the value as `REG_SZ`.
//! - **Unix profiles** get a single-quoted, runtime-guarded block, so a path
//!   with shell metacharacters can't be executed and re-sourcing can't prepend
//!   the directory twice.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// What ensuring the binary-on-PATH did, for the caller to report.
pub enum Outcome {
    /// The running binary is at the canonical `target` (`copied` = written just
    /// now; false = it was already the running binary). `on_path_now` = the
    /// directory is on the *current* process PATH (no restart needed).
    /// `shadowed` = a different `semctl` currently resolves first and may take
    /// precedence.
    Installed {
        target: PathBuf,
        copied: bool,
        on_path_now: bool,
        shadowed: Option<PathBuf>,
    },
    /// Installed to `target`, but persisting PATH failed — the user must add it.
    InstalledPathManual { target: PathBuf, error: String },
}

/// Install the running binary into a chosen, deterministic user-scope directory
/// and put that directory on PATH. Always targets the canonical location (rather
/// than trusting wherever the binary is run from), so `semctl` has a stable home
/// and re-running from a newer build updates it in place. Idempotent.
pub fn ensure_on_path() -> Result<Outcome> {
    let current = std::env::current_exe().context("locate the running semctl binary")?;
    let dir = bin_dir().context("determine a user bin directory")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let target = dir.join(exe_name());

    let copied = install_binary(&current, &target)?;

    let on_path_now = path_contains(&std::env::var_os("PATH").unwrap_or_default(), &dir);
    // A different semctl that resolves right now may still win by PATH order
    // after a restart — surface it so the user can resolve the ambiguity.
    let shadowed = which("semctl").filter(|p| !same_file(p, &target));

    // A failed PATH edit still leaves a usable copy on disk — report both.
    match add_dir_to_user_path(&dir) {
        Ok(_edited) => Ok(Outcome::Installed {
            target,
            copied,
            on_path_now,
            shadowed,
        }),
        Err(e) => Ok(Outcome::InstalledPathManual {
            target,
            error: format!("{e:#}"),
        }),
    }
}

/// The user-scope bin directory we install into (also used for a manual hint):
/// `%LOCALAPPDATA%\semctl\bin` on Windows, `~/.local/bin` on Unix.
pub fn bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("semctl").join("bin"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin"))
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "semctl.exe"
    } else {
        "semctl"
    }
}

/// Copy the running binary to `target`, symlink-safely and atomically. Returns
/// whether a copy happened (false = we're already the installed binary).
fn install_binary(current: &Path, target: &Path) -> Result<bool> {
    if same_file(current, target) {
        return Ok(false); // already running the installed copy
    }
    // Never write *through* a symlink (it could point at a dotfile); replace it.
    if let Ok(md) = std::fs::symlink_metadata(target)
        && md.file_type().is_symlink()
    {
        std::fs::remove_file(target)
            .with_context(|| format!("replace symlink at {}", target.display()))?;
    }
    // Copy to a sibling temp then atomically rename over the target, so an
    // interrupted copy can't truncate an existing working binary. `fs::copy`
    // carries the source's mode (the executable bit) on Unix.
    let tmp = target.with_file_name(format!(".semctl-install.{}.tmp", std::process::id()));
    std::fs::copy(current, &tmp).with_context(|| format!("copy semctl to {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("install semctl to {}", target.display()));
    }
    Ok(true)
}

/// Resolve `name` against PATH (checking `<dir>/name.exe` on Windows).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| resolve_in_dir(&dir, name))
}

fn resolve_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if cfg!(windows) {
        let exe = dir.join(format!("{name}.exe"));
        if exe.is_file() {
            return Some(exe);
        }
    }
    None
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn add_dir_to_user_path(dir: &Path) -> Result<bool> {
    if cfg!(windows) {
        windows_add_user_path(dir)
    } else {
        unix_add_user_path(dir)
    }
}

/// Persist `dir` onto the Windows user PATH via the *raw* registry, so
/// `%VAR%`-style entries and the `REG_EXPAND_SZ` value kind are preserved
/// (`[Environment]::SetEnvironmentVariable` would expand and downgrade them).
/// Prepends within the user PATH so this install wins over other user entries.
/// Returns whether an edit was made (false = already present).
fn windows_add_user_path(dir: &Path) -> Result<bool> {
    let d = dir.to_string_lossy().replace('\'', "''"); // escape for a single-quoted PS string
    let script = format!(
        "$ErrorActionPreference='Stop';\
         $d='{d}';\
         $k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true);\
         if($null -eq $k){{$k=[Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')}};\
         $cur=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames);\
         $kind=[Microsoft.Win32.RegistryValueKind]::ExpandString;\
         try{{$kind=$k.GetValueKind('Path')}}catch{{}};\
         $has=$false;\
         foreach($p in ($cur.Split(';')|Where-Object{{$_ -ne ''}})){{if($p.TrimEnd([char]92) -ieq $d.TrimEnd([char]92)){{$has=$true;break}}}};\
         if(-not $has){{\
           if($cur -eq ''){{$n=$d}}else{{$n=$d+';'+$cur}};\
           $k.SetValue('Path',$n,$kind);\
           Write-Output 'edited'\
         }}else{{Write-Output 'present'}};\
         $k.Close()"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("run powershell to update the user PATH")?;
    if !out.status.success() {
        anyhow::bail!(
            "powershell PATH update failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let edited = String::from_utf8_lossy(&out.stdout).contains("edited");
    if edited {
        broadcast_environment_change();
    }
    Ok(edited)
}

/// Tell Explorer and other interested top-level windows to reload the
/// persistent user environment. This cannot alter existing process environment
/// blocks, but applications launched after Explorer handles the message inherit
/// the updated PATH.
///
/// Best-effort: the registry edit is already durable, and one hung or
/// non-responsive window must not turn a successful PATH update into a reported
/// install failure.
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let environment: Vec<u16> = "Environment\0".encode_utf16().collect();

    // SAFETY: `environment` is a live, NUL-terminated UTF-16 buffer for the
    // duration of this synchronous call. HWND_BROADCAST and the remaining
    // values follow the documented WM_SETTINGCHANGE contract.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5_000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn broadcast_environment_change() {}

/// Persist `dir` onto PATH for POSIX shells by appending an idempotent,
/// injection-safe block to the user's shell profiles. Returns whether any file
/// was edited (false = every target already had the block).
fn unix_add_user_path(dir: &Path) -> Result<bool> {
    let s = dir
        .to_str()
        .context("bin dir path is not valid UTF-8 — add it to PATH manually")?;
    let quoted = shell_single_quote(s);
    let marker = "# added by `semctl install`";
    // Runtime-guarded (only prepends if not already present) so re-sourcing —
    // e.g. ~/.profile sourcing ~/.bashrc — can't prepend the directory twice;
    // the path is single-quoted so shell metacharacters can't be executed.
    let block = format!(
        "\n{marker}\n_semctl_dir={quoted}\ncase \":$PATH:\" in\n  \
         *\":$_semctl_dir:\"*) ;;\n  \
         *) export PATH=\"$_semctl_dir:$PATH\" ;;\nesac\nunset _semctl_dir\n"
    );
    let mut edited = false;
    for profile in unix_profiles() {
        if append_once(&profile, marker, &block)? {
            edited = true;
        }
    }
    Ok(edited)
}

/// The profile files to update, covering the login shell (`~/.profile`) and the
/// interactive rc of the user's `$SHELL` (created if absent — an rc-less zsh/bash
/// otherwise never sees the change), plus any other common rc that already
/// exists. The runtime guard makes writing to several files safe.
fn unix_profiles() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    add(home.join(".profile")); // POSIX login / sh

    let shell = std::env::var("SHELL").unwrap_or_default();
    match Path::new(&shell).file_name().and_then(OsStr::to_str) {
        Some("zsh") => {
            add(home.join(".zshrc")); // interactive zsh (created if missing)
            add(home.join(".zprofile")); // login zsh
        }
        Some("bash") => {
            add(home.join(".bashrc")); // interactive bash (created if missing)
            let bp = home.join(".bash_profile"); // login bash reads this instead of ~/.profile
            if bp.exists() {
                add(bp);
            }
        }
        _ => {}
    }
    // Cover a multi-shell user's other existing rc files too.
    for rc in [".bashrc", ".zshrc"] {
        let p = home.join(rc);
        if p.exists() {
            add(p);
        }
    }
    out
}

/// Append `block` to `file` unless `marker` is already present (idempotent).
/// Marker search is byte-based so a non-UTF-8 profile isn't treated as empty
/// (which would append a duplicate). Creates the file if missing.
fn append_once(file: &Path, marker: &str, block: &str) -> Result<bool> {
    use std::io::Write;
    if std::fs::read(file).is_ok_and(|bytes| contains_bytes(&bytes, marker.as_bytes())) {
        return Ok(false);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .with_context(|| format!("open {}", file.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("write {}", file.display()))?;
    Ok(true)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// POSIX single-quote a string so it is inert in a shell (`$`, backticks,
/// `"`, spaces, `;` all literal): wrap in `'…'`, and turn each embedded `'`
/// into `'\''` (close, escaped quote, reopen).
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Whether `dir` is one of the entries in a `PATH`-style variable.
fn path_contains(path_os: &OsStr, dir: &Path) -> bool {
    std::env::split_paths(path_os).any(|p| p == dir || same_file(&p, dir))
}

/// Result of removing the installed binary, for the caller to report.
pub enum BinaryRemoval {
    /// The canonical binary was deleted (`target`); it was not the running one.
    Removed { target: PathBuf },
    /// The canonical binary was the *running* one and was self-deleted.
    RemovedRunning { target: PathBuf },
    /// No binary at the canonical location.
    NotFound,
}

/// Delete the binary at the canonical install location. If that binary is the
/// currently running executable, self-delete it (handles the Windows running-exe
/// lock via the `self-replace` crate). Best-effort; a missing binary is `NotFound`.
pub fn remove_installed_binary() -> Result<BinaryRemoval> {
    let Some(dir) = bin_dir() else {
        return Ok(BinaryRemoval::NotFound);
    };
    let target = dir.join(exe_name());
    if !target.exists() {
        return Ok(BinaryRemoval::NotFound);
    }
    let running_is_target = std::env::current_exe()
        .ok()
        .is_some_and(|c| same_file(&c, &target));
    if running_is_target {
        self_replace::self_delete().context("remove the running semctl binary")?;
        Ok(BinaryRemoval::RemovedRunning { target })
    } else {
        std::fs::remove_file(&target).with_context(|| format!("remove {}", target.display()))?;
        Ok(BinaryRemoval::Removed { target })
    }
}

/// Undo the PATH edit [`ensure_on_path`] made — remove our bin dir from the user
/// PATH (the marked shell-profile block on Unix, the registry entry on Windows).
/// Best-effort and idempotent; returns whether anything was edited.
pub fn remove_dir_from_user_path() -> Result<bool> {
    let Some(dir) = bin_dir() else {
        return Ok(false);
    };
    if cfg!(windows) {
        windows_remove_user_path(&dir)
    } else {
        unix_remove_user_path(&dir)
    }
}

/// Remove our marked block from each shell profile that has it.
fn unix_remove_user_path(_dir: &Path) -> Result<bool> {
    let marker = "# added by `semctl install`";
    let mut edited = false;
    for profile in unix_profiles() {
        if remove_marked_block(&profile, marker, "unset _semctl_dir")? {
            edited = true;
        }
    }
    Ok(edited)
}

/// Strip the PATH block [`unix_add_user_path`] appended — the `marker` line
/// through its closing line (the one starting with `end_prefix`, e.g.
/// `unset _semctl_dir`), plus one blank line directly before the marker.
/// Non-existent / non-UTF-8 / marker-absent → `Ok(false)`.
fn remove_marked_block(file: &Path, marker: &str, end_prefix: &str) -> Result<bool> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Ok(false);
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == marker) else {
        return Ok(false);
    };
    let Some(end_rel) = lines[start..]
        .iter()
        .position(|l| l.trim_start().starts_with(end_prefix))
    else {
        return Ok(false); // marker without our block shape — leave it alone
    };
    let end = start + end_rel;
    // Drop one blank line immediately before the marker (the block was appended
    // with a leading newline).
    let from = if start > 0 && lines[start - 1].trim().is_empty() {
        start - 1
    } else {
        start
    };
    let kept: Vec<&str> = lines[..from]
        .iter()
        .chain(lines[end + 1..].iter())
        .copied()
        .collect();
    let mut out = kept.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(file, out).with_context(|| format!("write {}", file.display()))?;
    Ok(true)
}

/// Remove `dir` from the Windows user PATH via the raw registry (preserving the
/// `%VAR%` entries and the value kind), matching how [`windows_add_user_path`]
/// added it. Returns whether an edit was made.
fn windows_remove_user_path(dir: &Path) -> Result<bool> {
    let d = dir.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop';\
         $d='{d}';\
         $k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true);\
         if($null -eq $k){{Write-Output 'present'}}else{{\
           $cur=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames);\
           $kind=[Microsoft.Win32.RegistryValueKind]::ExpandString;\
           try{{$kind=$k.GetValueKind('Path')}}catch{{}};\
           $parts=@($cur.Split(';')|Where-Object{{$_ -ne ''}});\
           $keep=@($parts|Where-Object{{$_.TrimEnd([char]92) -ine $d.TrimEnd([char]92)}});\
           if($keep.Count -ne $parts.Count){{\
             $k.SetValue('Path',[string]::Join(';',$keep),$kind);\
             Write-Output 'edited'\
           }}else{{Write-Output 'present'}};\
           $k.Close()\
         }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("run powershell to update the user PATH")?;
    if !out.status.success() {
        anyhow::bail!(
            "powershell PATH update failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let edited = String::from_utf8_lossy(&out.stdout).contains("edited");
    if edited {
        broadcast_environment_change();
    }
    Ok(edited)
}

/// The old `semctx` selfpath install dir: `%LOCALAPPDATA%\semctx\bin` on Windows,
/// `~/.local/bin` on Unix (same dir as semctl, but the binary is named `semctx`).
fn legacy_bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("semctx").join("bin"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin"))
    }
}

fn legacy_exe_name() -> &'static str {
    if cfg!(windows) {
        "semctx.exe"
    } else {
        "semctx"
    }
}

/// Path to the old selfpath-installed `semctx` binary, if it exists.
pub fn legacy_binary() -> Option<PathBuf> {
    legacy_bin_dir()
        .map(|d| d.join(legacy_exe_name()))
        .filter(|p| p.exists())
}

/// A `cargo install`-placed old `semctx` binary in `$CARGO_HOME/bin`, if present
/// — cargo manages it, so we only point the user at `cargo uninstall`.
pub fn legacy_cargo_binary() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
        .map(|c| c.join("bin").join(legacy_exe_name()))
        .filter(|p| p.exists())
}

/// Whether a legacy PATH block left by the old `semctx install` is present in any
/// shell profile (Unix). Always false on Windows (the check there would spawn a
/// process; the binary/config presence gates migration instead).
pub fn legacy_path_block_present() -> bool {
    if cfg!(windows) {
        return false;
    }
    let marker = "# added by `semctx install`";
    unix_profiles()
        .iter()
        .any(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains(marker)))
}

/// Remove the old selfpath-installed `semctx` binary. Returns the removed path.
pub fn remove_legacy_binary() -> Result<Option<PathBuf>> {
    let Some(p) = legacy_binary() else {
        return Ok(None);
    };
    std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
    // Tidy the now-likely-empty old Windows bin dir (`~/.local/bin` on Unix is
    // shared with semctl, so never remove it there).
    if cfg!(windows)
        && let Some(dir) = legacy_bin_dir()
    {
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(Some(p))
}

/// Undo the old `semctx install` PATH edit: strip its marked block from the shell
/// profiles (Unix) or remove its bin dir from the Windows user PATH. Returns
/// whether anything was edited.
pub fn remove_legacy_from_user_path() -> Result<bool> {
    if cfg!(windows) {
        match legacy_bin_dir() {
            Some(dir) => windows_remove_user_path(&dir),
            None => Ok(false),
        }
    } else {
        let marker = "# added by `semctx install`";
        let mut edited = false;
        for profile in unix_profiles() {
            if remove_marked_block(&profile, marker, "unset _semctx_dir")? {
                edited = true;
            }
        }
        Ok(edited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_matches_platform() {
        assert_eq!(
            exe_name(),
            if cfg!(windows) {
                "semctl.exe"
            } else {
                "semctl"
            }
        );
    }

    #[test]
    fn bin_dir_is_platform_appropriate() {
        let d = bin_dir().expect("a home/local dir in the test env");
        if cfg!(windows) {
            assert!(d.ends_with("bin") && d.to_string_lossy().contains("semctl"));
        } else {
            assert!(d.ends_with(".local/bin"), "{}", d.display());
        }
    }

    #[test]
    fn path_contains_detects_membership() {
        let dir = std::env::temp_dir().join("semctl-pathtest-xyz");
        let other = std::env::temp_dir().join("semctl-other");
        let with = std::env::join_paths([other.clone(), dir.clone()]).unwrap();
        let without = std::env::join_paths([other]).unwrap();
        assert!(path_contains(with.as_os_str(), &dir));
        assert!(!path_contains(without.as_os_str(), &dir));
    }

    #[test]
    fn shell_single_quote_neutralizes_injection() {
        assert_eq!(
            shell_single_quote("/home/u/.local/bin"),
            "'/home/u/.local/bin'"
        );
        // An embedded single quote is closed, escaped, and reopened.
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        // Command substitution and vars are literal inside single quotes.
        assert_eq!(
            shell_single_quote("/tmp/a\"$(touch pwn)\""),
            "'/tmp/a\"$(touch pwn)\"'"
        );
    }

    #[test]
    fn append_once_is_idempotent_and_byte_aware() {
        let dir = std::env::temp_dir().join(format!("semctl-appendonce-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("profile");
        let marker = "# added by test";
        let block = format!("\n{marker}\nexport PATH=\"/x:$PATH\"\n");
        assert!(
            append_once(&file, marker, &block).unwrap(),
            "first append writes"
        );
        assert!(
            !append_once(&file, marker, &block).unwrap(),
            "second append is a no-op"
        );
        assert_eq!(
            std::fs::read_to_string(&file)
                .unwrap()
                .matches(marker)
                .count(),
            1,
            "marker appears exactly once"
        );

        // A marker inside non-UTF-8 content is still detected (no duplicate).
        let nonutf8 = dir.join("nonutf8");
        let mut bytes = marker.as_bytes().to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
        std::fs::write(&nonutf8, &bytes).unwrap();
        assert!(
            !append_once(&nonutf8, marker, &block).unwrap(),
            "marker found despite non-UTF-8"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_marked_block_strips_our_block_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("semctl-rmblock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("profile");
        let marker = "# added by `semctl install`";
        let block = format!(
            "\n{marker}\n_semctl_dir='/home/u/.local/bin'\ncase \":$PATH:\" in\n  \
             *\":$_semctl_dir:\"*) ;;\n  \
             *) export PATH=\"$_semctl_dir:$PATH\" ;;\nesac\nunset _semctl_dir\n"
        );
        std::fs::write(
            &file,
            format!("export EDITOR=vim\n{block}alias ll='ls -la'\n"),
        )
        .unwrap();

        assert!(
            remove_marked_block(&file, marker, "unset _semctl_dir").unwrap(),
            "block removed"
        );
        let left = std::fs::read_to_string(&file).unwrap();
        assert!(!left.contains(marker), "marker gone: {left:?}");
        assert!(!left.contains("_semctl_dir"), "block body gone: {left:?}");
        assert!(left.contains("EDITOR=vim"), "content before kept");
        assert!(left.contains("alias ll"), "content after kept");

        // Idempotent: a second removal is a no-op.
        assert!(
            !remove_marked_block(&file, marker, "unset _semctl_dir").unwrap(),
            "second removal is a no-op"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

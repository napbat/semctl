//! Git-subprocess layer backing codebase resolution: read the working copy's
//! `origin` remote, HEAD revision/branch, and dirty state via `git -C dir`.

use std::path::Path;

/// The `origin` remote URL of the git repo at `dir`, if any.
pub(super) async fn git_remote(dir: &Path) -> Option<String> {
    git_capture(dir, &["remote", "get-url", "origin"]).await
}

/// Run `git -C dir <args>` and return its trimmed stdout, or `None` if git
/// fails (not a repo, detached, etc.). Uses `tokio::process` with
/// `kill_on_drop` so a hung git is genuinely cancellable: a caller that wraps
/// this in a `tokio::time::timeout` (the hook's `connect`) drops the future on
/// timeout, which kills the child — a `spawn_blocking` git could not be
/// preempted and would delay process exit (the runtime joins blocking tasks).
pub(super) async fn git_capture(dir: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Whether the working tree has uncommitted changes (`git status --porcelain`
/// non-empty). Treated as clean when git isn't available.
pub(super) async fn git_is_dirty(dir: &Path) -> bool {
    git_capture(dir, &["status", "--porcelain"])
        .await
        .is_some_and(|s| !s.is_empty())
}

/// Reduce the common remote-URL forms to a comparable `host/path` shape:
/// `https://github.com/org/repo.git`, `git@github.com:org/repo.git`, and
/// `ssh://git@github.com/org/repo` all become `github.com/org/repo`.
pub(super) fn normalize_remote(url: &str) -> String {
    let mut s = url.trim();
    for scheme in ["ssh://", "https://", "http://", "git://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest;
            break;
        }
    }
    // Drop any `user@` (e.g. scp-style `git@host:org/repo`).
    let mut s = s.rsplit('@').next().unwrap_or(s).to_string();
    // scp-style separates host from path with `:`; normalize to `/`.
    if let Some(colon) = s.find(':').filter(|&c| !s[..c].contains('/')) {
        s.replace_range(colon..=colon, "/");
    }
    s = s.trim_end_matches('/').to_string();
    s = s.strip_suffix(".git").unwrap_or(&s).to_string();
    s.to_ascii_lowercase()
}

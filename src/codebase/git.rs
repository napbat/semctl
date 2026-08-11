//! Git-subprocess layer for checkout metadata and root discovery: read the
//! working copy's `origin` remote, HEAD revision/branch, and dirty state via
//! `git -C dir`.

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

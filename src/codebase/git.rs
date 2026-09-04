//! Git-subprocess layer for checkout metadata and root discovery: read the
//! working copy's `origin` remote, HEAD revision/branch, and dirty state via
//! `git -C dir`.

use std::path::Path;

#[cfg(windows)]
use std::io::{Read, Seek};
#[cfg(windows)]
use std::process::{Output, Stdio};

use tokio::process::Command;

/// The `origin` remote URL of the git repo at `dir`, if any.
pub(super) async fn git_remote(dir: &Path) -> Option<String> {
    git_capture(dir, &["remote", "get-url", "origin"]).await
}

/// Run `git -C dir <args>` and return its trimmed stdout, or `None` if git
/// fails (not a repo, detached, etc.). Uses `tokio::process` with
/// `kill_on_drop` so a caller timeout cancels the direct child rather than
/// leaving a blocking task that delays runtime shutdown. On Windows,
/// [`git_output`] also avoids the anonymous-pipe startup hang that can strand
/// Git's real grandchild beyond that direct-child cleanup.
pub(super) async fn git_capture(dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args).kill_on_drop(true);
    let out = git_output(&mut command).await.ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Capture a Git probe without anonymous output pipes on Windows.
///
/// Git for Windows' MSYS tty detection inspects all three standard handles in
/// the real `mingw64/bin/git.exe` grandchild. An anonymous pipe can wedge that
/// inspection before Git reaches `main`, leaving both the `cmd/git.exe` shim
/// and its grandchild alive forever. MCP stdio makes that failure reproducible.
/// A disk-backed stdout handle plus null stdin/stderr avoids the MSYS pipe path;
/// the temporary file is deleted when both parent and child handles close.
#[cfg(windows)]
async fn git_output(command: &mut Command) -> std::io::Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let child_stdout = stdout.try_clone()?;
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::null())
        .status()
        .await?;
    stdout.rewind()?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    Ok(Output {
        status,
        stdout: bytes,
        stderr: Vec::new(),
    })
}

#[cfg(not(windows))]
async fn git_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    command.output().await
}

/// Whether the working tree has uncommitted changes (`git status --porcelain`
/// non-empty). Treated as clean when git isn't available.
pub(super) async fn git_is_dirty(dir: &Path) -> bool {
    git_capture(dir, &["status", "--porcelain"])
        .await
        .is_some_and(|s| !s.is_empty())
}

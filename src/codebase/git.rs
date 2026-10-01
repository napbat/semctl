//! Git-subprocess layer for checkout metadata and root discovery: read the
//! working copy's `origin` remote, HEAD revision/branch, and dirty state via
//! `git -C dir`.

use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::io::{Read, Seek};
#[cfg(windows)]
use std::process::{Output, Stdio};

use tokio::process::Command;

/// The `origin` remote URL of the git repo at `dir`, if any.
pub(super) async fn git_remote(dir: &Path) -> Option<String> {
    let remote = git_capture(dir, &["remote", "get-url", "origin"]).await?;
    sanitized_remote(&remote)
}

/// Remote metadata excludes passwords and HTTP credentials. SSH user names
/// remain part of the identity because they can select different repositories.
fn sanitized_remote(remote: &str) -> Option<String> {
    if remote.contains("://") {
        let mut url = reqwest::Url::parse(remote).ok()?;
        if url.scheme() != "ssh" && !url.username().is_empty() {
            url.set_username("").ok()?;
        }
        if url.password().is_some() {
            url.set_password(None).ok()?;
        }
        url.set_query(None);
        url.set_fragment(None);
        return Some(url.to_string());
    }
    // A scp-style path can be relative to the SSH user's home directory.
    // Removing that user would collapse distinct repository identities.
    Some(remote.to_string())
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

/// Git emits a native absolute path followed by one line terminator. Preserve
/// path whitespace and native bytes instead of applying metadata text trimming.
pub(super) async fn git_working_copy_root(dir: &Path) -> Option<PathBuf> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .kill_on_drop(true);
    let output = git_output(&mut command).await.ok()?;
    if !output.status.success() {
        return None;
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        #[cfg(windows)]
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(String::from_utf8(bytes).ok()?);
    path.is_absolute().then_some(path)
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

#[cfg(test)]
mod tests {
    use super::sanitized_remote;

    #[test]
    fn remote_metadata_removes_https_credentials_and_parameters() {
        assert_eq!(
            sanitized_remote(
                "https://oauth2:test-token@example.com/org/repo.git?token=test-query#test-fragment"
            ),
            Some("https://example.com/org/repo.git".into())
        );
        assert_eq!(
            sanitized_remote("https://test-token@example.com/org/repo.git"),
            Some("https://example.com/org/repo.git".into())
        );
    }

    #[test]
    fn remote_metadata_removes_ssh_passwords_and_preserves_user_names() {
        assert_eq!(
            sanitized_remote("ssh://git:test-password@example.com:2222/org/repo.git"),
            Some("ssh://git@example.com:2222/org/repo.git".into())
        );
        assert_eq!(
            sanitized_remote("git@example.com:org/repo.git"),
            Some("git@example.com:org/repo.git".into())
        );
    }

    #[test]
    fn home_relative_ssh_repositories_keep_distinct_user_identities() {
        for (alice, bob) in [
            ("alice@example.com:repo.git", "bob@example.com:repo.git"),
            (
                "ssh://alice@example.com/~/repo.git",
                "ssh://bob@example.com/~/repo.git",
            ),
        ] {
            assert_eq!(sanitized_remote(alice).as_deref(), Some(alice));
            assert_eq!(sanitized_remote(bob).as_deref(), Some(bob));
            assert_ne!(sanitized_remote(alice), sanitized_remote(bob));
        }
    }

    #[test]
    fn ordinary_remote_paths_remain_usable() {
        for remote in [
            "https://example.com/org/repo.git",
            "file:///srv/repo.git",
            "/srv/repo.git",
            "../repo.git",
        ] {
            assert_eq!(sanitized_remote(remote).as_deref(), Some(remote));
        }
    }

    #[test]
    fn malformed_url_credentials_are_not_returned() {
        assert_eq!(
            sanitized_remote("https://user:test-token@[invalid/repo.git"),
            None
        );
    }
}

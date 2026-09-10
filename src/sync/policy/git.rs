//! Checked Git configuration and worktree policy discovery.

use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::{Sources, State};
use crate::sync::blocking::Cancellation;

pub(super) struct Configuration {
    directory: PathBuf,
    fingerprint: Option<blake3::Hash>,
    pub(super) excludes: Option<PathBuf>,
}

impl Configuration {
    pub(super) fn load(
        directory: &Path,
        sources: &mut Sources,
        cancellation: &Cancellation,
    ) -> Result<Self> {
        // Git silently skips an unreadable global config. Check the paths that
        // select global configuration before asking Git to resolve includes.
        let candidates = global_config_paths()
            .into_iter()
            .map(|path| absolute(directory, &path));
        let mut configured = false;
        for path in candidates {
            configured |= sources.read(&path)?.is_some();
        }
        let Some((listed, excludes)) = query(directory, cancellation)? else {
            ensure!(
                !configured,
                "Git is required to validate configured source exclusions"
            );
            ensure!(
                !directory
                    .ancestors()
                    .any(|path| path.join(".git").try_exists().unwrap_or(true)),
                "Git is required to validate checkout source policy"
            );
            return Ok(Self {
                directory: directory.to_path_buf(),
                fingerprint: None,
                excludes: default_excludes(),
            });
        };
        // --null keeps origin paths separate from config values. Values can
        // contain credentials, so no command output is included in diagnostics.
        let fields: Vec<_> = listed.split(|byte| *byte == 0).collect();
        for pair in fields[..fields.len().saturating_sub(1)].chunks(2) {
            ensure!(
                pair.len() == 2,
                "Git returned an invalid configuration listing"
            );
            if let Some(origin) = pair[0].strip_prefix(b"file:") {
                let path = absolute(directory, &path_from_bytes(origin)?);
                sources.read(&path)?;
            }
        }
        let fingerprint = Some(fingerprint(&listed, excludes.as_deref()));
        let excludes = match excludes {
            // Git treats an explicitly empty value as disabled. Only an absent
            // setting selects the XDG default ignore file.
            Some(path) if path.as_os_str().is_empty() => None,
            Some(path) => Some(absolute(directory, &path)),
            None => default_excludes(),
        };
        Ok(Self {
            directory: directory.to_path_buf(),
            fingerprint,
            excludes,
        })
    }

    pub(super) fn verify(&self, cancellation: &Cancellation) -> Result<()> {
        let current = query(&self.directory, cancellation)?
            .map(|(listed, excludes)| fingerprint(&listed, excludes.as_deref()));
        ensure!(
            current == self.fingerprint,
            "Git source policy changed during sync; retry sync"
        );
        Ok(())
    }
}

fn fingerprint(listed: &[u8], excludes: Option<&Path>) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    hash.update(listed);
    hash.update(b"\0");
    if let Some(path) = excludes {
        hash.update(path.as_os_str().as_encoded_bytes());
    }
    hash.finalize()
}

fn query(
    directory: &Path,
    cancellation: &Cancellation,
) -> Result<Option<(Vec<u8>, Option<PathBuf>)>> {
    let Some((status, listed)) = run_git(
        directory,
        &["config", "--null", "--show-origin", "--list", "--includes"],
        cancellation,
    )?
    else {
        return Ok(None);
    };
    ensure!(
        status == 0,
        "Git could not read source policy configuration (exit {status})"
    );
    let (_, excludes) = run_git(
        directory,
        &["config", "--null", "--path", "--get", "core.excludesfile"],
        cancellation,
    )?
    .context("Git became unavailable while reading source policy")?;
    let excludes = if excludes.is_empty() {
        None
    } else {
        Some(path_from_bytes(
            excludes
                .strip_suffix(&[0])
                .context("invalid Git excludes path response")?,
        )?)
    };
    Ok(Some((listed, excludes)))
}

fn global_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if std::env::var_os("GIT_CONFIG_NOSYSTEM").is_none() {
        if let Some(path) = std::env::var_os("GIT_CONFIG_SYSTEM") {
            paths.push(path.into());
        } else if cfg!(unix) {
            paths.push(PathBuf::from("/etc/gitconfig"));
        }
    }
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        paths.push(path.into());
    } else {
        if let Some(home) = git_home_dir() {
            paths.push(home.join(".gitconfig"));
        }
        if let Some(base) = xdg_config_dir() {
            paths.push(base.join("git/config"));
        }
    }
    paths
}

fn xdg_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| git_home_dir().map(|home| home.join(".config")))
}

fn git_home_dir() -> Option<PathBuf> {
    // Git honors HOME on Windows too, where the native profile directory can
    // differ from a portable Git installation's configured home directory.
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn default_excludes() -> Option<PathBuf> {
    xdg_config_dir().map(|base| base.join("git/ignore"))
}

/// Resolve .git files and commondir files used by linked worktrees. Track both
/// indirections so retargeting either one invalidates the policy snapshot.
pub(super) fn common_directory(directory: &Path, sources: &mut Sources) -> Result<Option<PathBuf>> {
    let marker = directory.join(".git");
    let (state, bytes) = sources.observe(&marker)?;
    let git_dir = match state {
        State::Missing => return Ok(None),
        State::Directory(target) => target,
        State::File(_) => {
            let bytes = bytes.context("Git directory marker has no content")?;
            let value = bytes
                .strip_prefix(b"gitdir: ")
                .context("invalid Git directory marker")?;
            let value = without_line_ending(value);
            ensure!(!value.is_empty(), "empty Git directory marker");
            absolute(directory, &path_from_bytes(value)?)
        }
    };
    let common = match sources.read(&git_dir.join("commondir"))? {
        Some(bytes) => {
            let value = without_line_ending(&bytes);
            ensure!(!value.is_empty(), "empty Git common directory");
            absolute(&git_dir, &path_from_bytes(value)?)
        }
        None => git_dir.clone(),
    };
    let common = std::fs::canonicalize(common).context("resolve Git common directory")?;
    sources.read(&common.join("config"))?;
    sources.read(&git_dir.join("config.worktree"))?;
    Ok(Some(common))
}

fn without_line_ending(bytes: &[u8]) -> &[u8] {
    bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes)
}

fn absolute(directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        directory.join(path)
    }
}

#[allow(clippy::unnecessary_wraps)] // Unix paths are arbitrary bytes; other platforms can reject encoding.
fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(
            std::str::from_utf8(bytes).context("Git policy path is not UTF-8")?,
        ))
    }
}

struct Capture {
    path: PathBuf,
    file: File,
}

impl Capture {
    fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            ".semctl-{:012x}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = crate::config::create_private_new(&path)?;
        Ok(Self { path, file })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // This private scratch file contains no live state. A failed cleanup
        // only leaves an owner-readable temporary file.
        let _ = std::fs::remove_file(&self.path);
    }
}

struct GitChild(Child);

impl Drop for GitChild {
    fn drop(&mut self) {
        // Killing an already-exited child is harmless. Always reap the child,
        // including when cancellation or a configuration timeout returns early.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_git(
    directory: &Path,
    args: &[&str],
    cancellation: &Cancellation,
) -> Result<Option<(i32, Vec<u8>)>> {
    cancellation.check()?;
    let capture = Capture::new()?;
    let mut command = Command::new(OsStr::new("git"));
    command
        .arg("-C")
        .arg(directory)
        .args(args)
        .stdin(Stdio::null())
        // A disk handle avoids the Git for Windows anonymous-pipe startup hang.
        .stdout(capture.file.try_clone()?)
        .stderr(Stdio::null());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("start Git source policy query"),
    };
    let mut child = GitChild(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        cancellation.check()?;
        if let Some(status) = child
            .0
            .try_wait()
            .context("wait for Git source policy query")?
        {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "Git source policy query timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let status = status
        .code()
        .context("Git source policy query was interrupted")?;
    ensure!(
        status == 0 || status == 1,
        "Git could not read source policy configuration (exit {status})"
    );
    let output = File::open(&capture.path).context("read Git source policy response")?;
    let mut bytes = Vec::new();
    output.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Git source policy configuration exceeds 16 MiB"
    );
    Ok(Some((status, bytes)))
}

//! Cross-process locking and atomic publication of local configuration files.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Create a new owner-only file without following or replacing an existing path.
pub(crate) fn create_private_new(path: &Path) -> Result<File> {
    private_options()
        .create_new(true)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))
}

/// Open a persistent lock file. Never remove this file while writers can run:
/// every writer must lock the same inode, including after a process exits.
pub(crate) fn open_lock(path: &Path) -> Result<File> {
    let parent = path.parent().context("lock path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    private_options()
        .read(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))
}

/// Hold a cross-process lock until the returned file is dropped.
/// Call this on a blocking thread when contention is possible.
pub(crate) fn lock_file(path: &Path) -> Result<File> {
    let file = open_lock(path)?;
    file.lock()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}

/// Publish complete owner-only bytes with a same-directory atomic rename.
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let name = path
        .file_name()
        .context("configuration path has no filename")?;
    let (temporary, mut file) = loop {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{}.{}.{sequence}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        match private_options().create_new(true).open(&temporary) {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("stage {}", path.display()));
            }
        }
    };
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        // The live file is unchanged. An owner-only temporary file is safe to
        // retain if this best-effort cleanup also fails.
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("publish {}", path.display()))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_publication_replaces_complete_contents_without_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        atomic_write_private(&path, b"first").unwrap();
        atomic_write_private(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_publication_preserves_existing_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("retained"), b"before").unwrap();
        assert!(atomic_write_private(&path, b"after").is_err());
        assert_eq!(fs::read(path.join("retained")).unwrap(), b"before");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn cross_process_lock_child() {
        let Some(path) = std::env::var_os("SEMCTL_TEST_LOCK_PATH") else {
            return;
        };
        let file = open_lock(Path::new(&path)).unwrap();
        assert!(matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)));
    }

    #[test]
    fn file_lock_excludes_another_process() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.lock");
        let _lock = lock_file(&path).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::persistence::tests::cross_process_lock_child",
            ])
            .env("SEMCTL_TEST_LOCK_PATH", &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

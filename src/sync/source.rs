//! Checked source-file reads shared by scanning and upload preparation.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::blocking::Cancellation;
use super::walker::MAX_FILE_BYTES;

/// Resolve a manifest path without allowing an absolute path, a parent
/// component, or a symlink that leaves the original checkout root.
pub(super) fn checked_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid checkout path: {relative}"
    );
    let canonical = std::fs::canonicalize(root.join(path))
        .with_context(|| format!("resolve checkout file {relative}"))?;
    ensure!(
        canonical.starts_with(root),
        "checkout path escapes its root: {relative}"
    );
    ensure!(
        canonical.is_file(),
        "checkout path is not a file: {relative}"
    );
    Ok(canonical)
}

/// Read a bounded file. Non-UTF-8 content is a deterministic exclusion.
/// All filesystem failures abort the scan or upload preparation.
pub(super) fn read(
    root: &Path,
    relative: &str,
    cancellation: &Cancellation,
) -> Result<Option<String>> {
    Ok(String::from_utf8(read_bytes(root, relative, cancellation)?).ok())
}

/// Read the exact bytes used to validate cached content decisions.
pub(super) fn read_bytes(
    root: &Path,
    relative: &str,
    cancellation: &Cancellation,
) -> Result<Vec<u8>> {
    cancellation.check()?;
    let path = checked_path(root, relative)?;
    let file = File::open(&path).with_context(|| format!("open checkout file {relative}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read checkout file {relative}"))?;
    cancellation.check()?;
    ensure!(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_FILE_BYTES,
        "{relative} exceeded the file size limit during sync"
    );
    ensure!(
        checked_path(root, relative)? == path,
        "{relative} changed its target during sync"
    );
    Ok(bytes)
}

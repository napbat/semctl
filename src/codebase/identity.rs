//! Stable, opaque identity for one local checkout, and the project slug it
//! belongs under.
//!
//! The server must distinguish two working copies even when their folder names
//! and Git remotes are identical: each submits a complete desired-state
//! manifest, so collapsing them onto one codebase makes them delete one
//! another's files. The raw path never leaves this machine. We hash it together
//! with the installation id and use that opaque value both for sync arbitration
//! and for a deterministic, tenant-unique local codebase slug.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

const MAX_SLUG_LEN: usize = 120;

/// Opaque stable identity for one local checkout. Different semctl processes in
/// the same checkout intentionally produce the same value; another clone gets a
/// different value even when it points at the same Git remote.
pub(crate) fn source_id(dir: &Path) -> Result<String> {
    let dir = std::fs::canonicalize(dir)
        .with_context(|| format!("resolve checkout identity {}", dir.display()))?;
    ensure!(dir.is_dir(), "checkout identity requires a directory");
    let installation_id = crate::config::installation_id()?;
    Ok(source_id_for(&installation_id, &dir))
}

/// The label a codebase is shown under, from the checkout's folder name.
///
/// Only for a server that derives projects itself. There the slug carries no
/// identity and needs no uniqueness, so it can be the readable thing a person
/// expects to see — `semctx`, not `semctx-4f1c9e...` — and two of them
/// colliding costs nothing.
pub(super) fn label(display_name: &str) -> String {
    let mut base = slugify(display_name);
    base.truncate(MAX_SLUG_LEN);

    let trimmed = base.trim_matches('-');

    if trimmed.is_empty() {
        "codebase".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A deterministic slug for this checkout. `display_name` remains the friendly
/// folder name; the slug carries the opaque checkout identity so two folders
/// with that same name can coexist in one tenant. Using the full digest makes a
/// collision a cryptographic event rather than a naming race.
pub(super) fn slug(display_name: &str, source_id: &str) -> String {
    let suffix_len = source_id.len().min(MAX_SLUG_LEN.saturating_sub(2));
    let suffix = &source_id[..suffix_len];
    let max_base_len = MAX_SLUG_LEN.saturating_sub(suffix.len() + 1);
    let mut base = slugify(display_name);
    base.truncate(max_base_len);
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "codebase" } else { base };
    format!("{base}-{suffix}")
}

fn source_id_for(installation_id: &str, dir: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    if let Some(path) = legacy_identity_path(dir) {
        // Preserve deployed identities only on the subset whose old encoding
        // is lossless. Ambiguous paths must never claim an old shared identity.
        hasher.update(b"semctl-sync-source-v1\0");
        hasher.update(installation_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(path.as_bytes());
    } else {
        hasher.update(b"semctl-sync-source-v2\0");
        hasher.update(installation_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(path_key(dir).as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn legacy_identity_path(dir: &Path) -> Option<String> {
    let path = dir.to_str()?;
    if legacy_key_is_ambiguous(path) {
        return None;
    }
    #[cfg(windows)]
    {
        Some(path.replace('\\', "/").to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        Some(path.to_string())
    }
}

fn legacy_key_is_ambiguous(path: &str) -> bool {
    // Lossy UTF-8 conversion can manufacture U+FFFD in a cache key for a
    // different native path. Such keys cannot prove prior checkout ownership.
    path.contains('\u{fffd}') || (cfg!(unix) && path.contains('\\'))
}

/// Reversible cache key. Unambiguous UTF-8 paths retain their existing wire shape.
/// The reserved prefix cannot be an absolute local path on supported platforms.
pub(crate) fn path_key(path: &Path) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    if let Some(path) = path.to_str()
        && !legacy_key_is_ambiguous(path)
    {
        return path.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        format!(
            "semctl-path:unix:{}",
            URL_SAFE_NO_PAD.encode(path.as_os_str().as_bytes())
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes: Vec<u8> = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        format!("semctl-path:windows:{}", URL_SAFE_NO_PAD.encode(bytes))
    }
}

/// Decode a cache key without replacing invalid native path characters.
pub(crate) fn path_from_key(key: &str) -> Option<PathBuf> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    if !key.starts_with("semctl-path:") {
        // A legacy Unix backslash key may name a shared single-manifest
        // codebase created through the old source-id collision. It cannot
        // authorize recovery into that codebase under the new identity.
        if legacy_key_is_ambiguous(key) {
            return None;
        }
        return Some(PathBuf::from(key));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bytes = URL_SAFE_NO_PAD
            .decode(key.strip_prefix("semctl-path:unix:")?)
            .ok()?;
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        path.is_absolute().then_some(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let bytes = URL_SAFE_NO_PAD
            .decode(key.strip_prefix("semctl-path:windows:")?)
            .ok()?;
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let path = PathBuf::from(std::ffi::OsString::from_wide(&units));
        path.is_absolute().then_some(path)
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "codebase".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[cfg(unix)]
    use super::path_key;
    use super::{MAX_SLUG_LEN, label, path_from_key, slug, source_id_for};

    #[test]
    fn source_identity_is_stable_for_the_same_checkout() {
        let first = source_id_for("install-a", Path::new("/work/repo"));
        let second = source_id_for("install-a", Path::new("/work/repo"));
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn same_named_checkouts_receive_distinct_catalog_slugs() {
        let first = source_id_for("install-a", Path::new("/work/first/repo"));
        let second = source_id_for("install-a", Path::new("/work/second/repo"));
        assert_ne!(first, second);
        assert_ne!(slug("repo", &first), slug("repo", &second));
    }

    #[test]
    fn source_identity_separates_installations() {
        assert_ne!(
            source_id_for("install-a", Path::new("/work/repo")),
            source_id_for("install-b", Path::new("/work/repo"))
        );
    }

    #[test]
    fn checkout_slug_is_valid_and_bounded() {
        let source = source_id_for("install-a", Path::new("/work/repo"));
        let value = slug(&"A Very Long Name!".repeat(20), &source);
        assert_eq!(value.len(), MAX_SLUG_LEN);
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        );
        assert!(value.ends_with(&source));
    }

    #[test]
    fn a_label_is_the_readable_folder_name() {
        // Against a server that derives projects itself the slug identifies
        // nothing, so it is what a person would expect to read rather than a
        // digest they have to look past.
        assert_eq!(label("semctx"), "semctx");
        assert_eq!(label("My Repo!"), "my-repo");
        assert_eq!(label("  "), "codebase");
        assert_eq!(label(""), "codebase");
    }

    #[test]
    fn a_label_is_valid_and_bounded() {
        let value = label(&"A Very Long Name!".repeat(20));

        assert!(value.len() <= MAX_SLUG_LEN);
        assert!(!value.starts_with('-') && !value.ends_with('-'));
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        );
    }

    #[test]
    fn two_checkouts_of_the_same_name_may_share_a_label() {
        // The collision the digest used to prevent is allowed now, because the
        // server no longer decides anything by the slug. Two of them meeting is
        // two rows with one name, not one row with two checkouts in it.
        assert_eq!(label("hv"), label("hv"));
    }

    #[test]
    fn ordinary_paths_keep_the_deployed_source_identity() {
        let path = Path::new("/work/repo");
        let legacy = format!("semctl-sync-source-v1\0install-a\0{}", path.display());
        assert_eq!(
            source_id_for("install-a", path),
            blake3::hash(legacy.as_bytes()).to_hex().to_string()
        );
    }

    #[test]
    fn a_folder_with_no_remote_keeps_a_slug_of_its_own() {
        // Nothing says two unrelated folders of this name are one project, so
        // nothing may fuse them: the checkout digest stays in the slug.
        let source = source_id_for("install-a", Path::new("/work/src"));
        assert!(slug("src", &source).ends_with(&source));
    }

    #[cfg(unix)]
    #[test]
    fn native_path_characters_cannot_alias_a_different_checkout() {
        use std::os::unix::ffi::OsStringExt;

        assert_ne!(
            source_id_for("install", Path::new("/work/same\\checkout")),
            source_id_for("install", Path::new("/work/same/checkout"))
        );
        let ambiguous = Path::new("/work/same\\checkout");
        assert_eq!(path_from_key(ambiguous.to_str().unwrap()), None);
        assert_eq!(
            path_from_key(&path_key(ambiguous)).as_deref(),
            Some(ambiguous)
        );
        let first = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/work/\xff".to_vec()));
        let second = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/work/\xfe".to_vec()));
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(
            source_id_for("install", &first),
            source_id_for("install", &second)
        );
        let replacement = Path::new("/work/\u{fffd}");
        assert_eq!(path_from_key(replacement.to_str().unwrap()), None);
        assert_ne!(
            source_id_for("install", replacement),
            source_id_for("install", &first)
        );
        assert_eq!(
            path_from_key(&path_key(replacement)).as_deref(),
            Some(replacement)
        );
        assert_eq!(
            path_from_key(&path_key(&first)).as_deref(),
            Some(first.as_path())
        );
        assert_eq!(
            path_from_key(&path_key(&second)).as_deref(),
            Some(second.as_path())
        );
        assert_ne!(path_key(&first), path_key(&second));
    }

    #[test]
    fn malformed_native_path_keys_fail_closed() {
        assert_eq!(path_from_key("semctl-path:unknown:AA"), None);
        assert_eq!(path_from_key("semctl-path:unix:!"), None);
        assert_eq!(path_from_key("semctl-path:unix:"), None);
        assert_eq!(path_from_key("semctl-path:unix:cmVsYXRpdmU"), None);
    }
}

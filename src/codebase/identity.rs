//! Stable, opaque identity for one local checkout.
//!
//! The server must distinguish two working copies even when their folder names
//! and Git remotes are identical: each submits a complete desired-state
//! manifest, so collapsing them onto one codebase makes them delete one
//! another's files. The raw path never leaves this machine. We hash it together
//! with the installation id and use that opaque value both for sync arbitration
//! and for a deterministic, tenant-unique local codebase slug.

use std::path::Path;

use anyhow::Result;

const MAX_SLUG_LEN: usize = 120;

/// Opaque stable identity for one local checkout. Different semctl processes in
/// the same checkout intentionally produce the same value; another clone gets a
/// different value even when it points at the same Git remote.
pub(crate) fn source_id(dir: &Path) -> Result<String> {
    let installation_id = crate::config::installation_id()?;
    Ok(source_id_for(&installation_id, dir))
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
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut path = canonical.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        path.make_ascii_lowercase();
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"semctl-sync-source-v1\0");
    hasher.update(installation_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.finalize().to_hex().to_string()
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

    use super::{MAX_SLUG_LEN, slug, source_id_for};

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
}

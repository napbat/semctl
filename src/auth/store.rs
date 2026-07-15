//! Credential storage: the token file.
//!
//! Tokens are written to a `0600` `credentials.json` in the semctl config dir
//! (see [`config::credentials_path`]). We deliberately don't use the OS keychain:
//! it's inconsistent across platforms — Windows Credential Manager caps a secret
//! at ~2560 bytes (our refresh-token JWT overflows it), and the Linux
//! secret-service backend silently no-ops in headless/CI shells with no D-Bus
//! daemon. Refresh tokens have long lifetimes (days/weeks) so we keep them;
//! access tokens are ephemeral and re-fetched on 401.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::config;

/// What the credentials file holds — both tokens together as JSON so a single
/// file covers the whole auth state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Seconds since the Unix epoch when the access token stops being valid.
    pub expires_at_unix: u64,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        let now = now_unix();
        // 30-second skew margin — refresh just before genuine expiry so
        // an in-flight request doesn't 401.
        self.expires_at_unix.saturating_sub(30) <= now
    }
}

pub fn load_tokens() -> Result<Option<TokenSet>> {
    // Headless / CI escape hatch: a raw access token via env, bypassing the
    // credentials file entirely. No refresh token — used as-is (never expires
    // locally) until the server 401s.
    if let Some(token) = std::env::var("SEMCTX_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        return Ok(Some(TokenSet {
            access_token: token,
            refresh_token: None,
            expires_at_unix: u64::MAX,
        }));
    }
    let path = config::credentials_path()?;
    let path = if path.exists() {
        path
    } else {
        // Fall back to the legacy `~/.config/semctx/` credentials so an existing
        // login survives the rename to semctl (read-only; a re-login writes the
        // new path).
        let legacy = config::legacy_config_dir()?.join("credentials.json");
        if legacy.exists() { legacy } else { path }
    };
    match fs::read_to_string(&path) {
        Ok(json) => {
            Ok(Some(serde_json::from_str(&json).with_context(|| {
                format!("parse stored token {}", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("read credentials {}: {e}", path.display())),
    }
}

pub fn save_tokens(tokens: &TokenSet) -> Result<()> {
    let path = config::credentials_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(tokens).context("serialize token")?;
    write_private(&path, &json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn clear_tokens() -> Result<()> {
    let path = config::credentials_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("clear credentials {}: {e}", path.display())),
    }
}

/// Write `contents` to `path` readable only by the owner. On Unix the file is
/// created with mode `0600` (and re-chmod'd in case it already existed with
/// looser bits); on Windows it inherits the user-profile ACL, which already
/// restricts access to the owner — matching how gh / aws store credentials.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Seconds since the Unix epoch, saturating to 0 if the clock predates it.
pub(super) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

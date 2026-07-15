//! On-disk config: server URL, identity provider URL, active tenant.
//!
//! Tokens are NOT in `config.toml` — they live in a sibling `credentials.json`
//! (see [`credentials_path`] and [`crate::auth`]), kept separate so the config is safe
//! to read/share while the secret stays in its own `0600` file.
//! Config dir is `~/.config/semctl/` on every platform (XDG-style, overridable
//! with `XDG_CONFIG_HOME`).

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

mod codebase_cache;
pub use codebase_cache::{cache_codebase, uncache_codebase_id};

/// Default server URL — the public napbat deployment. Overridden by CLI
/// `--server`, `SEMCTX_SERVER`, or `server_url` in the config file (which
/// `login` writes, so the last logged-in server becomes the default).
const DEFAULT_SERVER_URL: &str = "https://semctx.napbat.ca";

/// Default OAuth client id for the device-code flow — identity's dedicated
/// `semctx-cli` public client (device-authorization endpoint + `device_code` +
/// `refresh_token`, `semctx.api` scope). Override per-deployment with
/// `SEMCTX_CLIENT_ID` or the config file's `client_id` (see [`client_id`]).
pub const CLIENT_ID: &str = "semctx-cli";

/// Effective OIDC client id — `SEMCTX_CLIENT_ID` env > config file > the
/// built-in [`CLIENT_ID`] default.
pub fn client_id() -> String {
    std::env::var("SEMCTX_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| load().ok().and_then(|c| c.client_id))
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

/// Scopes the CLI requests. `offline_access` is what gives us a refresh
/// token so the user isn't forced through the device flow on every
/// access-token expiry.
pub const SCOPES: &[&str] = &["semctx.api", "offline_access"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Semctx server REST base URL. Override at the CLI with `--server`.
    /// This is the ONE endpoint the CLI needs to know — it asks the server
    /// where to authenticate (RFC 9728), so identity is never stored locally.
    pub server_url: Option<String>,

    /// Active tenant — slug or Guid. Override at the CLI with `--tenant`.
    pub active_tenant: Option<String>,

    /// Active codebase id (Guid) the code/graph tools operate on. Normally
    /// left unset: `semctl mcp` resolves the codebase from the working
    /// directory. An explicit `SEMCTX_CODEBASE` / `--codebase` overrides that.
    pub active_codebase: Option<String>,

    /// Cache of working-directory → codebase id, populated by `semctl index`.
    /// Lets `semctl mcp` resolve a folder it has indexed before without a
    /// server round-trip. Keyed by the canonical (absolute) directory path.
    #[serde(default)]
    pub codebase_cache: std::collections::HashMap<String, String>,

    /// Opt-in list of *umbrella* directories: a cached parent whose sub-repos may
    /// resolve to its codebase when they aren't indexed on their own. EMPTY BY
    /// DEFAULT — with none set, a folder with no codebase of its own resolves to
    /// nothing rather than being silently folded into a parent's index. Entries
    /// are matched exactly (canonical absolute paths) against a cached ancestor.
    /// See [`Config::cached_codebase_for`].
    #[serde(default)]
    pub umbrella_roots: Vec<String>,

    /// Glob form of [`Config::umbrella_roots`], matched against a cached *ancestor*
    /// directory's absolute path. `*` is separator-literal here (does NOT cross
    /// `/`), so `/home/me/git/*` blesses each immediate dir-of-repos as an
    /// umbrella without also blessing the repos nested inside them.
    #[serde(default)]
    pub umbrella_globs: Vec<String>,

    /// OIDC client id used for `semctl auth login`. Override when identity
    /// registers the CLI under a different client than the built-in default.
    pub client_id: Option<String>,
}

impl Config {
    /// Effective server URL — CLI flag / env > config file (the last server
    /// `login` recorded, or a manual entry) > the built-in napbat default.
    /// Blank/whitespace in any source is treated as unset.
    pub fn server_url(&self, cli_override: Option<&str>) -> String {
        cli_override
            .map(str::to_string)
            .or_else(|| std::env::var("SEMCTX_SERVER").ok())
            .or_else(|| self.server_url.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string())
    }

    /// Effective active tenant.
    pub fn active_tenant(&self, cli_override: Option<&str>) -> Option<String> {
        cli_override
            .map(str::to_string)
            .or_else(|| self.active_tenant.clone())
    }

    /// Effective active codebase — CLI flag / env > config file.
    pub fn active_codebase(&self, cli_override: Option<&str>) -> Option<String> {
        cli_override
            .map(str::to_string)
            .or_else(|| self.active_codebase.clone())
    }
}

/// Load the config, creating a default if no file exists. Errors only
/// when the file is present but malformed.
pub fn load() -> Result<Config> {
    // Read the semctl config; if it doesn't exist yet, fall back to the legacy
    // `~/.config/semctx/` file so an existing install keeps its server/tenant on
    // first run under the new name. The next `save` writes to the semctl path.
    let path = match config_path()? {
        p if p.exists() => p,
        _ => {
            let legacy = legacy_config_dir()?.join("config.toml");
            if legacy.exists() {
                legacy
            } else {
                return Ok(Config::default());
            }
        }
    };
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Write the config, creating parent directories as needed.
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(cfg).context("serialize config")?;
    fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Delete the semctl config directory (config + stored credentials) for
/// `semctl uninstall --purge`. Returns whether it existed. The legacy
/// `~/.config/semctx/` dir is left untouched.
pub fn remove_all() -> Result<bool> {
    let dir = config_dir()?;
    if !dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    Ok(true)
}

/// Path to the on-disk credentials file (the OIDC token set). A sibling of
/// `config.toml` in the semctl config dir. Plaintext JSON written `0600` on
/// Unix — the standard CLI pattern (gh, aws, gcloud all keep tokens in a
/// `~/.config` dotfile). See [`auth::save_tokens`](crate::auth::save_tokens).
pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

/// The semctl config directory — always `~/.config/semctl`, on every platform.
/// We deliberately don't use the OS-native location (macOS would otherwise put
/// this under `~/Library/Application Support`) so the path is identical
/// everywhere. `XDG_CONFIG_HOME` overrides the `~/.config` base when set.
fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(xdg) => PathBuf::from(xdg),
        None => dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir — set HOME or XDG_CONFIG_HOME"))?
            .join(".config"),
    };
    Ok(base.join("semctl"))
}

/// The legacy `~/.config/semctx/` directory this CLI shipped under before it was
/// renamed to `semctl`. Read-only fallback so an existing login/config survives
/// the rename; nothing is ever written here.
pub(crate) fn legacy_config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(xdg) => PathBuf::from(xdg),
        None => dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir — set HOME or XDG_CONFIG_HOME"))?
            .join(".config"),
    };
    Ok(base.join("semctx"))
}

/// Whether a legacy `~/.config/semctx/` config dir is present (the trigger for
/// the one-time migration in `semctl install`).
pub fn legacy_present() -> bool {
    legacy_config_dir().is_ok_and(|d| d.exists())
}

/// Copy `config.toml` + `credentials.json` from the legacy `~/.config/semctx/`
/// into the semctl config dir, only for files semctl doesn't already have.
/// Returns whether anything was copied.
pub fn migrate_from_legacy() -> Result<bool> {
    let legacy = legacy_config_dir()?;
    if !legacy.exists() {
        return Ok(false);
    }
    let dst = config_dir()?;
    let mut copied = false;
    for name in ["config.toml", "credentials.json"] {
        let (from, to) = (legacy.join(name), dst.join(name));
        if from.exists() && !to.exists() {
            fs::create_dir_all(&dst).with_context(|| format!("create {}", dst.display()))?;
            fs::copy(&from, &to).with_context(|| format!("copy {}", from.display()))?;
            // Keep the credentials file owner-only, matching how we write it.
            #[cfg(unix)]
            if name == "credentials.json" {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&to, fs::Permissions::from_mode(0o600));
            }
            copied = true;
        }
    }
    Ok(copied)
}

/// Remove the legacy `~/.config/semctx/` directory. Returns whether it existed.
pub fn remove_legacy() -> Result<bool> {
    let legacy = legacy_config_dir()?;
    if !legacy.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&legacy).with_context(|| format!("remove {}", legacy.display()))?;
    Ok(true)
}

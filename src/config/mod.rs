//! On-disk config: server URL, identity provider URL, active tenant.
//!
//! Tokens are NOT in `config.toml` — they live in a sibling `credentials.json`
//! (see [`crate::auth`]), kept separate so the config is safe
//! to read/share while the secret stays in its own `0600` file.
//! Config dir is `~/.config/semctl/` on every platform (XDG-style, overridable
//! with `XDG_CONFIG_HOME`).

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

mod codebase_cache;
mod persistence;
mod state;
pub use codebase_cache::{cache_codebase, uncache_codebase_id};
pub(crate) use persistence::{atomic_write_private, create_private_new, lock_file, open_lock};
pub(crate) use state::StateStore;

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

    /// Generation of the login whose server and tenant this config describes.
    /// Missing on legacy installations. Credentials must agree before use.
    pub auth_generation: Option<u64>,

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
    /// Persisted resource server, without invocation-specific environment values.
    pub(crate) fn persisted_server_url(&self) -> String {
        self.server_url
            .as_ref()
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string())
    }

    /// Effective server URL — CLI flag / env > config file (the last server
    /// `login` recorded, or a manual entry) > the built-in napbat default.
    /// Blank/whitespace in any source is treated as unset.
    pub fn server_url(&self, cli_override: Option<&str>) -> String {
        server_url_from(
            cli_override,
            std::env::var("SEMCTX_SERVER").ok().as_deref(),
            self.server_url.as_deref(),
        )
    }

    /// Effective active codebase — CLI flag / env > config file.
    pub fn active_codebase(&self, cli_override: Option<&str>) -> Option<String> {
        cli_override
            .map(str::to_string)
            .or_else(|| self.active_codebase.clone())
    }
}

fn server_url_from(
    cli: Option<&str>,
    environment: Option<&str>,
    configured: Option<&str>,
) -> String {
    cli.filter(|s| !s.trim().is_empty())
        .or_else(|| environment.filter(|s| !s.trim().is_empty()))
        .or_else(|| configured.filter(|s| !s.trim().is_empty()))
        .map_or_else(|| DEFAULT_SERVER_URL.to_string(), str::to_string)
}

/// Load the config, creating a default if no file exists. Errors only
/// when the file is present but malformed.
pub fn load() -> Result<Config> {
    load_from(&config_path()?, &legacy_config_dir()?.join("config.toml"))
}

fn load_from(current: &Path, legacy: &Path) -> Result<Config> {
    // Read the semctl config; if it doesn't exist yet, fall back to the legacy
    // `~/.config/semctx/` file so an existing install keeps its server/tenant on
    // first run under the new name. The next update writes to the semctl path.
    let (path, text) = match fs::read_to_string(current) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let result = fs::read_to_string(legacy);
            (legacy, result)
        }
        result => (current, result),
    };
    let text = match text {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Update the latest config under a cross-process lock and publish it atomically.
/// The closure runs on a blocking thread. It must only change local fields.
pub async fn update(change: impl FnOnce(&mut Config) + Send + 'static) -> Result<Config> {
    let state = StateStore::configured()?;
    let generation = state.generation()?;
    tokio::task::spawn_blocking(move || state.update_config(generation, change))
        .await
        .context("wait for config update")?
}

#[cfg(test)]
fn update_at(path: &Path, legacy: &Path, change: impl FnOnce(&mut Config)) -> Result<Config> {
    let state = StateStore {
        current: path
            .parent()
            .context("config path has no parent")?
            .to_path_buf(),
        legacy: legacy
            .parent()
            .context("legacy path has no parent")?
            .to_path_buf(),
    };
    let _lock = state.lock_blocking()?;
    let mut cfg = load_from(path, legacy)?;
    change(&mut cfg);
    let text = toml::to_string_pretty(&cfg).context("serialize config")?;
    atomic_write_private(path, text.as_bytes())?;
    Ok(cfg)
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Stable, opaque identity for this semctl installation.
///
/// Sync combines this value with the canonical checkout path before sending a
/// hash to the server. The installation id itself never leaves this machine.
/// A separate file avoids rewriting `config.toml` (and racing another semctl
/// process) merely to establish the identity.
pub(crate) fn installation_id() -> Result<String> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("installation-id");

    match read_installation_id(&path) {
        Ok(id) => return Ok(id),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    }

    let seed = format!(
        "semctl-installation-id-v1\0{}\0{}\0{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        dir.display()
    );
    let id = blake3::hash(seed.as_bytes()).to_hex().to_string();

    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(id.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.flush())
                .with_context(|| format!("write {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
            Ok(id)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            // Another semctl process won first-run creation. It may still be
            // finishing its tiny write, so give it a bounded moment to publish.
            for _ in 0..20 {
                if let Ok(id) = read_installation_id(&path) {
                    return Ok(id);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            read_installation_id(&path)
                .with_context(|| format!("read concurrently-created {}", path.display()))
        }
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

fn read_installation_id(path: &std::path::Path) -> std::io::Result<String> {
    let id = fs::read_to_string(path)?.trim().to_string();
    if id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "installation id must be 64 hexadecimal characters",
        ))
    }
}

/// Delete the semctl config directory (config, credentials, and local caches)
/// for `semctl uninstall --purge`. Returns whether it existed. The legacy
/// `~/.config/semctx/` dir is left untouched.
pub fn remove_all() -> Result<bool> {
    StateStore::configured()?.purge()
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

/// Private local history for verified workspace edit plans. It stores
/// preimages needed by MCP `undo_edit` and CLI `edit undo`; nothing here is sent to
/// the server.
pub(crate) fn edit_history_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("edit-history"))
}

/// Non-secret per-checkout file stamps, hashes, and content-filter decisions
/// used to avoid rereading unchanged files across `semctl index` runs.
pub(crate) fn sync_cache_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("sync-cache"))
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

/// Migrate legacy config and credentials without replacing existing files.
/// Remove the legacy directory only after every required file is published.
/// Return whether the legacy directory existed.
pub fn migrate_from_legacy() -> Result<bool> {
    let legacy = legacy_config_dir()?;
    migrate_from_paths(&legacy, &config_dir()?)
}

fn migrate_from_paths(legacy: &Path, dst: &Path) -> Result<bool> {
    if !legacy
        .try_exists()
        .with_context(|| format!("check {}", legacy.display()))?
    {
        return Ok(false);
    }
    let state = StateStore {
        current: dst.to_path_buf(),
        legacy: legacy.to_path_buf(),
    };
    let _lock = state.lock_blocking()?;
    if !legacy
        .try_exists()
        .with_context(|| format!("check {}", legacy.display()))?
    {
        return Ok(false);
    }
    for name in ["config.toml", "credentials.json"] {
        let (from, to) = (legacy.join(name), dst.join(name));
        if migration_file_exists(&from)? && !migration_file_exists(&to)? {
            let bytes = fs::read(&from).with_context(|| format!("read {}", from.display()))?;
            atomic_write_private(&to, &bytes)?;
        }
    }
    fs::remove_dir_all(legacy).with_context(|| format!("remove {}", legacy.display()))?;
    Ok(true)
}

fn migration_file_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            // A destination symlink could point into the directory that this
            // migration removes. Only regular files justify retiring a source.
            ensure!(
                metadata.is_file(),
                "migration requires a regular file at {}",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(test)]
mod tests;

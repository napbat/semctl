//! Private credential bytes and their persisted login binding.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;

use crate::config::{self, StateStore};

#[derive(Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: u64,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        self.expires_at_unix.saturating_sub(30) <= now_unix()
    }
}

/// Non-secret identity of one completed login. Refresh keeps this generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStamp {
    pub server_url: String,
    pub generation: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SessionBinding {
    #[serde(flatten)]
    pub stamp: SessionStamp,
    pub authority_url: String,
}

/// Flattening preserves the legacy token fields. Missing binding is readable,
/// but cannot authorize a request until the legacy issuer has been checked.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StoredCredentials {
    #[serde(flatten)]
    pub tokens: TokenSet,
    #[serde(default)]
    pub session: Option<SessionBinding>,
}

pub fn load_tokens() -> Result<Option<TokenSet>> {
    if let Some(tokens) = environment_tokens() {
        return Ok(Some(tokens));
    }
    Ok(CredentialStore::configured()?
        .load()?
        .map(|stored| stored.tokens))
}

pub(super) fn environment_tokens() -> Option<TokenSet> {
    std::env::var("SEMCTX_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
        .map(|access_token| TokenSet {
            access_token,
            refresh_token: None,
            expires_at_unix: u64::MAX,
        })
}

#[derive(Clone)]
pub(super) struct CredentialStore {
    pub state: StateStore,
}

impl CredentialStore {
    pub(super) fn configured() -> Result<Self> {
        Ok(Self {
            state: StateStore::configured()?,
        })
    }

    pub(super) fn load(&self) -> Result<Option<StoredCredentials>> {
        let current = self.state.current.join("credentials.json");
        let legacy = self.state.legacy.join("credentials.json");
        let (path, result) = match fs::read_to_string(&current) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let result = fs::read_to_string(&legacy);
                (legacy, result)
            }
            result => (current, result),
        };
        match result {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .with_context(|| format!("parse credentials {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("read credentials {}", path.display()))
            }
        }
    }

    /// The caller holds the shared state lock through publication.
    pub(super) fn save(&self, credentials: &StoredCredentials) -> Result<()> {
        let json = serde_json::to_vec_pretty(credentials).context("serialize credentials")?;
        config::atomic_write_private(&self.state.current.join("credentials.json"), &json)
    }

    /// The caller advances the generation before removal. A failed removal
    /// therefore leaves unusable credentials instead of reviving an older login.
    pub(super) fn clear(&self) -> Result<()> {
        let mut failures = Vec::new();
        for directory in [&self.state.legacy, &self.state.current] {
            let path = directory.join("credentials.json");
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    failures.push(format!("clear credentials {}: {error}", path.display()));
                }
            }
        }
        if !failures.is_empty() {
            return Err(anyhow!(failures.join("; ")));
        }
        Ok(())
    }
}

pub(super) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

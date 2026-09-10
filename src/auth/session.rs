//! Login publication, credential binding, and guarded tenant changes.

use anyhow::{Context, Result, anyhow, ensure};
use base64::Engine;

use super::store::{CredentialStore, SessionBinding, StoredCredentials, environment_tokens};
use super::{SessionStamp, TokenSet, discover_authority, refresh};

pub struct LoginAttempt {
    server_url: String,
    generation: u64,
}

pub struct AuthenticatedSession {
    pub access_token: String,
    pub authority_url: String,
    pub stamp: SessionStamp,
    pub active_tenant: Option<String>,
}

/// Compare complete resource URLs without credentials or secret query values.
/// Paths remain part of identity because deployments can share one HTTP origin.
pub fn normalize_server_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("invalid server URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "server URL must use HTTP or HTTPS"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "server URL must not contain credentials, a query, or a fragment"
    );
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub fn begin_login(server_url: &str) -> Result<LoginAttempt> {
    Ok(LoginAttempt {
        server_url: normalize_server_url(server_url)?,
        generation: CredentialStore::configured()?.state.generation()?,
    })
}

pub async fn finish_login(
    attempt: LoginAttempt,
    authority_url: &str,
    tokens: TokenSet,
) -> Result<AuthenticatedSession> {
    let store = CredentialStore::configured()?;
    let authority_url = normalize_server_url(authority_url)?;
    let lock = store.state.lock().await?;
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        publish_login(&store, attempt, authority_url, tokens)
    })
    .await
    .context("wait for login publication")?
}

fn publish_login(
    store: &CredentialStore,
    attempt: LoginAttempt,
    authority_url: String,
    tokens: TokenSet,
) -> Result<AuthenticatedSession> {
    store.state.check_generation(attempt.generation)?;
    let mut cfg = store.state.load_config()?;
    let generation = store.state.advance()?;
    let binding = SessionBinding {
        stamp: SessionStamp {
            server_url: attempt.server_url.clone(),
            generation,
        },
        authority_url,
    };
    cfg.server_url = Some(attempt.server_url);
    cfg.auth_generation = Some(generation);
    cfg.active_tenant = None;
    // Readers hold the same lock and require both generations to agree. A crash
    // between these publications fails closed instead of mixing two logins.
    store.state.save_config(&cfg)?;
    store.save(&StoredCredentials {
        tokens: tokens.clone(),
        session: Some(binding.clone()),
    })?;
    Ok(snapshot(tokens, binding, cfg.active_tenant))
}

pub async fn clear_tokens() -> Result<()> {
    let store = CredentialStore::configured()?;
    let lock = store.state.lock().await?;
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        store.state.advance()?;
        store.clear()
    })
    .await
    .context("wait for logout")?
}

pub async fn get_valid_access_token(http: &reqwest::Client, server_url: &str) -> Result<String> {
    // This explicit headless override belongs to this invocation. It never
    // borrows a persisted refresh token or changes stored login state.
    if let Some(tokens) = environment_tokens() {
        return Ok(tokens.access_token);
    }
    Ok(authenticated_session(http, server_url).await?.access_token)
}

pub async fn authenticated_session(
    http: &reqwest::Client,
    server_url: &str,
) -> Result<AuthenticatedSession> {
    let server_url = normalize_server_url(server_url)?;
    let store = CredentialStore::configured()?;
    if let Some(tokens) = environment_tokens() {
        let authority_url = discover_authority(http, &server_url).await?;
        let _lock = store.state.lock().await?;
        let cfg = store.state.load_config()?;
        let active_tenant = (normalize_server_url(&cfg.persisted_server_url())? == server_url)
            .then_some(cfg.active_tenant)
            .flatten();
        return Ok(AuthenticatedSession {
            access_token: tokens.access_token,
            authority_url: normalize_server_url(&authority_url)?,
            stamp: SessionStamp {
                server_url,
                generation: store.state.generation()?,
            },
            active_tenant,
        });
    }
    valid_stored_session(http, &store, &server_url).await
}

pub(super) async fn valid_stored_session(
    http: &reqwest::Client,
    store: &CredentialStore,
    server_url: &str,
) -> Result<AuthenticatedSession> {
    let lock = store.state.lock().await?;
    let mut credentials = store
        .load()?
        .ok_or_else(|| anyhow!("not logged in — run `semctl auth login`"))?;
    let mut cfg = store.state.load_config()?;
    let generation = store.state.generation()?;
    let binding = if let Some(binding) = credentials.session.clone() {
        ensure!(
            binding.stamp.generation == generation && cfg.auth_generation == Some(generation),
            "login state is incomplete or was cleared; run `semctl auth login`"
        );
        ensure!(
            normalize_server_url(&binding.stamp.server_url)? == server_url
                && normalize_server_url(&cfg.persisted_server_url())? == server_url,
            "stored login belongs to another server; run `semctl auth login --server <url>` for this server"
        );
        binding
    } else {
        ensure!(
            generation == 0 && cfg.auth_generation.is_none(),
            "legacy login was invalidated; run `semctl auth login`"
        );
        ensure!(
            normalize_server_url(&cfg.persisted_server_url())? == server_url,
            "legacy login is not bound to this server; run `semctl auth login`"
        );
        let authority_url = normalize_server_url(&discover_authority(http, server_url).await?)?;
        ensure!(
            legacy_issuer(&credentials.tokens.access_token).as_deref() == Some(&authority_url),
            "legacy credential issuer cannot be verified; run `semctl auth login`"
        );
        let generation = store.state.advance()?;
        let binding = SessionBinding {
            stamp: SessionStamp {
                server_url: server_url.to_string(),
                generation,
            },
            authority_url,
        };
        cfg.auth_generation = Some(generation);
        store.state.save_config(&cfg)?;
        credentials.session = Some(binding.clone());
        store.save(&credentials)?;
        binding
    };
    if credentials.tokens.is_expired() {
        let refresh_token = credentials.tokens.refresh_token.as_deref().ok_or_else(|| {
            anyhow!("access token expired and no refresh token — run `semctl auth login`")
        })?;
        // Refresh only at the authority recorded by login. Resource metadata
        // cannot redirect an existing refresh token to a different authority.
        credentials.tokens = refresh(http, &binding.authority_url, refresh_token).await?;
        let store = store.clone();
        let refreshed = credentials.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            store.save(&refreshed)
        })
        .await
        .context("wait for refreshed credentials")??;
    }
    Ok(snapshot(credentials.tokens, binding, cfg.active_tenant))
}

fn legacy_issuer(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    normalize_server_url(claims.get("iss")?.as_str()?).ok()
}

fn snapshot(
    tokens: TokenSet,
    binding: SessionBinding,
    active_tenant: Option<String>,
) -> AuthenticatedSession {
    AuthenticatedSession {
        access_token: tokens.access_token,
        authority_url: binding.authority_url,
        stamp: binding.stamp,
        active_tenant,
    }
}

/// Persist a membership result only while both its login and tenant snapshot
/// remain current. A delayed login or prompt must not overwrite a newer choice.
pub async fn set_active_tenant(
    stamp: &SessionStamp,
    expected: Option<String>,
    selected: Option<String>,
) -> Result<bool> {
    let store = CredentialStore::configured()?;
    let stamp = stamp.clone();
    let lock = store.state.lock().await?;
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        set_tenant(&store, &stamp, expected.as_deref(), selected)
    })
    .await
    .context("wait for tenant selection")?
}

fn set_tenant(
    store: &CredentialStore,
    stamp: &SessionStamp,
    expected: Option<&str>,
    selected: Option<String>,
) -> Result<bool> {
    let mut cfg = store.state.load_config()?;
    if store.state.generation()? != stamp.generation
        || normalize_server_url(&cfg.persisted_server_url())? != stamp.server_url
        || cfg.active_tenant.as_deref() != expected
    {
        return Ok(false);
    }
    cfg.active_tenant = selected;
    store.state.save_config(&cfg)?;
    Ok(true)
}

#[cfg(test)]
mod tests;

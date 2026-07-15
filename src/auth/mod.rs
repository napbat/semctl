//! OIDC device-code flow + the get-a-valid-token orchestration.
//!
//! Why device code: a CLI can't reasonably run a local HTTP server + browser
//! redirect for an authorization-code flow. Device flow shows the user a short
//! code to complete in their browser while the CLI polls the token endpoint —
//! the standard CLI auth pattern (gcloud / aws sso / gh).
//!
//! Credential storage (the `credentials.json` token file) lives in [`store`];
//! its public API is re-exported here so callers keep using `crate::auth::*`.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::config::{self, SCOPES};

mod store;

pub use store::{TokenSet, clear_tokens, load_tokens, save_tokens};

/// RFC 8628 device-authorization response from `/connect/device`.
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    /// Optional combined URL with the `user_code` pre-filled — RFC 8628 §3.2.
    /// Identity may emit either or both.
    verification_uri_complete: Option<String>,
    expires_in: u64,
    /// Poll interval in seconds. Optional per RFC 8628 §3.2 — identity omits it
    /// when it's the default; fall back to 5s.
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
}

/// OAuth 2.0 Protected Resource Metadata (RFC 9728) — the field we use.
#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

/// Ask the semctx server which authorization server to authenticate against —
/// the "resource server tells the client the authority" leg of OAuth, served at
/// `/.well-known/oauth-protected-resource` (RFC 9728). Best-effort — the caller
/// falls back to the configured/default identity when this fails (an older
/// server, or one that doesn't expose the document).
pub async fn discover_authority(http: &reqwest::Client, server_url: &str) -> Result<String> {
    let url = format!(
        "{}/.well-known/oauth-protected-resource",
        server_url.trim_end_matches('/')
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("{url} -> {}", resp.status());
    }
    let meta: ProtectedResourceMetadata =
        resp.json().await.with_context(|| format!("parse {url}"))?;
    meta.authorization_servers
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("{url}: server advertised no authorization_servers"))
}

/// Step 1: request a device + user code from identity. Returns the
/// codes plus the URL to show the user.
pub async fn request_device_code(http: &reqwest::Client, identity_url: &str) -> Result<DeviceInit> {
    let url = format!("{}/connect/device", identity_url.trim_end_matches('/'));
    let client_id = config::client_id();
    let resp = http
        .post(&url)
        .form(&[
            ("client_id", client_id.as_str()),
            ("scope", &SCOPES.join(" ")),
        ])
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("device init {status}: {body}");
    }
    let raw: DeviceCodeResponse = resp.json().await.context("parse device init response")?;
    Ok(DeviceInit {
        device_code: raw.device_code,
        user_code: raw.user_code,
        verification_url: raw
            .verification_uri_complete
            .unwrap_or(raw.verification_uri),
        expires_in: Duration::from_secs(raw.expires_in),
        poll_interval: Duration::from_secs(raw.interval.max(1)),
    })
}

#[derive(Debug, Clone)]
pub struct DeviceInit {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: Duration,
    pub poll_interval: Duration,
}

/// Step 2: poll the token endpoint until the user completes the flow
/// (or we time out). RFC 8628 §3.5 specifies the polling protocol +
/// the `authorization_pending` / `slow_down` error semantics.
pub async fn poll_for_token(
    http: &reqwest::Client,
    identity_url: &str,
    init: &DeviceInit,
) -> Result<TokenSet> {
    let url = format!("{}/connect/token", identity_url.trim_end_matches('/'));
    let client_id = config::client_id();
    let deadline = Instant::now() + init.expires_in;
    let mut interval = init.poll_interval;

    loop {
        if Instant::now() >= deadline {
            bail!("device flow timed out — re-run `semctl auth login`");
        }
        tokio::time::sleep(interval).await;

        let resp = http
            .post(&url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &init.device_code),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        if resp.status().is_success() {
            let raw: TokenResponse = resp.json().await.context("parse token response")?;
            return Ok(materialize(raw));
        }

        // Not yet ready — RFC 8628 says inspect the error field.
        let err: TokenErrorResponse = resp.json().await.context("parse token error")?;
        match err.error.as_str() {
            "authorization_pending" => {
                // Keep polling at the current interval.
            }
            "slow_down" => {
                // Bump interval by 5s (spec recommendation).
                interval += Duration::from_secs(5);
            }
            "access_denied" => bail!("user denied the request at identity"),
            "expired_token" => bail!("device code expired — re-run `semctl auth login`"),
            other => bail!("device flow error: {other}"),
        }
    }
}

/// Refresh an access token using the stored refresh token. Returns the
/// new token set (with a possibly-rotated refresh token).
pub async fn refresh(
    http: &reqwest::Client,
    identity_url: &str,
    refresh_token: &str,
) -> Result<TokenSet> {
    let url = format!("{}/connect/token", identity_url.trim_end_matches('/'));
    let client_id = config::client_id();
    let resp = http
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("refresh {status}: {body}");
    }
    let raw: TokenResponse = resp.json().await.context("parse refresh response")?;
    Ok(materialize(raw))
}

fn materialize(raw: TokenResponse) -> TokenSet {
    let expires_at_unix = store::now_unix() + raw.expires_in;
    TokenSet {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        expires_at_unix,
    }
}

/// Convenience: load the stored token, refresh if expired, save the
/// refreshed token, return the access string ready for `Bearer`. Used
/// by every authenticated subcommand before hitting the server.
pub async fn get_valid_access_token(http: &reqwest::Client) -> Result<String> {
    let cfg = config::load()?;
    let mut tokens =
        load_tokens()?.ok_or_else(|| anyhow!("not logged in — run `semctl auth login`"))?;

    if tokens.is_expired() {
        let refresh_tok = tokens.refresh_token.clone().ok_or_else(|| {
            anyhow!("access token expired and no refresh token — re-run `semctl auth login`")
        })?;
        // Ask the server where to authenticate (RFC 9728) — the same authority
        // login used. The CLI never caches identity, so a moved identity server
        // is picked up on the next refresh instead of dead-ending.
        let identity_url = discover_authority(http, &cfg.server_url(None)).await?;
        tokens = refresh(http, &identity_url, &refresh_tok).await?;
        save_tokens(&tokens)?;
    }
    Ok(tokens.access_token)
}

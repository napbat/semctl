use anyhow::Result;

use crate::auth;
use crate::cli::Cli;
use crate::config;

/// The server doesn't expose `/v1/me` yet — until it does, we decode
/// the JWT's `sub` / `email` claims locally and print those, plus
/// check that the server accepts our token by pinging `/v1/domains`.
pub async fn run(cli: &Cli) -> Result<()> {
    let http = reqwest::Client::new();
    let token = auth::get_valid_access_token(&http).await?;

    let claims = decode_jwt_payload(&token)?;
    let sub = claims
        .get("sub")
        .and_then(|v| v.as_str())
        .unwrap_or("(no sub)");
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("(no email)");
    let name = claims.get("name").and_then(|v| v.as_str()).unwrap_or("");

    println!("sub:    {sub}");
    println!("email:  {email}");
    if !name.is_empty() {
        println!("name:   {name}");
    }

    // Token claims that determine whether the server accepts it — the server
    // pins issuer + audience, so a mismatch here is the usual 401 cause.
    println!(
        "iss:    {}",
        claims
            .get("iss")
            .and_then(|v| v.as_str())
            .unwrap_or("(none)")
    );
    println!("aud:    {}", render_aud(&claims));
    if let Some(scope) = claims.get("scope").and_then(|v| v.as_str()) {
        println!("scope:  {scope}");
    }
    if let Some(exp) = claims.get("exp").and_then(serde_json::Value::as_i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let state = if exp <= now { "EXPIRED" } else { "valid" };
        println!("exp:    {exp} ({state}, {}s from now)", exp - now);
    }

    let cfg = config::load()?;
    if let Some(tenant) = cfg.active_tenant(cli.tenant.as_deref()) {
        println!("tenant: {tenant} (active)");
    } else {
        println!("tenant: (none — set with `semctl auth tenants --switch <slug>`)");
    }

    // Liveness probe — proves the access token actually works against
    // the configured server.
    let client = crate::client::from_cli(cli)?;
    match client
        .get::<Vec<crate::client::api::DomainDescriptor>>("/v1/domains")
        .await
    {
        Ok(domains) => println!("server: ok ({} domain(s) registered)", domains.len()),
        Err(e) => println!("server: UNREACHABLE — {e}"),
    }
    Ok(())
}

/// `aud` may be a single string or an array of strings (JWT spec allows both).
fn render_aud(claims: &serde_json::Value) -> String {
    match claims.get("aud") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => "(none)".to_string(),
    }
}

/// Decode the *payload* segment of a JWT — middle of the three dot-separated
/// parts — without verifying the signature. The server already verified;
/// we just want the claims for display.
fn decode_jwt_payload(token: &str) -> Result<serde_json::Value> {
    use anyhow::anyhow;
    use base64::Engine;

    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("malformed JWT — expected three segments"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| anyhow!("decode JWT payload: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| anyhow!("parse JWT claims: {e}"))
}

use anyhow::{Context, Result, anyhow};
use clap::Args;

use crate::auth;
use crate::cli::Cli;
use crate::client::api;
use crate::config;

#[derive(Debug, Args)]
pub struct TenantsArgs {
    /// Set the named tenant (slug or Guid) as the active one. Saved to
    /// the config file; subsequent invocations apply `X-Tenant-Id`
    /// automatically unless overridden by `--tenant`.
    #[arg(long)]
    pub switch: Option<String>,
}

/// Fetch the caller's tenant memberships from identity. Tenants live there,
/// not on the semctx server's REST surface; the same bearer works for both.
/// Shared by this command and `semctl auth login` (post-login tenant selection).
pub async fn fetch(
    http: &reqwest::Client,
    identity_url: &str,
    token: &str,
) -> Result<Vec<api::TenantDto>> {
    let url = format!("{}/v1/tenants", identity_url.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("identity {url} -> {status}: {body}"));
    }

    let envelope: api::TenantsEnvelope = resp.json().await.context("parse tenants response")?;
    Ok(envelope.data.map(|p| p.items).unwrap_or_default())
}

pub async fn run(args: TenantsArgs, cli: &Cli) -> Result<()> {
    let cfg = config::load()?;
    let server_url = cfg.server_url(cli.server.as_deref());

    let http = reqwest::Client::new();
    // Discover the authority from the server (same as login + refresh) — the CLI
    // never caches where identity lives.
    let identity_url = auth::discover_authority(&http, &server_url).await?;
    let token = auth::get_valid_access_token(&http).await?;
    let items = fetch(&http, &identity_url, &token).await?;

    if items.is_empty() {
        println!("(no tenant memberships)");
        return Ok(());
    }

    let active = cfg.active_tenant(None);
    println!("{:<36}  {:<24}  NAME", "ID", "SLUG");
    for t in &items {
        let marker = if active.as_deref() == Some(t.slug.as_str())
            || active.as_deref() == Some(t.id.as_str())
        {
            " *"
        } else {
            "  "
        };
        println!("{}{:<36}  {:<24}  {}", marker, t.id, t.slug, t.name);
    }

    if let Some(switch) = args.switch {
        // Validate the target is in our membership list — better to
        // fail here than silently set an invalid active_tenant.
        let matched = items
            .iter()
            .find(|t| t.slug == switch || t.id == switch)
            .ok_or_else(|| anyhow!("'{switch}' is not in your tenant memberships"))?;

        let mut cfg = config::load()?;
        cfg.active_tenant = Some(matched.slug.clone());
        config::save(&cfg)?;
        println!();
        println!("Active tenant -> {} ({})", matched.slug, matched.name);
    }

    Ok(())
}

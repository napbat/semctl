use std::io::{IsTerminal, Write};

use anyhow::{Context, Result};
use clap::Args;

use super::tenants;
use crate::auth;
use crate::cli::Cli;
use crate::client::api;
use crate::config;

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Don't try to open the verification URL in a browser — just print it.
    /// (Auto-open is already skipped on a non-interactive / headless shell.)
    #[arg(long)]
    pub no_open: bool,
}

pub async fn run(args: LoginArgs, cli: &Cli) -> Result<()> {
    let mut cfg = config::load()?;
    // The server we're logging into (--server / SEMCTX_SERVER > config > default).
    let server_url = cfg.server_url(cli.server.as_deref());

    let http = reqwest::Client::new();

    // Ask the server where to authenticate (RFC 9728 protected-resource
    // metadata). The server is the single source of truth — no local override or
    // cache — so if identity ever moves, every CLI follows it.
    let identity_url = auth::discover_authority(&http, &server_url)
        .await
        .with_context(|| format!("{server_url} didn't tell us where to authenticate"))?;
    println!("{server_url} authenticates via {identity_url}");

    println!("Requesting device code from {identity_url} …");
    let init = auth::request_device_code(&http, &identity_url).await?;

    println!();
    println!("  Open this URL in your browser:");
    println!("    {}", init.verification_url);
    println!();
    println!("  Enter the code:");
    println!("    {}", init.user_code);
    println!();

    // Best-effort convenience: pop the URL open (it has the code pre-filled).
    // Only when interactive — on a headless / SSH box there's no browser, and
    // the URL is printed above for manual use either way.
    if !args.no_open && std::io::stdin().is_terminal() {
        open_browser(&init.verification_url);
    }

    println!(
        "Waiting for you to complete the flow (expires in {} min) …",
        init.expires_in.as_secs() / 60
    );

    let tokens = auth::poll_for_token(&http, &identity_url, &init).await?;
    auth::save_tokens(&tokens)?;

    // Remember which server we logged into; identity is never cached — it's
    // re-discovered from the server on every login / refresh.
    cfg.server_url = Some(server_url.clone());
    config::save(&cfg)?;

    println!();
    println!("Logged in to {server_url}. Tokens stored in ~/.config/semctl/credentials.json.");

    // Resolve an active tenant so the user is ready to `index` / `search`
    // without a separate `semctl auth tenants --switch`. Best-effort — never fail
    // the login over this.
    select_tenant(&http, &identity_url, &tokens.access_token, &mut cfg).await;

    Ok(())
}

/// Pick the active tenant after login and persist it to the config.
///
/// - active tenant already set → leave it (just report it);
/// - exactly one membership → auto-select;
/// - several, interactive shell → prompt;
/// - several, non-interactive → list them and point at `tenants --switch`.
async fn select_tenant(
    http: &reqwest::Client,
    identity_url: &str,
    token: &str,
    cfg: &mut config::Config,
) {
    if let Some(active) = cfg.active_tenant(None) {
        println!("Active tenant: {active} (change with `semctl auth tenants --switch <slug>`).");
        return;
    }

    let tenants = match tenants::fetch(http, identity_url, token).await {
        Ok(t) => t,
        Err(e) => {
            // Don't derail a successful login; the user can run `semctl auth tenants`.
            eprintln!(
                "note: couldn't list tenants ({e}); set one with `semctl auth tenants --switch`."
            );
            return;
        }
    };

    let chosen = match tenants.as_slice() {
        [] => {
            println!("No tenant memberships yet.");
            return;
        }
        [only] => only,
        many if std::io::stdin().is_terminal() => {
            if let Some(i) = prompt_tenant(many) {
                &many[i]
            } else {
                println!(
                    "No tenant set. Choose one later with `semctl auth tenants --switch <slug>`."
                );
                return;
            }
        }
        _ => {
            println!(
                "You belong to multiple tenants — pick one with `semctl auth tenants --switch <slug>`."
            );
            return;
        }
    };

    cfg.active_tenant = Some(chosen.slug.clone());
    if let Err(e) = config::save(cfg) {
        eprintln!("note: logged in but couldn't save active tenant ({e}).");
        return;
    }
    println!("Active tenant -> {} ({}).", chosen.slug, chosen.name);
}

/// Interactive numbered picker. Returns the chosen index, or `None` if the
/// user pressed Enter / gave no valid choice.
fn prompt_tenant(tenants: &[api::TenantDto]) -> Option<usize> {
    println!();
    println!("You belong to several tenants:");
    for (i, t) in tenants.iter().enumerate() {
        println!("  [{}] {:<24} {}", i + 1, t.slug, t.name);
    }
    print!("Select a tenant [1-{}] (Enter to skip): ", tenants.len());
    std::io::stdout().flush().ok()?;

    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let n: usize = line.trim().parse().ok()?;
    n.checked_sub(1).filter(|&i| i < tenants.len())
}

/// Best-effort: open `url` in the user's default browser via the `open` crate
/// (per-OS launcher + WSL handling). Detached so we don't block on the
/// launcher; never fails the login — if there's no GUI / handler, the URL is
/// already printed above for manual use.
fn open_browser(url: &str) {
    let _ = open::that_detached(url);
}

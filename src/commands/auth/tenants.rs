use std::io::{IsTerminal, Write};

use anyhow::{Result, anyhow};
use clap::Args;

use crate::auth;
use crate::cli::Cli;
use crate::config;

#[derive(Debug, Args)]
pub struct TenantsArgs {
    /// Set the named tenant (slug or Guid) as the active one. Saved to
    /// the config file; subsequent invocations apply `X-Tenant-Id`
    /// automatically unless overridden by `--tenant`.
    #[arg(long)]
    pub switch: Option<String>,
}

pub async fn run(args: TenantsArgs, cli: &Cli) -> Result<()> {
    let cfg = config::load()?;
    let server_url = cfg.server_url(cli.server.as_deref());

    let http = reqwest::Client::new();
    // Discover the authority from the server (same as login + refresh) — the CLI
    // never caches where identity lives.
    let identity_url = auth::discover_authority(&http, &server_url).await?;
    let token = auth::get_valid_access_token(&http).await?;
    let items = auth::fetch_tenants(&http, &identity_url, &token).await?;

    if items.is_empty() {
        println!("(no tenant memberships)");
        return Ok(());
    }

    let active = cfg.active_tenant(None);
    let interactive = args.switch.is_none() && std::io::stdin().is_terminal() && items.len() > 1;
    if interactive {
        println!("{:<5}  {:<36}  {:<24}  NAME", "#", "ID", "SLUG");
    } else {
        println!("{:<36}  {:<24}  NAME", "ID", "SLUG");
    }
    for (index, t) in items.iter().enumerate() {
        let marker = if active.as_deref() == Some(t.slug.as_str())
            || active.as_deref() == Some(t.id.as_str())
        {
            " *"
        } else {
            "  "
        };
        if interactive {
            println!(
                "[{:>2}]{}  {:<36}  {:<24}  {}",
                index + 1,
                marker,
                t.id,
                t.slug,
                t.name
            );
        } else {
            println!("{}{:<36}  {:<24}  {}", marker, t.id, t.slug, t.name);
        }
    }

    let selected = if let Some(switch) = args.switch.as_deref() {
        // Validate the target is in our membership list — better to
        // fail here than silently set an invalid active_tenant.
        Some(
            items
                .iter()
                .find(|t| t.slug == switch || t.id == switch)
                .ok_or_else(|| anyhow!("'{switch}' is not in your tenant memberships"))?,
        )
    } else if interactive {
        prompt_index(items.len()).map(|index| &items[index])
    } else {
        None
    };

    if let Some(matched) = selected {
        let mut cfg = config::load()?;
        cfg.active_tenant = Some(matched.slug.clone());
        config::save(&cfg)?;
        println!();
        println!("Active tenant -> {} ({})", matched.slug, matched.name);
    }

    Ok(())
}

fn prompt_index(count: usize) -> Option<usize> {
    loop {
        print!("Select a tenant [1-{count}] (Enter to cancel): ");
        std::io::stdout().flush().ok()?;

        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            return None;
        }
        if let Some(index) = parse_selection(&line, count) {
            return Some(index);
        }
        println!("Enter a number from 1 to {count}, or press Enter to cancel.");
    }
}

fn parse_selection(input: &str, count: usize) -> Option<usize> {
    input
        .trim()
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
        .filter(|&index| index < count)
}

#[cfg(test)]
mod tests {
    use super::parse_selection;

    #[test]
    fn interactive_selection_is_one_based_and_bounds_checked() {
        assert_eq!(parse_selection("1", 3), Some(0));
        assert_eq!(parse_selection(" 3 \n", 3), Some(2));
        assert_eq!(parse_selection("0", 3), None);
        assert_eq!(parse_selection("4", 3), None);
        assert_eq!(parse_selection("nope", 3), None);
        assert_eq!(parse_selection("", 3), None);
    }
}

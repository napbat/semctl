use anyhow::Result;

use crate::cli::Cli;
use crate::client::{self, api};

pub async fn run(cli: &Cli) -> Result<()> {
    let client = client::from_cli(cli)?;
    let domains: Vec<api::DomainDescriptor> = client.get("/v1/domains").await?;

    if domains.is_empty() {
        println!("(no domains registered)");
        return Ok(());
    }

    for d in &domains {
        println!("{}  ({})", d.id, d.display_name);
        for tag in &d.tag_schema {
            println!(
                "    {:<14} {:<10}  {}",
                tag.name, tag.data_type, tag.description
            );
        }
    }
    Ok(())
}

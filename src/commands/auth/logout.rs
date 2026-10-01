use anyhow::Result;

use crate::auth;

pub async fn run() -> Result<()> {
    auth::clear_tokens().await?;
    println!("Logged out. Run `semctl auth login` to authenticate again.");
    Ok(())
}

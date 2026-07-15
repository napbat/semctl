use anyhow::Result;

use crate::auth;

pub fn run() -> Result<()> {
    auth::clear_tokens()?;
    println!("Logged out. Run `semctl auth login` to authenticate again.");
    Ok(())
}

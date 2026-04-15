use anyhow::Result;

use crate::config;
use crate::name;

pub fn run(shadow_name: String) -> Result<()> {
    let config = config::load_or_create()?;
    let shadow_path = name::resolve_name(&shadow_name, &config)?;
    println!("{}", shadow_path.display());
    Ok(())
}

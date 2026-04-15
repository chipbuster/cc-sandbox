use anyhow::Result;

use crate::config;
use crate::devcontainer;
use crate::name;

pub fn run(shadow_name: String) -> Result<()> {
    let config = config::load_or_create()?;
    let shadow_path = name::resolve_name(&shadow_name, &config)?;

    eprintln!("Starting devcontainer...");
    devcontainer::devcontainer_up(&shadow_path)?;

    devcontainer::devcontainer_exec(&shadow_path, &config.shell.command)?;

    Ok(())
}

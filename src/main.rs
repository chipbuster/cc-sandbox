mod cli;
mod commands;
mod config;
mod devcontainer;
mod fs_lookup;
mod name;
mod shadow;

use std::process::Command as ProcessCommand;

use anyhow::{Result, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command};

fn check_external_tools() -> Result<()> {
    let required = ["cp", "rsync", "docker", "devcontainer"];
    let mut missing = Vec::new();

    for tool in &required {
        let result = ProcessCommand::new("which").arg(tool).output();
        let found = match result {
            Ok(output) => output.status.success(),
            Err(_) => false,
        };
        if !found {
            missing.push(*tool);
        }
    }

    if !missing.is_empty() {
        let list = missing.join(", ");
        bail!(
            "Required tools not found on PATH: {list}.\n\
             cc-sandbox needs: cp (GNU coreutils), rsync, docker, and devcontainer.\n\
             Install devcontainer with: npm install -g @devcontainers/cli"
        );
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("trace")
    } else {
        EnvFilter::new("warn")
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    check_external_tools()?;

    match cli.command {
        Command::Start { path, name } => commands::start::run(path, name),
        Command::List => commands::list::run(),
        Command::Shell { name } => commands::shell::run(name),
        Command::Accept { name, yes } => commands::accept::run(name, yes),
        Command::Reject { name, yes } => commands::reject::run(name, yes),
        Command::Path { name } => commands::path::run(name),
    }
}

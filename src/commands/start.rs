use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::devcontainer;
use crate::fs_lookup;
use crate::shadow;

pub fn run(path: PathBuf, name: Option<String>) -> Result<()> {
    let source = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve path {}", path.display()))?;

    if !source.is_dir() {
        bail!("{} is not a directory.", source.display());
    }

    if !config::has_devcontainer(&source) {
        bail!(
            "No devcontainer configuration found in {}.\n\
             cc-sandbox requires a .devcontainer/devcontainer.json or .devcontainer.json.\n\
             Create one before using cc-sandbox, or use a different tool.",
            source.display()
        );
    }

    // Warn on dirty git state, but don't block.
    check_git_dirty(&source);

    let mut config = config::load_or_create()?;
    let shadow_root = fs_lookup::resolve_shadow_root(&mut config, &source)?;
    let mount_point = fs_lookup::find_mount_point(&source)?;

    let shadow_path =
        shadow::compute_shadow_path(&shadow_root, &mount_point, &source, name.as_deref())?;

    eprintln!("Creating shadow at {}", shadow_path.display());
    shadow::create_shadow(&source, &shadow_path)?;

    let meta = shadow::ShadowMeta {
        source: source.clone(),
        created_at: chrono::Local::now(),
        name_suffix: name
            .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d-%H%M").to_string()),
    };
    shadow::write_meta(&shadow_path, &meta)?;

    eprintln!("Starting devcontainer...");
    devcontainer::devcontainer_up(&shadow_path)?;

    eprintln!("Launching agent...");
    let status = devcontainer::devcontainer_exec(&shadow_path, &config.agent.command)?;

    if !status.success() {
        eprintln!(
            "Agent exited with status {}. The shadow is still available at {}",
            status,
            shadow_path.display()
        );
    }

    Ok(())
}

fn check_git_dirty(source: &std::path::Path) {
    let output = Command::new("git")
        .args(["-C", &source.display().to_string(), "status", "--porcelain"])
        .output();

    if let Ok(output) = output
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            eprintln!(
                "Warning: {} has uncommitted changes. The shadow will include them.",
                source.display()
            );
        }
        // If git status fails (not a git repo), that's fine -- just skip the warning.
    }
}

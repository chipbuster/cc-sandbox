use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::devcontainer;
use crate::name;
use crate::shadow;

pub fn run(shadow_name: String, yes: bool) -> Result<()> {
    let config = config::load_or_create()?;
    let shadow_path = name::resolve_name(&shadow_name, &config)?;
    let meta = shadow::read_meta(&shadow_path)?;

    if !meta.source.is_dir() {
        bail!(
            "Source directory {} no longer exists. It may have been moved or deleted.\n\
             The shadow is still at {} — you can inspect it manually with `cc-sandbox path`.",
            meta.source.display(),
            shadow_path.display()
        );
    }

    let summary = shadow::change_summary(&shadow_path, &meta.source)?;
    eprintln!("Changes: {summary}");

    if !yes {
        eprint!(
            "Accept {} back to {}? [y/N]: ",
            summary,
            meta.source.display()
        );
        if !confirm_stdin()? {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    // Stop the container. Tolerate missing.
    devcontainer::stop_and_remove_container(&shadow_path)?;

    // SAFETY: shadow is NOT deleted if rsync fails. The user can inspect and retry.
    let exclude_flag = format!("--exclude={}", config::meta_filename());
    let status = Command::new("rsync")
        .args([
            "-a",
            "--delete",
            &exclude_flag,
            &format!("{}/", shadow_path.display()),
            &format!("{}/", meta.source.display()),
        ])
        .status()
        .context("Failed to execute rsync")?;

    if !status.success() {
        bail!(
            "rsync failed. The shadow is preserved at {} — inspect and retry.\n\
             The source at {} may be partially updated.",
            shadow_path.display(),
            meta.source.display()
        );
    }

    // rsync succeeded; safe to delete shadow.
    // Defense in depth: verify the shadow path is under a known shadow root.
    let under_shadow_root = config
        .filesystem
        .iter()
        .any(|e| shadow_path.starts_with(&e.shadow_root));
    if !under_shadow_root {
        bail!(
            "BUG: shadow path {} is not under any known shadow root. Refusing to rm -rf.",
            shadow_path.display()
        );
    }

    fs::remove_dir_all(&shadow_path).with_context(|| {
        format!(
            "Failed to remove shadow directory {}",
            shadow_path.display()
        )
    })?;

    // Clean up empty parent directories.
    if let Some(sr) = config
        .filesystem
        .iter()
        .find(|e| shadow_path.starts_with(&e.shadow_root))
    {
        shadow::cleanup_empty_parents(&shadow_path, &sr.shadow_root)?;
    }

    eprintln!("Accepted. Shadow removed.");
    Ok(())
}

fn confirm_stdin() -> Result<bool> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read from stdin")?;
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

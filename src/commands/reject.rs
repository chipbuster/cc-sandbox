use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::devcontainer;
use crate::name;
use crate::shadow;

// SAFETY: reject never writes to the source directory. The only filesystem
// mutations are container removal and shadow deletion.

pub fn run(shadow_name: String, yes: bool) -> Result<()> {
    let config = config::load_or_create()?;
    let shadow_path = name::resolve_name(&shadow_name, &config)?;
    let meta = shadow::read_meta(&shadow_path)?;

    // Show change summary. If the source is gone, fall back to file count.
    if meta.source.is_dir() {
        let summary = shadow::change_summary(&shadow_path, &meta.source)?;
        eprintln!("Changes that will be discarded: {summary}");

        if !yes {
            eprint!("Discard {}? This cannot be undone. [y/N]: ", summary);
            if !confirm_stdin()? {
                eprintln!("Aborted.");
                return Ok(());
            }
        }
    } else {
        let file_count = count_files(&shadow_path);
        eprintln!(
            "Source {} no longer exists. Shadow contains ~{file_count} files.",
            meta.source.display()
        );

        if !yes {
            eprint!("Discard this shadow? This cannot be undone. [y/N]: ");
            if !confirm_stdin()? {
                eprintln!("Aborted.");
                return Ok(());
            }
        }
    }

    // Stop the container. Tolerate missing.
    devcontainer::stop_and_remove_container(&shadow_path)?;

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

    eprintln!("Rejected. Shadow removed.");
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

fn count_files(dir: &std::path::Path) -> usize {
    let output = Command::new("find")
        .args([&dir.display().to_string(), "-type", "f"])
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().count(),
        Err(_) => 0,
    }
}

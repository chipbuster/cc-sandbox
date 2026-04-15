use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Serialize, Deserialize)]
pub struct ShadowMeta {
    pub source: PathBuf,
    pub created_at: DateTime<Local>,
    pub name_suffix: String,
}

#[derive(Debug)]
pub struct ChangeSummary {
    pub modified: usize,
    pub added: usize,
    pub deleted: usize,
}

impl std::fmt::Display for ChangeSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} modified, {} added, {} deleted",
            self.modified, self.added, self.deleted
        )
    }
}

/// Compute the shadow directory path for a given source.
pub fn compute_shadow_path(
    shadow_root: &Path,
    mount_point: &Path,
    source: &Path,
    name: Option<&str>,
) -> Result<PathBuf> {
    let relative = source.strip_prefix(mount_point).with_context(|| {
        format!(
            "Source {} is not under mount point {}",
            source.display(),
            mount_point.display()
        )
    })?;

    if relative.as_os_str().is_empty() {
        bail!(
            "Cannot shadow the filesystem root {}. Use a subdirectory.",
            mount_point.display()
        );
    }

    let leaf = relative
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Source path has no final component"))?
        .to_string_lossy();

    let suffix = match name {
        Some(n) => n.to_string(),
        None => Local::now().format("%Y-%m-%d-%H%M").to_string(),
    };

    let parent = relative.parent().unwrap_or(Path::new(""));
    let shadow_leaf = format!("{leaf}-{suffix}");

    Ok(shadow_root.join(parent).join(shadow_leaf))
}

/// Create a reflink shadow copy of `source` at `shadow`.
/// On failure, cleans up the partial shadow directory.
pub fn create_shadow(source: &Path, shadow: &Path) -> Result<()> {
    if shadow.exists() {
        bail!(
            "Shadow path already exists: {}. Use a different --name or wait a minute for the timestamp to change.",
            shadow.display()
        );
    }

    if let Some(parent) = shadow.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create shadow parent directory {}",
                parent.display()
            )
        })?;
    }

    // Create the shadow directory itself so cp can copy contents into it.
    fs::create_dir(shadow)
        .with_context(|| format!("Failed to create shadow directory {}", shadow.display()))?;

    // SAFETY: --reflink=always, never =auto. This is safety rule #4.
    let source_arg = format!("{}/.", source.display());
    let shadow_arg = shadow.display().to_string();
    let status = Command::new("cp")
        .args(["--reflink=always", "-a", &source_arg, &shadow_arg])
        .status()
        .context("Failed to execute cp")?;

    if !status.success() {
        // Clean up the partial shadow before reporting the error.
        let _ = fs::remove_dir_all(shadow);
        bail!(
            "cp --reflink=always failed for {}. The source directory appears to be on a \
             filesystem that does not support reflink copies. Move the project to a Btrfs, \
             XFS-with-reflink, or ZFS 2.2+ volume.",
            source.display()
        );
    }

    Ok(())
}

/// Write the shadow metadata file.
pub fn write_meta(shadow: &Path, meta: &ShadowMeta) -> Result<()> {
    let path = shadow.join(config::meta_filename());
    let json = serde_json::to_string_pretty(meta).context("Failed to serialize shadow metadata")?;
    fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Read the shadow metadata file.
pub fn read_meta(shadow: &Path) -> Result<ShadowMeta> {
    let path = shadow.join(config::meta_filename());
    let contents =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let meta: ShadowMeta = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(meta)
}

/// Recursively enumerate all shadows under a shadow root.
/// Returns (shadow_dir_path, metadata) pairs.
pub fn enumerate_shadows(shadow_root: &Path) -> Result<Vec<(PathBuf, ShadowMeta)>> {
    let mut results = Vec::new();
    if !shadow_root.exists() {
        return Ok(results);
    }
    enumerate_recursive(shadow_root, &mut results)?;
    Ok(results)
}

fn enumerate_recursive(dir: &Path, results: &mut Vec<(PathBuf, ShadowMeta)>) -> Result<()> {
    let meta_path = dir.join(config::meta_filename());
    if meta_path.exists() {
        match read_meta(dir) {
            Ok(meta) => results.push((dir.to_path_buf(), meta)),
            Err(e) => {
                tracing::warn!("Skipping {}: {e:#}", dir.display());
            }
        }
        // Don't recurse into shadow directories.
        return Ok(());
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("Cannot read {}: {e}", dir.display());
            return Ok(());
        }
    };

    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            enumerate_recursive(&entry.path(), results)?;
        }
    }

    Ok(())
}

/// Remove empty parent directories between `shadow` (exclusive) and
/// `shadow_root` (exclusive). Stops at the first non-empty directory.
pub fn cleanup_empty_parents(shadow: &Path, shadow_root: &Path) -> Result<()> {
    let mut current = shadow.parent();
    while let Some(dir) = current {
        if dir == shadow_root || !dir.starts_with(shadow_root) {
            break;
        }
        // remove_dir fails on non-empty directories, which is what we want.
        if fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
    Ok(())
}

/// Compute a change summary between a shadow and its source using rsync dry-run.
pub fn change_summary(shadow: &Path, source: &Path) -> Result<ChangeSummary> {
    let exclude_flag = format!("--exclude={}", config::meta_filename());
    let output = Command::new("rsync")
        .args([
            "-ain",
            "--delete",
            &exclude_flag,
            &format!("{}/", shadow.display()),
            &format!("{}/", source.display()),
        ])
        .output()
        .context("Failed to execute rsync for change summary")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rsync dry-run failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut modified = 0usize;
    let mut added = 0usize;
    let mut deleted = 0usize;

    for line in stdout.lines() {
        if line.starts_with("*deleting") {
            deleted += 1;
        } else if line.starts_with(">f") {
            // >f+++++++++ means new file (all + flags = created).
            // >f with mixed flags = modified.
            let flags = &line[2..line.find(' ').unwrap_or(line.len())];
            if flags.chars().all(|c| c == '+') {
                added += 1;
            } else {
                modified += 1;
            }
        }
        // Skip directory changes (>d, cd, etc.)
    }

    Ok(ChangeSummary {
        modified,
        added,
        deleted,
    })
}

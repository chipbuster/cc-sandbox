use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{self, Config, FilesystemEntry};

/// Get the device ID (st_dev) for a path.
pub fn get_device_id(path: &Path) -> Result<u64> {
    use nix::sys::stat::stat;
    let s = stat(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    Ok(s.st_dev)
}

/// Unescape octal sequences in /proc/mounts mount points (e.g. \040 -> space).
fn unescape_mount_path(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Try to read 3 octal digits.
            let mut octal = String::with_capacity(3);
            for _ in 0..3 {
                if let Some(&next) = chars.clone().peekable().peek() {
                    if next.is_ascii_digit() {
                        octal.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(val) = u8::from_str_radix(&octal, 8) {
                    result.push(val as char);
                } else {
                    result.push('\\');
                    result.push_str(&octal);
                }
            } else {
                result.push('\\');
                result.push_str(&octal);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Find the mount point for a given path by parsing /proc/mounts.
pub fn find_mount_point(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", path.display()))?;

    let mounts = fs::read_to_string("/proc/mounts").context("Failed to read /proc/mounts")?;

    let mut best_mount: Option<PathBuf> = None;
    let mut best_len = 0;

    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount_path(fields[1]));
        if canonical.starts_with(&mount_point) {
            let len = mount_point.as_os_str().len();
            if len > best_len {
                best_len = len;
                best_mount = Some(mount_point);
            }
        }
    }

    best_mount
        .ok_or_else(|| anyhow::anyhow!("Could not determine mount point for {}", path.display()))
}

/// Look up (or interactively prompt for) the shadow root for the filesystem
/// containing `source`. Updates and saves the config if a new entry is added
/// or device_id has changed.
pub fn resolve_shadow_root(config: &mut Config, source: &Path) -> Result<PathBuf> {
    let mount_point = find_mount_point(source)?;
    let device_id = get_device_id(source)?;

    // Look for an existing entry by mount point.
    if let Some(entry) = config
        .filesystem
        .iter_mut()
        .find(|e| e.mount_point == mount_point)
    {
        if entry.device_id != device_id {
            eprintln!(
                "Warning: The device behind {} has changed (was {}, now {}). \
                 The shadow root may need updating.",
                mount_point.display(),
                entry.device_id,
                device_id
            );
            let shadow_root = prompt_shadow_root(&mount_point)?;
            entry.device_id = device_id;
            entry.shadow_root = shadow_root.clone();
            config::save(config)?;
            return Ok(shadow_root);
        }
        return Ok(entry.shadow_root.clone());
    }

    // No entry found; prompt.
    let shadow_root = prompt_shadow_root(&mount_point)?;
    config.filesystem.push(FilesystemEntry {
        mount_point,
        device_id,
        shadow_root: shadow_root.clone(),
    });
    config::save(config)?;
    Ok(shadow_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_plain_string() {
        assert_eq!(unescape_mount_path("/home/user"), "/home/user");
    }

    #[test]
    fn test_unescape_space() {
        // \040 is the octal escape for space (ASCII 32)
        assert_eq!(unescape_mount_path("/home/my\\040user"), "/home/my user");
    }

    #[test]
    fn test_unescape_tab() {
        // \011 is the octal escape for tab (ASCII 9)
        assert_eq!(unescape_mount_path("/mnt/my\\011dir"), "/mnt/my\tdir");
    }

    #[test]
    fn test_unescape_backslash() {
        // \134 is the octal escape for backslash (ASCII 92)
        assert_eq!(unescape_mount_path("/mnt/my\\134dir"), "/mnt/my\\dir");
    }

    #[test]
    fn test_unescape_multiple() {
        assert_eq!(
            unescape_mount_path("/data/my\\040cool\\040project"),
            "/data/my cool project"
        );
    }
}

fn prompt_shadow_root(mount_point: &Path) -> Result<PathBuf> {
    let default = mount_point.join(".cc-sandbox");
    eprint!(
        "Source is on filesystem mounted at {}. Where should shadows live? [{}]: ",
        mount_point.display(),
        default.display()
    );

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read from stdin")?;
    let input = input.trim();

    if input.is_empty() {
        Ok(default)
    } else {
        let path = PathBuf::from(input);
        if !path.is_absolute() {
            bail!(
                "Shadow root must be an absolute path, got: {}",
                path.display()
            );
        }
        Ok(path)
    }
}

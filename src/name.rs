use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::config::Config;
use crate::shadow;

/// Resolve a user-supplied name to a shadow directory path.
///
/// If the name contains `/`, treat it as a relative path under each shadow root.
/// Otherwise, search all shadow leaves for an exact match.
///
/// Returns an error on zero or ambiguous matches.
pub fn resolve_name(name: &str, config: &Config) -> Result<PathBuf> {
    let mut matches = Vec::new();

    if name.contains('/') {
        // Path-relative lookup: check each shadow root.
        for entry in &config.filesystem {
            let candidate = entry.shadow_root.join(name);
            let meta_path = candidate.join(crate::config::meta_filename());
            if meta_path.exists() {
                matches.push(candidate);
            }
        }
    } else {
        // Leaf-name lookup: enumerate all shadows, match on leaf.
        for entry in &config.filesystem {
            let shadows = shadow::enumerate_shadows(&entry.shadow_root)?;
            for (path, _meta) in shadows {
                if let Some(leaf) = path.file_name()
                    && leaf.to_string_lossy() == name
                {
                    matches.push(path);
                }
            }
        }
    }

    match matches.len() {
        0 => {
            // Suggest close matches (substring check).
            let mut suggestions = Vec::new();
            for entry in &config.filesystem {
                let shadows = shadow::enumerate_shadows(&entry.shadow_root)?;
                for (path, _meta) in shadows {
                    if let Some(leaf) = path.file_name() {
                        let leaf_str = leaf.to_string_lossy();
                        if leaf_str.contains(name) {
                            suggestions.push(leaf_str.to_string());
                        }
                    }
                }
            }

            if suggestions.is_empty() {
                bail!("No shadow named '{name}' found. Run `cc-sandbox list` to see all shadows.");
            } else {
                let list = suggestions.join("\n  ");
                bail!("No shadow named '{name}' found. Did you mean one of these?\n  {list}");
            }
        }
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let list: Vec<String> = matches
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect();
            bail!(
                "Ambiguous name '{name}' matches {} shadows:\n{}\n\
                 Use more of the path or the full name including the timestamp to disambiguate.",
                matches.len(),
                list.join("\n")
            );
        }
    }
}

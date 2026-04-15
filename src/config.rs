use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const META_FILENAME: &str = ".cc-sandbox-meta.json";

pub fn meta_filename() -> &'static str {
    META_FILENAME
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub filesystem: Vec<FilesystemEntry>,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub shell: ShellConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FilesystemEntry {
    pub mount_point: PathBuf,
    pub device_id: u64,
    pub shadow_root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    pub command: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: vec![
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
            ],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShellConfig {
    pub command: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: vec!["bash".to_string()],
        }
    }
}

/// Returns the path to the config file, respecting $XDG_CONFIG_HOME.
pub fn config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("cc-sandbox").join("config.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("cc-sandbox")
            .join("config.toml")
    } else {
        // Fallback; unlikely on Linux but handle it.
        PathBuf::from("/tmp/cc-sandbox/config.toml")
    }
}

/// Load the config from disk, or create a default one if it doesn't exist.
pub fn load_or_create() -> Result<Config> {
    let path = config_path();
    if path.exists() {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    } else {
        let config = Config::default();
        save(&config)?;
        Ok(config)
    }
}

/// Save the config atomically: write to a temp file then rename.
pub fn save(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(config).context("Failed to serialize config")?;
    let tmp_path = path.with_extension("toml.tmp");
    fs::write(&tmp_path, &contents)
        .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "Failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// Check that a directory contains a devcontainer configuration.
pub fn has_devcontainer(path: &Path) -> bool {
    path.join(".devcontainer")
        .join("devcontainer.json")
        .exists()
        || path.join(".devcontainer.json").exists()
}

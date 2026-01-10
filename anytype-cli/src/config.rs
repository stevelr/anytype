use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CliConfig {
    pub url: Option<String>,
    pub keystore: Option<String>,
    pub default_space: Option<String>,
}

impl CliConfig {
    pub fn path() -> Result<PathBuf> {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        Ok(base.join("anytype").join("cli.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let config = serde_json::from_str(&data).context("parse cli config")?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("serialize cli config")?;
        fs::write(&path, data).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn reset() -> Result<()> {
        let path = Self::path()?;
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(())
    }
}

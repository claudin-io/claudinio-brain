//! Brain catalogue and defaults.
//!
//! The config holds *paths only*. No fact data ever lives here, so reading a
//! config can never leak the contents of any brain.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Name of the brain to use when no selector is given. Must appear in `brains`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_brain: Option<String>,

    /// Overrides where `--global` looks. Defaults to the platform data dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,

    /// A catalogue of shortcuts. Naming two brains here does NOT connect them.
    ///
    /// `BTreeMap`, not `HashMap`: iteration order feeds user-visible output and
    /// error messages, and must not vary between runs.
    #[serde(default)]
    pub brains: BTreeMap<String, PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("malformed config at {path}: {source}")]
pub struct BadConfig {
    pub path: PathBuf,
    #[source]
    pub source: toml::de::Error,
}

impl Config {
    /// Reads a config file. A missing file is an empty config, not an error --
    /// but a present-and-broken one is loud.
    pub fn load(path: &Path) -> Result<Self, BadConfig> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(Self::default());
        };
        toml::from_str(&text).map_err(|source| BadConfig {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn brain_names(&self) -> Vec<String> {
        self.brains.keys().cloned().collect()
    }

    /// Writes the config back, preserving every field it does not own.
    ///
    /// Serializing the whole struct is what keeps an existing `default_brain` or
    /// an unrelated catalogue entry from being dropped when one name is added.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }

    /// Registers a brain under `name`.
    ///
    /// Re-registering the same name at the same path is a no-op rather than an
    /// error, so re-running `init --name` after a failure is safe. Pointing an
    /// existing name somewhere new is refused: silently repointing a catalogue
    /// entry is how a later command would open the wrong company's brain.
    pub fn register(&mut self, name: &str, path: &Path) -> Result<(), NameTaken> {
        match self.brains.get(name) {
            Some(existing) if existing != path => Err(NameTaken {
                name: name.to_string(),
                existing: existing.clone(),
            }),
            _ => {
                self.brains.insert(name.to_string(), path.to_path_buf());
                Ok(())
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the name {name:?} already refers to {}; pick another name", existing.display())]
pub struct NameTaken {
    pub name: String,
    pub existing: PathBuf,
}

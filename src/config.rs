use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Name of the configuration file, looked up in the repository root.
pub const FILE_NAME: &str = ".mito.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Directory holding mito's state, relative to the repository root.
    /// Defaults to `.mito`.
    pub dir: Option<PathBuf>,
    #[serde(default)]
    pub base: BaseConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct BaseConfig {
    /// Revision the base worktree follows. Defaults to `origin/HEAD`, falling
    /// back to the main worktree's `HEAD`.
    pub track: Option<String>,
}

impl Config {
    /// Loads `.mito.toml` from `root`, or the defaults when it is absent.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_uses_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.dir, None);
        assert_eq!(config.base.track, None);
    }

    #[test]
    fn parses_all_fields() {
        let config = Config::parse(
            r#"
            dir = "../mito-state"

            [base]
            track = "origin/main"
            "#,
        )
        .unwrap();
        assert_eq!(config.dir, Some(PathBuf::from("../mito-state")));
        assert_eq!(config.base.track.as_deref(), Some("origin/main"));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Config::parse("[base]\ntraack = \"main\"\n").is_err());
    }
}

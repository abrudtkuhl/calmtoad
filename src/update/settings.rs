//! A tiny, versioned settings file: whether to check automatically, when we last did,
//! and which version the user asked not to be reminded about.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Error, Result, now_seconds, updates_dir};

const SETTINGS_FILE: &str = "updates.json";
const CHECK_INTERVAL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default = "default_true")]
    pub auto_check: bool,
    #[serde(default)]
    pub last_checked: Option<u64>,
    #[serde(default)]
    pub skipped_version: Option<String>,
}

fn default_schema() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: default_schema(),
            auto_check: true,
            last_checked: None,
            skipped_version: None,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(&updates_dir())
    }

    pub fn load_from(dir: &Path) -> Self {
        let Ok(bytes) = fs::read(path_in(dir)) else {
            return Self::default();
        };
        let Ok(settings) = serde_json::from_slice::<Settings>(&bytes) else {
            return Self::default();
        };
        if settings.schema != default_schema() {
            return Self::default();
        }
        settings
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&updates_dir())
    }

    pub fn save_to(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let path = path_in(dir);
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        // Windows will not rename over an existing file.
        let _ = fs::remove_file(&path);
        fs::rename(temp, path)?;
        Ok(())
    }

    pub fn is_due(&self) -> bool {
        if !self.auto_check {
            return false;
        }
        match self.last_checked {
            None => true,
            Some(last) => now_seconds().saturating_sub(last) >= CHECK_INTERVAL_SECONDS,
        }
    }

    pub fn mark_checked(&mut self) {
        self.last_checked = Some(now_seconds());
    }

    pub fn skip(&mut self, version: &str) -> Result<()> {
        self.skipped_version = Some(version.to_string());
        self.save()
    }

    pub fn clear_skip(&mut self) -> Result<()> {
        self.skipped_version = None;
        self.save()
    }

    pub fn skip_matches(&self, version: &str) -> bool {
        self.skipped_version.as_deref() == Some(version)
    }
}

fn path_in(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}

/// Ensure the updater's working directory exists before a journal or helper is written.
pub(crate) fn ensure_dir() -> Result<PathBuf> {
    let dir = updates_dir();
    fs::create_dir_all(&dir).map_err(Error::from)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_skip_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.mark_checked();
        settings.skip("0.2.0").unwrap();
        settings.save_to(dir.path()).unwrap();

        let loaded = Settings::load_from(dir.path());
        assert!(loaded.skip_matches("0.2.0"));
        assert!(!loaded.skip_matches("0.3.0"));
        assert!(loaded.last_checked.is_some());
        assert!(!loaded.is_due());
    }

    #[test]
    fn a_missing_or_corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Settings::load_from(dir.path()).auto_check);

        fs::write(path_in(dir.path()), b"not json").unwrap();
        assert!(Settings::load_from(dir.path()).auto_check);
    }
}

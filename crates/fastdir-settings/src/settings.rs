//! Small persisted config (`%APPDATA%\FastDir\settings.json`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Check GitHub for a new version every time FastDir starts.
    pub auto_check_updates: bool,
    /// Version the user chose to skip ("Skip this version").
    pub skipped_version: Option<String>,
    /// Version that ran last, used to show "Updated to vX" after an update.
    pub last_run_version: Option<String>,
    /// Show `<Free space>` in the treemap (F6, off by default like WinDirStat).
    pub show_free_space: bool,
    /// Show `<Unknown>` in the treemap (F7, off by default like WinDirStat).
    pub show_unknown: bool,
    /// Size treemap cells by allocated (on-disk) bytes instead of logical bytes.
    pub treemap_disk_size: bool,
    /// Show finished top-level folders and a treemap preview while scanning.
    pub show_partial_results: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_check_updates: true,
            skipped_version: None,
            last_run_version: None,
            show_free_space: false,
            show_unknown: false,
            treemap_disk_size: false,
            show_partial_results: true,
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("FastDir").join("settings.json"))
}

impl Settings {
    /// Missing or unreadable files give the defaults.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best effort: failing to persist a setting must never break the app.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_fields_missing() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.auto_check_updates);
        assert!(s.skipped_version.is_none());
        assert!(!s.show_free_space);
        assert!(!s.show_unknown);
        assert!(!s.treemap_disk_size);
        assert!(s.show_partial_results);
    }

    #[test]
    fn roundtrip() {
        let s = Settings {
            auto_check_updates: false,
            skipped_version: Some("0.2.0".into()),
            last_run_version: Some("0.1.0".into()),
            show_free_space: true,
            show_unknown: true,
            treemap_disk_size: true,
            show_partial_results: false,
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.auto_check_updates);
        assert_eq!(back.skipped_version.as_deref(), Some("0.2.0"));
        assert!(back.show_free_space);
        assert!(back.show_unknown);
        assert!(back.treemap_disk_size);
        assert!(!back.show_partial_results);
    }
}

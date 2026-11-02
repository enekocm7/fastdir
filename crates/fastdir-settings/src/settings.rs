//! Small persisted config (`%APPDATA%\FastDir\config.toml`).
//!
//! Everything the app remembers between launches lives here: updater state,
//! the theme, the window geometry, the panel split, the sort order and the
//! last scanned path. Settings written by older versions to `settings.json`
//! are migrated on first load.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which theme the user picked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Follow the Windows light / dark setting.
    #[default]
    System,
    Light,
    Dark,
}

/// The column the directory list is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    Name,
    #[default]
    Size,
    Disk,
    Items,
    Files,
    Dirs,
    Modified,
}

/// Sort column and direction remembered across launches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSettings {
    pub column: SortColumn,
    pub descending: bool,
}

impl Default for SortSettings {
    fn default() -> Self {
        Self {
            column: SortColumn::Size,
            descending: true,
        }
    }
}

/// Window position, size and maximized state.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            width: 1440.,
            height: 920.,
            maximized: false,
        }
    }
}

/// Sizes of the resizable panels, in logical pixels.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelSizes {
    /// Top section vs. treemap (vertical split).
    pub main: Vec<f32>,
    /// Directory list vs. extension list (horizontal split).
    pub top: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// How many files the "Top files" view lists.
    #[serde(default = "default_top_files_limit")]
    pub top_files_limit: usize,
    /// Show finished top-level folders and a treemap preview while scanning.
    pub show_partial_results: bool,
    /// Light, dark, or follow Windows.
    pub theme: ThemePreference,
    /// Column and direction of the directory list.
    pub sort: SortSettings,
    /// Last window geometry, restored on the next launch.
    pub window: Option<WindowState>,
    /// Last divider positions.
    pub panels: PanelSizes,
    /// Root of the last scan, remembered for convenience.
    pub last_scan_path: Option<String>,
}

fn default_top_files_limit() -> usize {
    1000
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
            top_files_limit: default_top_files_limit(),
            show_partial_results: true,
            theme: ThemePreference::System,
            sort: SortSettings::default(),
            window: None,
            panels: PanelSizes::default(),
            last_scan_path: None,
        }
    }
}

/// The config file currently in use.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("FastDir").join("config.toml"))
}

/// Where versions before the TOML config stored their updater settings.
fn legacy_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("FastDir").join("settings.json"))
}

impl Settings {
    /// Missing or unreadable files give the defaults. A JSON `settings.json`
    /// from an older version is migrated to `config.toml` on first load.
    pub fn load() -> Self {
        if let Some(settings) = config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str::<Self>(&s).ok())
        {
            return settings;
        }
        if let Some(settings) = legacy_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
        {
            settings.save();
            return settings;
        }
        Self::default()
    }

    /// Best effort: failing to persist a setting must never break the app.
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let tmp = path.with_extension("toml.tmp");
            if std::fs::write(&tmp, text).is_ok() {
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
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.auto_check_updates);
        assert!(s.skipped_version.is_none());
        assert!(!s.show_free_space);
        assert!(!s.show_unknown);
        assert!(!s.treemap_disk_size);
        assert_eq!(s.top_files_limit, 1000);
        assert!(s.show_partial_results);
        assert_eq!(s.theme, ThemePreference::System);
        assert_eq!(s.sort, SortSettings::default());
        assert!(s.window.is_none());
        assert!(s.panels.main.is_empty());
        assert!(s.last_scan_path.is_none());
    }

    #[test]
    fn nested_defaults_when_only_some_fields_are_given() {
        let s: Settings = toml::from_str(
            r#"
            theme = "dark"

            [sort]
            column = "name"
            "#,
        )
        .unwrap();
        assert_eq!(s.theme, ThemePreference::Dark);
        assert_eq!(s.sort.column, SortColumn::Name);
        assert!(s.sort.descending, "the missing direction keeps its default");
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
            top_files_limit: 250,
            show_partial_results: false,
            theme: ThemePreference::Light,
            sort: SortSettings {
                column: SortColumn::Modified,
                descending: false,
            },
            window: Some(WindowState {
                x: 10.5,
                y: 20.5,
                width: 1280.,
                height: 720.,
                maximized: true,
            }),
            panels: PanelSizes {
                main: vec![420., 500.],
                top: vec![700., 330.],
            },
            last_scan_path: Some(r"C:\Users".into()),
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn legacy_json_still_deserializes() {
        let json = r#"{
            "auto_check_updates": false,
            "skipped_version": "0.2.0",
            "last_run_version": "0.1.0",
            "show_free_space": true,
            "show_unknown": true,
            "treemap_disk_size": true
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.auto_check_updates);
        assert_eq!(s.skipped_version.as_deref(), Some("0.2.0"));
        assert!(s.show_free_space);
        assert_eq!(s.theme, ThemePreference::System);
        assert_eq!(s.sort, SortSettings::default());
        assert_eq!(s.top_files_limit, 1000);
    }
}

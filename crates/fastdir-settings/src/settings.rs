//! Small persisted config (`%APPDATA%\FastDir\config.toml` on Windows,
//! `~/.config/FastDir/config.toml` elsewhere).
//!
//! Everything the app remembers between launches lives here, grouped into
//! sections so the file stays readable as it grows: what to scan, how the
//! treemap and the file list look, which file operations confirm, what
//! happens on startup, the updater state, and the interface language.
//! versions (flat `config.toml` keys or `settings.json`) are migrated on
//! first load, so every field keeps `#[serde(default)]`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    Owner,
    /// Size change against a saved scan (comparison view only).
    Delta,
    /// Change status (comparison view only).
    Status,
}

/// How folder sizes are displayed in the list, the status bar and tooltips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeUnit {
    /// 1024-based (the Windows convention).
    #[default]
    Binary,
    /// 1000-based (SI, like macOS and drive manufacturers).
    Decimal,
}

/// What the subtree `%` column is relative to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PercentBase {
    /// The parent folder (WinDirStat style).
    #[default]
    Parent,
    /// The whole scan root.
    Total,
}

/// Absolute timestamps vs. relative ones (`3 days ago`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DateFormat {
    #[default]
    Absolute,
    Relative,
}

/// Which treemap palette maps extension ranks to colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TreemapPalette {
    /// WinDirStat-style rainbow.
    #[default]
    Default,
    /// Friendlier to red-green colour blindness.
    ColorBlind,
    /// Greyscale ramp.
    Mono,
}

/// What decides a treemap leaf's colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TreemapColorMode {
    /// Colour by file extension (the extension list is the legend).
    #[default]
    Extension,
    /// Colour by age: new files are warm, old files are cold.
    Age,
    /// Colour by file-type category (video, images, archives, ...).
    Category,
    /// Colour by size change against a saved scan (green shrank, red grew).
    Change,
}

/// How the extension panel groups its rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtGrouping {
    /// One row per extension.
    #[default]
    Extension,
    /// Expandable category rows, each listing its extensions.
    Category,
    /// Per-user totals (only offered when the scan collected owners).
    Owner,
}

/// What the Delete key does (Shift+Delete always does the other).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeleteAction {
    /// Move to the Recycle Bin.
    #[default]
    Recycle,
    /// Delete permanently.
    Permanent,
}

/// What FastDir shows on startup (a path argument always wins).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartupMode {
    /// The drive overview.
    #[default]
    Drives,
    /// Re-scan the last scanned path.
    Last,
    /// A blank home screen with just a "Scan a folder" button.
    Empty,
}

/// Which releases the update check looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Finished releases only.
    #[default]
    Stable,
    /// Also offer pre-releases.
    Prerelease,
}

/// How often the automatic update check runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateFrequency {
    /// On every launch.
    #[default]
    Start,
    /// At most once a day.
    Daily,
    /// At most once a week.
    Weekly,
}

/// What a context-menu action applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionAppliesTo {
    /// Only folders.
    Dir,
    /// Only files.
    File,
    /// Both.
    #[default]
    Both,
}

/// One user-defined command in the context menu's *Run* submenu
/// (`[[actions]]` in `config.toml`).
///
/// Placeholders in `command`, `args` and `working_dir`: `{path}`, `{name}`,
/// `{parent}` and `{ext}`. `name` and `command` are required in practice; an
/// action missing either is never shown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionDef {
    /// Label shown in the menu and in confirmations.
    pub name: String,
    /// Program to run. With `shell = false` (the default) it is spawned
    /// directly; with `shell = true` the whole line goes through
    /// `cmd /C` / `sh -c`.
    pub command: String,
    /// Arguments, one entry each.
    pub args: Vec<String>,
    /// Folder to run in; empty means the item (a file's folder for files).
    pub working_dir: String,
    /// Folders, files or both.
    pub applies_to: ActionAppliesTo,
    /// Optional extra condition: a name glob (`*.zip`) or, for folders, a
    /// file the folder must contain (`Cargo.toml`, `.git`).
    #[serde(rename = "match")]
    pub match_pattern: Option<String>,
    /// Ask before running, showing the exact command line.
    pub confirm: bool,
    /// Re-scan the target folder after the command finishes.
    pub refresh_after: bool,
    /// Show stdout / stderr in a dialog afterwards.
    pub show_output: bool,
    /// Run through the platform shell.
    pub shell: bool,
    /// Keyboard shortcut: `1`..`9` binds Ctrl+1..Ctrl+9.
    pub shortcut: Option<u8>,
    /// Disabled actions stay in the config but are not offered in the menu.
    pub enabled: bool,
}

impl Default for ActionDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            working_dir: String::new(),
            applies_to: ActionAppliesTo::Both,
            match_pattern: None,
            confirm: true,
            refresh_after: false,
            show_output: false,
            shell: false,
            shortcut: None,
            enabled: true,
        }
    }
}

impl ActionDef {
    /// Ready-made examples for the Settings *Add preset* menu and for fresh
    /// configs. They start disabled so nothing runs by accident.
    pub fn presets() -> Vec<Self> {
        let terminal = if cfg!(windows) {
            ActionDef {
                name: "Open terminal here".into(),
                command: "cmd".into(),
                args: ["/C", "start", "", "cmd", "/K", "cd", "/D", "{path}"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                working_dir: "{path}".into(),
                applies_to: ActionAppliesTo::Dir,
                ..ActionDef::default()
            }
        } else {
            ActionDef {
                name: "Open terminal here".into(),
                command: "x-terminal-emulator".into(),
                args: Vec::new(),
                working_dir: "{path}".into(),
                applies_to: ActionAppliesTo::Dir,
                ..ActionDef::default()
            }
        };
        vec![
            ActionDef {
                enabled: false,
                ..terminal
            },
            ActionDef {
                name: "Open in VS Code".into(),
                command: "code".into(),
                args: vec!["{path}".into()],
                confirm: false,
                enabled: false,
                ..ActionDef::default()
            },
            ActionDef {
                name: "cargo clean".into(),
                command: "cargo".into(),
                args: vec!["clean".into()],
                working_dir: "{path}".into(),
                applies_to: ActionAppliesTo::Dir,
                match_pattern: Some("Cargo.toml".into()),
                refresh_after: true,
                enabled: false,
                ..ActionDef::default()
            },
            ActionDef {
                name: "git gc".into(),
                command: "git".into(),
                args: vec!["gc".into()],
                working_dir: "{path}".into(),
                applies_to: ActionAppliesTo::Dir,
                match_pattern: Some(".git".into()),
                refresh_after: true,
                enabled: false,
                ..ActionDef::default()
            },
        ]
    }

    /// Whether the action is complete enough to offer.
    pub fn is_usable(&self) -> bool {
        self.enabled && !self.name.trim().is_empty() && !self.command.trim().is_empty()
    }
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

/// What to scan and how: exclusions, hidden files, junctions, threads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanSettings {
    /// Folder/file names to skip, matched case-insensitively against each
    /// entry's name (`*` and `?` wildcards), e.g. `$Recycle.Bin`, `*.tmp`.
    pub excluded: Vec<String>,
    /// Skip hidden and system entries (dot-files on Unix).
    pub skip_hidden_system: bool,
    /// Descend into junctions / symlinked folders. Off by default: following
    /// them can double-count files, and loops are only entered once.
    pub follow_junctions: bool,
    /// Descend into mount points on other volumes. Off by default, so a scan
    /// stays on the volume it started on.
    pub cross_volumes: bool,
    /// Scan threads, or `None` for automatic. A fixed number keeps a slow
    /// HDD or network share responsive.
    pub thread_count: Option<u16>,
    /// Show finished top-level folders and a treemap preview while scanning.
    pub show_partial_results: bool,
    /// Record the owning account of every entry. Off by default because on
    /// Windows it needs one extra security query per file.
    pub collect_owners: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            excluded: Vec::new(),
            skip_hidden_system: false,
            follow_junctions: false,
            cross_volumes: false,
            thread_count: None,
            show_partial_results: true,
            collect_owners: false,
        }
    }
}

/// How the treemap looks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TreemapSettings {
    /// Show `<Free space>` in the treemap (F6, off by default like WinDirStat).
    pub show_free_space: bool,
    /// Show `<Unknown>` in the treemap (F7, off by default like WinDirStat).
    pub show_unknown: bool,
    /// Size treemap cells by allocated (on-disk) bytes instead of logical bytes.
    pub disk_size: bool,
    /// Which palette maps extension ranks to colours.
    pub palette: TreemapPalette,
    /// Colour by extension or by age.
    pub color_mode: TreemapColorMode,
    /// Cushion (pillow) shading, or flat rectangles.
    pub cushion: bool,
    /// Ridge height multiplier for the cushion shading (0.0..=2.0).
    pub cushion_strength: f32,
    /// Leave a 1px gap between cells.
    pub borders: bool,
    /// Stop subdividing below this cell size (device px).
    pub min_cell: f32,
    /// Show the treemap at all (off gives a list-only layout for small
    /// screens or very large scans).
    pub show: bool,
}

impl Default for TreemapSettings {
    fn default() -> Self {
        Self {
            show_free_space: false,
            show_unknown: false,
            disk_size: false,
            palette: TreemapPalette::Default,
            color_mode: TreemapColorMode::Extension,
            cushion: true,
            cushion_strength: 1.0,
            borders: false,
            min_cell: 1.0,
            show: true,
        }
    }
}

/// How the directory list looks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListSettings {
    /// Binary (1024-based) vs. decimal (1000-based) size units.
    pub size_unit: SizeUnit,
    /// Subtree `%` relative to the parent folder or the whole scan.
    pub percent_base: PercentBase,
    /// Absolute timestamps vs. relative ones (`3 days ago`).
    pub date_format: DateFormat,
    /// Tint the "Last change" column on the same new-to-old age scale as the
    /// treemap.
    pub age_tint: bool,
    /// Column and direction for new scans.
    pub sort: SortSettings,
    /// How many files the "Top files" view lists.
    #[serde(default = "default_top_files_limit")]
    pub top_files_limit: usize,
    /// Visible columns.
    pub show_items: bool,
    pub show_files: bool,
    pub show_folders: bool,
    pub show_modified: bool,
    pub show_disk: bool,
    /// Show the Owner column (only meaningful when the scan collected owners).
    pub show_owner: bool,
    /// Hide folders that contain no data (no files and zero size, recursively).
    pub hide_empty_folders: bool,
    /// Whether the extension panel lists extensions or file-type categories.
    pub ext_grouping: ExtGrouping,
}

/// Default number of rows in the "Top files" view.
fn default_top_files_limit() -> usize {
    1000
}

impl Default for ListSettings {
    fn default() -> Self {
        Self {
            size_unit: SizeUnit::Binary,
            percent_base: PercentBase::Parent,
            date_format: DateFormat::Absolute,
            age_tint: false,
            sort: SortSettings::default(),
            top_files_limit: default_top_files_limit(),
            show_items: true,
            show_files: true,
            show_folders: true,
            show_modified: true,
            show_disk: true,
            show_owner: true,
            hide_empty_folders: false,
            ext_grouping: ExtGrouping::Extension,
        }
    }
}

/// Confirmations and defaults for file operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpSettings {
    /// Confirm before moving to the Recycle Bin.
    pub confirm_recycle: bool,
    /// Confirm before deleting permanently.
    pub confirm_permanent: bool,
    /// Confirm before moving files.
    pub confirm_move: bool,
    /// What the Delete key does (Shift+Delete does the other).
    pub default_delete: DeleteAction,
    /// Keep the current cursor and scroll position when a delete finishes
    /// instead of moving to the deleted item's folder. Off by default, which
    /// keeps the previous "show me what was deleted" behavior.
    pub keep_selection_after_delete: bool,
    /// Where the last move went, remembered for convenience.
    pub last_move_dest: Option<String>,
}

impl Default for OpSettings {
    fn default() -> Self {
        Self {
            confirm_recycle: true,
            confirm_permanent: true,
            confirm_move: true,
            default_delete: DeleteAction::Recycle,
            keep_selection_after_delete: false,
            last_move_dest: None,
        }
    }
}

/// What FastDir shows on startup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartupSettings {
    pub mode: StartupMode,
    /// Root of the last scan, remembered for convenience (and re-scanned
    /// when `mode` is `Last`).
    pub last_path: Option<String>,
}

impl Default for StartupSettings {
    fn default() -> Self {
        Self {
            mode: StartupMode::Drives,
            last_path: None,
        }
    }
}

/// Updater state and preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Run the update check automatically (gated by `frequency`).
    pub auto_check: bool,
    /// Stable releases only, or pre-releases too.
    pub channel: UpdateChannel,
    /// How often the automatic check runs.
    pub frequency: UpdateFrequency,
    /// Unix timestamp of the last check (any check, manual or automatic).
    pub last_check: Option<i64>,
    /// Version the user chose to skip ("Skip this version").
    pub skipped_version: Option<String>,
    /// Version that ran last, used to show "Updated to vX" after an update.
    pub last_run_version: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            auto_check: true,
            channel: UpdateChannel::Stable,
            frequency: UpdateFrequency::Start,
            last_check: None,
            skipped_version: None,
            last_run_version: None,
        }
    }
}

/// Interface language (`[locale] language`). `system` follows the Windows
/// display language; any other value is a language code such as `en` or
/// `es` (region variants like `es-MX` fall back to their base language,
/// unknown values to English). New configs default to `system` so the
/// first launch matches Windows; existing files keep loading because the
/// field (and the whole section) deserializes with a default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocaleSettings {
    /// Language code (`en`, `es`, …) or `system` to follow Windows.
    /// Missing values re-detect the OS language so old files keep working.
    #[serde(default = "default_language")]
    pub language: String,
}

/// The default for a fresh config: the OS display language resolved to a
/// supported code, with an English fallback.
fn default_language() -> String {
    fastdir_locale::system_language().to_string()
}

impl Default for LocaleSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
        }
    }
}

/// Light, dark, or follow Windows; plus the interface scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: ThemePreference,
    /// Interface scale factor (1.0 = 100%), separate from Windows DPI.
    /// Applied to the window's rem size, so text and rem-based layout grow.
    pub ui_scale: f32,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            ui_scale: 1.0,
        }
    }
}

/// Explorer integration and multi-instance behaviour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IntegrationSettings {
    /// "Analyze with FastDir" in the Explorer folder/drive context menu
    /// (Windows only). Adopted from the registry on startup; toggling writes
    /// the per-user keys, mirroring what the installer creates.
    pub context_menu: bool,
    /// Second launches forward their folder to the open window and exit
    /// instead of starting a new process. Takes effect on next launch.
    pub single_instance: bool,
}

impl Default for IntegrationSettings {
    fn default() -> Self {
        Self {
            context_menu: true,
            single_instance: false,
        }
    }
}

/// Disabled example actions for fresh configs.
fn default_actions() -> Vec<ActionDef> {
    ActionDef::presets()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub scan: ScanSettings,
    pub treemap: TreemapSettings,
    pub list: ListSettings,
    pub ops: OpSettings,
    pub startup: StartupSettings,
    pub updates: UpdateSettings,
    pub appearance: AppearanceSettings,
    /// Explorer context menu and single-instance behaviour.
    pub integration: IntegrationSettings,
    /// Interface language.
    pub locale: LocaleSettings,
    /// User overrides for file-type categories (`[categories]`): category id
    /// (`video`, `images`, ...) to extension names. Overrides both move
    /// built-in extensions and add new ones.
    pub categories: BTreeMap<String, Vec<String>>,
    /// User-defined context-menu commands (`[[actions]]`).
    #[serde(default = "default_actions")]
    pub actions: Vec<ActionDef>,
    /// Last window geometry, restored on the next launch.
    pub window: Option<WindowState>,
    /// Last divider positions.
    pub panels: PanelSizes,
    /// Kept so very old files still parse; use `list.sort` instead.
    pub sort: SortSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scan: ScanSettings::default(),
            treemap: TreemapSettings::default(),
            list: ListSettings::default(),
            ops: OpSettings::default(),
            startup: StartupSettings::default(),
            updates: UpdateSettings::default(),
            appearance: AppearanceSettings::default(),
            integration: IntegrationSettings::default(),
            locale: LocaleSettings::default(),
            categories: BTreeMap::new(),
            actions: default_actions(),
            window: None,
            panels: PanelSizes::default(),
            sort: SortSettings::default(),
        }
    }
}

/// The config file currently in use: `%APPDATA%\FastDir\config.toml` on
/// Windows, `$XDG_CONFIG_HOME/FastDir/config.toml` (or
/// `~/.config/FastDir/config.toml`) elsewhere.
pub fn config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(base).join("FastDir").join("config.toml"))
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            if !dir.is_empty() {
                return Some(PathBuf::from(dir).join("FastDir").join("config.toml"));
            }
        }
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("FastDir")
                .join("config.toml"),
        )
    }
}

/// Where versions before the TOML config stored their updater settings:
/// `settings.json` next to the current config file.
fn legacy_path() -> Option<PathBuf> {
    config_path().map(|p| p.with_file_name("settings.json"))
}

/// The flat keys written before settings were grouped into sections.
/// Missing keys fall back to the old defaults (e.g. auto-check on), not to
/// `false`, so a partial legacy file keeps behaving as it used to.
#[derive(Deserialize)]
#[serde(default)]
struct LegacySettings {
    auto_check_updates: bool,
    skipped_version: Option<String>,
    last_run_version: Option<String>,
    show_free_space: bool,
    show_unknown: bool,
    treemap_disk_size: bool,
    show_partial_results: bool,
    #[serde(default = "default_top_files_limit")]
    top_files_limit: usize,
    theme: ThemePreference,
    sort: SortSettings,
    window: Option<WindowState>,
    panels: PanelSizes,
    last_scan_path: Option<String>,
}

impl Default for LegacySettings {
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

impl From<LegacySettings> for Settings {
    fn from(old: LegacySettings) -> Self {
        Self {
            scan: ScanSettings {
                show_partial_results: old.show_partial_results,
                ..ScanSettings::default()
            },
            treemap: TreemapSettings {
                show_free_space: old.show_free_space,
                show_unknown: old.show_unknown,
                disk_size: old.treemap_disk_size,
                ..TreemapSettings::default()
            },
            list: ListSettings {
                sort: old.sort,
                top_files_limit: old.top_files_limit,
                ..ListSettings::default()
            },
            ops: OpSettings::default(),
            startup: StartupSettings {
                last_path: old.last_scan_path,
                ..StartupSettings::default()
            },
            updates: UpdateSettings {
                auto_check: old.auto_check_updates,
                skipped_version: old.skipped_version,
                last_run_version: old.last_run_version,
                ..UpdateSettings::default()
            },
            appearance: AppearanceSettings {
                theme: old.theme,
                ..AppearanceSettings::default()
            },
            integration: IntegrationSettings::default(),
            locale: LocaleSettings::default(),
            categories: BTreeMap::new(),
            actions: default_actions(),
            window: old.window,
            panels: old.panels,
            // Written before the sort lived in `[list]`; keep both in step.
            sort: old.sort,
        }
    }
}

/// Top-level section names of the current format. A file containing any of
/// them is parsed as [`Settings`]; anything else falls back to the legacy
/// flat keys so old files keep loading.
const SECTION_KEYS: [&str; 11] = [
    "scan",
    "treemap",
    "list",
    "ops",
    "startup",
    "updates",
    "appearance",
    "integration",
    "locale",
    "categories",
    "actions",
];

fn parse_current(text: &str) -> Option<Settings> {
    let table: toml::Table = toml::from_str(text).ok()?;
    if !SECTION_KEYS.iter().any(|k| table.contains_key(*k)) {
        return None;
    }
    toml::from_str(text).ok()
}

fn parse_legacy_toml(text: &str) -> Option<Settings> {
    toml::from_str::<LegacySettings>(text)
        .ok()
        .map(Settings::from)
}

/// Strictly reads only the `[scan]` section of a config file, for the
/// command line. Unlike [`Settings::load`], a missing or invalid file is an
/// error rather than a silent fall back to defaults. `path` of `None` means
/// the default [`config_path`]. A file without a `[scan]` section yields the
/// default scan settings.
pub fn load_scan_settings(path: Option<&std::path::Path>) -> Result<ScanSettings, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct OnlyScan {
        scan: ScanSettings,
    }
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => config_path().ok_or("cannot locate the default config file")?,
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read config file {}: {e}", path.display()))?;
    toml::from_str::<OnlyScan>(&text)
        .map(|c| c.scan)
        .map_err(|e| format!("invalid config file {}: {e}", path.display()))
}

impl Settings {
    /// Missing or unreadable files give the defaults. A flat `config.toml`
    /// or a JSON `settings.json` from an older version is migrated to the
    /// current format on first load.
    pub fn load() -> Self {
        if let Some(text) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            if let Some(settings) = parse_current(&text).or_else(|| parse_legacy_toml(&text)) {
                return settings;
            }
        }
        if let Some(text) = legacy_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            if let Ok(old) = serde_json::from_str::<LegacySettings>(&text) {
                let settings = Settings::from(old);
                settings.save();
                // Only drop the legacy file once the migrated config exists.
                if config_path().is_some_and(|p| p.exists()) {
                    if let Some(p) = legacy_path() {
                        let _ = std::fs::remove_file(p);
                    }
                }
                return settings;
            }
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

    /// The sort currently in force: `[list] sort` wins, the legacy top-level
    /// `sort` is only a fallback for files that predate it.
    pub fn effective_sort(&self) -> SortSettings {
        self.list.sort
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_fields_missing() {
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.updates.auto_check);
        assert!(s.updates.skipped_version.is_none());
        assert!(!s.treemap.show_free_space);
        assert!(!s.treemap.show_unknown);
        assert!(!s.treemap.disk_size);
        assert!(s.scan.show_partial_results);
        assert!(!s.scan.cross_volumes);
        assert!(
            !s.scan.collect_owners,
            "owner collection is opt-in because it is expensive"
        );
        assert!(s.list.show_owner);
        assert_eq!(s.appearance.ui_scale, 1.0);
        assert!(s.integration.context_menu);
        assert!(!s.integration.single_instance);
        assert!(s.scan.thread_count.is_none());
        assert_eq!(s.appearance.theme, ThemePreference::System);
        assert_eq!(s.list.sort, SortSettings::default());
        assert_eq!(s.list.top_files_limit, default_top_files_limit());
        assert!(s.window.is_none());
        assert!(s.panels.main.is_empty());
        assert!(s.startup.last_path.is_none());
        assert_eq!(s.list.size_unit, SizeUnit::Binary);
        assert_eq!(s.list.date_format, DateFormat::Absolute);
        assert!(!s.list.hide_empty_folders, "empty folders shown by default");
        assert!(s.ops.confirm_recycle);
        assert_eq!(s.ops.default_delete, DeleteAction::Recycle);
        assert!(
            !s.ops.keep_selection_after_delete,
            "keeping the view after a delete is opt-in"
        );
        assert_eq!(s.updates.channel, UpdateChannel::Stable);
        assert!(
            fastdir_locale::is_supported(&s.locale.language) || s.locale.language == "system",
            "fresh default follows the OS language or falls back to English"
        );
    }

    #[test]
    fn nested_defaults_when_only_some_fields_are_given() {
        let s: Settings = toml::from_str(
            r#"
            [appearance]
            theme = "dark"

            [list.sort]
            column = "name"
            "#,
        )
        .unwrap();
        assert_eq!(s.appearance.theme, ThemePreference::Dark);
        assert_eq!(s.list.sort.column, SortColumn::Name);
        assert!(
            s.list.sort.descending,
            "the missing direction keeps its default"
        );
    }

    #[test]
    fn roundtrip() {
        let s = Settings {
            scan: ScanSettings {
                excluded: vec!["$Recycle.Bin".into(), "*.tmp".into()],
                skip_hidden_system: true,
                follow_junctions: true,
                cross_volumes: false,
                thread_count: Some(4),
                show_partial_results: false,
                collect_owners: true,
            },
            treemap: TreemapSettings {
                show_free_space: true,
                show_unknown: true,
                disk_size: true,
                palette: TreemapPalette::ColorBlind,
                color_mode: TreemapColorMode::Age,
                cushion: false,
                cushion_strength: 0.5,
                borders: true,
                min_cell: 2.0,
                show: false,
            },
            list: ListSettings {
                size_unit: SizeUnit::Decimal,
                percent_base: PercentBase::Total,
                date_format: DateFormat::Relative,
                age_tint: true,
                sort: SortSettings {
                    column: SortColumn::Modified,
                    descending: false,
                },
                top_files_limit: 250,
                show_items: false,
                show_files: true,
                show_folders: true,
                show_modified: false,
                show_disk: true,
                show_owner: false,
                hide_empty_folders: true,
                ext_grouping: ExtGrouping::Category,
            },
            ops: OpSettings {
                confirm_recycle: false,
                confirm_permanent: true,
                confirm_move: false,
                default_delete: DeleteAction::Permanent,
                keep_selection_after_delete: true,
                last_move_dest: Some(r"C:\backup".into()),
            },
            startup: StartupSettings {
                mode: StartupMode::Last,
                last_path: Some(r"C:\Users".into()),
            },
            updates: UpdateSettings {
                auto_check: false,
                channel: UpdateChannel::Prerelease,
                frequency: UpdateFrequency::Weekly,
                last_check: Some(1_800_000_000),
                skipped_version: Some("0.2.0".into()),
                last_run_version: Some("0.1.0".into()),
            },
            appearance: AppearanceSettings {
                theme: ThemePreference::Light,
                ui_scale: 1.25,
            },
            integration: IntegrationSettings {
                context_menu: false,
                single_instance: true,
            },
            locale: LocaleSettings {
                language: "es".into(),
            },
            categories: BTreeMap::from([
                (
                    "video".to_string(),
                    vec!["mp4".to_string(), "mkv".to_string()],
                ),
                ("other".to_string(), vec!["custom".to_string()]),
            ]),
            actions: vec![ActionDef {
                name: "cargo clean".into(),
                command: "cargo".into(),
                args: vec!["clean".into()],
                working_dir: "{path}".into(),
                applies_to: ActionAppliesTo::Dir,
                match_pattern: Some("Cargo.toml".into()),
                confirm: true,
                refresh_after: true,
                show_output: true,
                shell: true,
                shortcut: Some(1),
                enabled: true,
            }],
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
            sort: SortSettings {
                column: SortColumn::Modified,
                descending: false,
            },
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("[scan]"), "sections keep the file readable");
        assert!(text.contains("[treemap]"));
        assert!(
            text.contains("[locale]"),
            "the language section is written out"
        );
        assert!(
            text.contains("[categories]"),
            "category overrides are written out"
        );
        assert!(text.contains("mp4"), "category extensions are written out");
        assert!(text.contains("[[actions]]"), "actions are written out");
        assert!(text.contains("match = \"Cargo.toml\""));
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn actions_parse_and_default_safely() {
        let s: Settings = toml::from_str(
            r#"
            [[actions]]
            name = "cargo clean"
            command = "cargo"
            args = ["clean"]
            working_dir = "{path}"
            applies_to = "dir"
            match = "Cargo.toml"
            confirm = false
            refresh_after = true
            shortcut = 2

            [[actions]]
            name = "open"
            command = "code"
            applies_to = "both"
            "#,
        )
        .unwrap();
        assert_eq!(s.actions.len(), 2);
        let first = &s.actions[0];
        assert_eq!(first.applies_to, ActionAppliesTo::Dir);
        assert_eq!(first.match_pattern.as_deref(), Some("Cargo.toml"));
        assert!(!first.confirm);
        assert!(first.refresh_after);
        assert_eq!(first.shortcut, Some(2));
        assert!(!first.show_output, "missing flags keep safe defaults");
        assert!(!first.shell);
        // The second action keeps `confirm = true`: actions ask by default.
        assert!(s.actions[1].confirm);
        assert!(s.actions[1].enabled);
    }

    #[test]
    fn fresh_configs_offer_disabled_presets() {
        let s: Settings = toml::from_str("").unwrap();
        assert!(!s.actions.is_empty(), "presets are available to copy from");
        assert!(
            s.actions.iter().all(|a| !a.enabled),
            "presets are off by default"
        );
        assert!(
            s.actions.iter().all(|a| !a.is_usable()),
            "a disabled preset is never offered in the menu"
        );
        // A config that predates actions gets the same presets.
        let old: Settings = toml::from_str("[scan]\ncross_volumes = false\n").unwrap();
        assert_eq!(old.actions, s.actions);
        // An actions-only file is still the current format, not legacy.
        let only: Settings =
            toml::from_str("[[actions]]\nname = \"x\"\ncommand = \"y\"\n").unwrap();
        assert_eq!(only.actions[0].name, "x");
    }

    #[test]
    fn category_overrides_parse_on_their_own() {
        let s: Settings =
            toml::from_str("[categories]\nvideo = [\"mp4\", \"custom\"]\narchives = [\"zip\"]\n")
                .unwrap();
        assert_eq!(
            s.categories.get("video").map(Vec::as_slice),
            Some(["mp4".to_string(), "custom".to_string()].as_slice())
        );
        assert_eq!(
            s.categories.get("archives").map(Vec::as_slice),
            Some(["zip".to_string()].as_slice())
        );
        // Unknown category names are kept for forward compatibility; the
        // core `CategoryMap` ignores them.
        let unknown: Settings = toml::from_str("[categories]\nnope = [\"x\"]\n").unwrap();
        assert!(unknown.categories.contains_key("nope"));
    }

    #[test]
    fn locale_section_parses_and_old_files_keep_loading() {
        let s: Settings = toml::from_str("[locale]\nlanguage = \"es\"\n").unwrap();
        assert_eq!(s.locale.language, "es");
        // Files predating the section fall back to the OS language.
        let old: Settings = toml::from_str("[scan]\ncross_volumes = false\n").unwrap();
        assert!(!old.scan.cross_volumes);
        assert!(!old.locale.language.is_empty());
    }

    #[test]
    fn legacy_toml_maps_flat_keys_into_sections() {
        let toml_text = r#"
            auto_check_updates = false
            skipped_version = "0.2.0"
            last_run_version = "0.1.0"
            show_free_space = true
            show_unknown = true
            treemap_disk_size = true
            show_partial_results = false
            theme = "light"
            last_scan_path = "C:\\Users"

            [sort]
            column = "modified"
            descending = false
        "#;
        assert!(parse_current(toml_text).is_none());
        let s = parse_legacy_toml(toml_text).unwrap();
        assert!(!s.updates.auto_check);
        assert_eq!(s.updates.skipped_version.as_deref(), Some("0.2.0"));
        assert_eq!(s.updates.last_run_version.as_deref(), Some("0.1.0"));
        assert!(s.treemap.show_free_space);
        assert!(s.treemap.show_unknown);
        assert!(s.treemap.disk_size);
        assert!(!s.scan.show_partial_results);
        assert_eq!(s.appearance.theme, ThemePreference::Light);
        assert_eq!(s.list.sort.column, SortColumn::Modified);
        assert_eq!(s.startup.last_path.as_deref(), Some("C:\\Users"));
        // Untouched sections keep their defaults.
        assert_eq!(s.ops, OpSettings::default());
        assert_eq!(s.list.size_unit, SizeUnit::Binary);
    }

    #[test]
    fn legacy_toml_keeps_old_defaults_for_missing_keys() {
        let s = parse_legacy_toml("show_free_space = true\n").unwrap();
        assert!(s.updates.auto_check, "auto-check used to default to on");
        assert!(s.scan.show_partial_results);
        assert_eq!(s.appearance.theme, ThemePreference::System);
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
        let s: Settings = serde_json::from_str::<LegacySettings>(json)
            .map(Settings::from)
            .unwrap();
        assert!(!s.updates.auto_check);
        assert_eq!(s.updates.skipped_version.as_deref(), Some("0.2.0"));
        assert!(s.treemap.show_free_space);
        assert_eq!(s.appearance.theme, ThemePreference::System);
        assert_eq!(s.list.sort, SortSettings::default());
        assert_eq!(s.list.top_files_limit, default_top_files_limit());
    }

    #[test]
    fn scan_settings_load_is_strict() {
        let dir = std::env::temp_dir().join(format!("fastdir-scan-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("ok.toml");
        std::fs::write(
            &ok,
            "[scan]
excluded = [\"*.tmp\"]
follow_junctions = true
thread_count = 4
",
        )
        .unwrap();
        let s = load_scan_settings(Some(&ok)).unwrap();
        assert_eq!(s.excluded, vec!["*.tmp".to_string()]);
        assert!(s.follow_junctions);
        assert_eq!(s.thread_count, Some(4));

        let none = dir.join("none.toml");
        std::fs::write(
            &none,
            "[treemap]
show = false
",
        )
        .unwrap();
        assert_eq!(
            load_scan_settings(Some(&none)).unwrap(),
            ScanSettings::default()
        );

        let bad = dir.join("bad.toml");
        std::fs::write(
            &bad,
            "[scan
nope",
        )
        .unwrap();
        assert!(
            load_scan_settings(Some(&bad))
                .unwrap_err()
                .contains("invalid")
        );
        assert!(load_scan_settings(Some(&dir.join("missing.toml"))).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

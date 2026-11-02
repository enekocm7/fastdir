//! Platform wording for file-manager and modifier-key labels.
//!
//! The file manager ("Explorer" / "Finder" / "Files"), the trash
//! ("Recycle Bin" / "Trash") and the modifier key ("Ctrl" / "Cmd") differ
//! per OS. These helpers keep the labels right on Linux and macOS. A full
//! shortcut-system overhaul (user-remappable keys) is deliberately out of
//! scope: all bindings stay Ctrl-based and macOS additionally binds the Cmd
//! equivalents in [`super::bind_keys`].

/// Modifier name shown in tooltips: Cmd on macOS, Ctrl everywhere else.
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// "Show in Finder" on macOS, "Show in Files" on Linux, "Show in Explorer"
/// on Windows.
pub fn reveal_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Show in Finder"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "Show in Files"
    }
    #[cfg(not(unix))]
    {
        "Show in Explorer"
    }
}

/// "Trash" outside Windows, "Recycle Bin" on it.
pub fn trash_name() -> &'static str {
    #[cfg(windows)]
    {
        "Recycle Bin"
    }
    #[cfg(not(windows))]
    {
        "Trash"
    }
}

/// Tooltip for an action with a modifier shortcut, e.g. `tip("Open", "O")`
/// renders "Open (Ctrl+O)" — or "Open (Cmd+O)" on macOS.
pub fn tip(action: &str, key: &str) -> String {
    format!("{action} ({}+{key})", mod_key())
}

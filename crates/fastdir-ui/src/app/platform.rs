//! Platform wording for shortcut labels.
//!
//! The modifier key is Cmd on macOS and Ctrl everywhere else. (File-manager
//! names and the trash name used to live here too; they moved into the
//! `fastdir-locale` tables so they translate.) Full shortcut-system overhaul
//! (user-remappable keys) is deliberately out of scope: all bindings stay
//! Ctrl-based and macOS additionally binds the Cmd equivalents in
//! [`super::bind_keys`].

/// Modifier name shown in tooltips: Cmd on macOS, Ctrl everywhere else.
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

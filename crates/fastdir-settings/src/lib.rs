//! Persisted FastDir settings (`%APPDATA%\FastDir\config.toml` on Windows,
//! `~/.config/FastDir/config.toml` elsewhere).

mod settings;

pub use settings::*;

//! File-system operations triggered from the UI.

use std::io;
use std::path::{Path, PathBuf};

pub fn delete(path: &Path, permanent: bool) -> Result<(), String> {
    if permanent {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let res = if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        res.map_err(|e| e.to_string())
    } else {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("The name cannot be empty".into());
    }
    if new_name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("The name contains characters that are not allowed: \\ / : * ? \" < > |".into());
    }
    let parent = path.parent().ok_or("Cannot rename a drive root")?;
    let dest = parent.join(new_name);
    if dest.exists() && !dest.as_os_str().eq_ignore_ascii_case(path.as_os_str()) {
        return Err(format!("\"{new_name}\" already exists"));
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Move `src` into the directory `dest_dir`. Falls back to copy + delete
/// when the destination is on another volume.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let name = src.file_name().ok_or("Cannot move a drive root")?;
    let dest = dest_dir.join(name);
    if dest.exists() {
        return Err(format!("\"{}\" already exists", dest.display()));
    }
    if dest.starts_with(src) {
        return Err("Cannot move a folder into itself".into());
    }
    match std::fs::rename(src, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            copy_recursive(src, &dest).map_err(|e| format!("Copy failed: {e}"))?;
            let meta = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                std::fs::remove_dir_all(src)
            } else {
                std::fs::remove_file(src)
            }
            .map_err(|e| format!("Copied, but removing the original failed: {e}"))?;
            Ok(dest)
        }
    }
}

fn copy_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

/// Show the item selected in the system file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Open a file or folder with its default application.
pub fn open(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

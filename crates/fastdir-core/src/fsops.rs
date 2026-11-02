//! File-system operations triggered from the UI.

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
        return Err(
            "The name contains characters that are not allowed: \\ / : * ? \" < > |".into(),
        );
    }
    let parent = path.parent().ok_or("Cannot rename a drive root")?;
    let dest = parent.join(new_name);
    if dest.exists() && !dest.as_os_str().eq_ignore_ascii_case(path.as_os_str()) {
        return Err(format!("\"{new_name}\" already exists"));
    }
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Shared counters for a long-running delete / move, polled by the UI.
///
/// `items` counts files and folders processed, `bytes` the file bytes
/// deleted or copied. Setting `cancel` stops the operation at the next file
/// (or the next chunk of a large file being copied).
#[derive(Default, Debug)]
pub struct OpProgress {
    pub items: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

impl OpProgress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn add(&self, items: u64, bytes: u64) {
        self.items.fetch_add(items, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Raise the counters to at least the given totals. Called after each
    /// top-level item so operations that can't report finer progress (the
    /// Recycle Bin, same-volume renames) still advance the bar.
    pub fn reach(&self, items: u64, bytes: u64) {
        self.items.fetch_max(items, Ordering::Relaxed);
        self.bytes.fetch_max(bytes, Ordering::Relaxed);
    }
}

/// What happened to one path handed to [`delete_tracked`].
#[derive(Debug, Default)]
pub struct DeleteReport {
    /// Paths that could not be removed, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// The operation was cancelled before this path was fully removed.
    pub cancelled: bool,
}

impl DeleteReport {
    pub fn complete(&self) -> bool {
        self.failed.is_empty() && !self.cancelled
    }
}

/// Delete `path` (recursively), reporting progress and honouring
/// cancellation. A permanent delete keeps going past items it can't remove
/// and lists them in the report; whatever could be removed stays removed.
pub fn delete_tracked(path: &Path, permanent: bool, progress: &OpProgress) -> DeleteReport {
    let mut report = DeleteReport::default();
    if progress.is_cancelled() {
        report.cancelled = true;
        return report;
    }
    if permanent {
        remove_tree(path, progress, &mut report.failed);
        report.cancelled = progress.is_cancelled() && path_exists(path);
    } else if let Err(e) = trash::delete(path) {
        report.failed.push((path.to_path_buf(), e.to_string()));
    }
    report
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Remove `path` and everything below it, depth first. Returns whether
/// `path` itself is gone. Failures are collected rather than aborting.
fn remove_tree(path: &Path, progress: &OpProgress, failed: &mut Vec<(PathBuf, String)>) -> bool {
    if progress.is_cancelled() {
        return false;
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
        Err(e) => {
            failed.push((path.to_path_buf(), e.to_string()));
            return false;
        }
    };
    // Symlinks and junctions are removed themselves, never followed.
    if meta.is_dir() {
        let entries = match std::fs::read_dir(path) {
            Ok(it) => it,
            Err(e) => {
                failed.push((path.to_path_buf(), e.to_string()));
                return false;
            }
        };
        let mut all_gone = true;
        for entry in entries {
            match entry {
                Ok(entry) => all_gone &= remove_tree(&entry.path(), progress, failed),
                Err(e) => {
                    failed.push((path.to_path_buf(), e.to_string()));
                    all_gone = false;
                }
            }
        }
        if !all_gone {
            return false;
        }
        match retry_readonly(path, &meta, || std::fs::remove_dir(path)) {
            Ok(()) => {
                progress.add(1, 0);
                true
            }
            Err(e) => {
                failed.push((path.to_path_buf(), e.to_string()));
                false
            }
        }
    } else {
        let is_link = meta.file_type().is_symlink();
        let res = retry_readonly(path, &meta, || {
            std::fs::remove_file(path).or_else(|e| {
                // Directory symlinks / junctions on Windows need RemoveDirectory.
                if is_link {
                    std::fs::remove_dir(path)
                } else {
                    Err(e)
                }
            })
        });
        match res {
            Ok(()) => {
                progress.add(1, if is_link { 0 } else { meta.len() });
                true
            }
            Err(e) => {
                failed.push((path.to_path_buf(), e.to_string()));
                false
            }
        }
    }
}

/// Run `op`; if it is refused because the item is read-only, clear the
/// read-only flag and try once more (Explorer deletes such files too).
fn retry_readonly(
    path: &Path,
    meta: &std::fs::Metadata,
    op: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    match op() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && meta.permissions().readonly() => {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            if std::fs::set_permissions(path, perms).is_err() {
                return Err(e);
            }
            op()
        }
        res => res,
    }
}

/// Why [`move_into_tracked`] did not move anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveError {
    /// Cancelled mid-copy; the partial destination copy was removed.
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveError::Cancelled => f.write_str("Cancelled"),
            MoveError::Failed(e) => f.write_str(e),
        }
    }
}

/// A successful move.
#[derive(Debug)]
pub struct Moved {
    pub dest: PathBuf,
    /// Cross-volume moves only: parts of the source that could not be
    /// removed after everything had been copied.
    pub leftover: Vec<(PathBuf, String)>,
}

/// Move `src` into the directory `dest_dir`. Falls back to copy + delete
/// when the destination is on another volume.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    match move_into_tracked(src, dest_dir, &OpProgress::default()) {
        Ok(Moved { dest, leftover }) => match leftover.first() {
            None => Ok(dest),
            Some((p, e)) => Err(format!(
                "Copied, but removing the original failed: {}: {e}",
                p.display()
            )),
        },
        Err(e) => Err(e.to_string()),
    }
}

/// [`move_into`] with progress and cancellation. Cancelling while copying
/// across volumes removes the partial copy and leaves the source untouched.
/// Once the copy is complete the source is removed even if cancel is set,
/// so the files never end up in both places.
pub fn move_into_tracked(
    src: &Path,
    dest_dir: &Path,
    progress: &OpProgress,
) -> Result<Moved, MoveError> {
    let name = src
        .file_name()
        .ok_or_else(|| MoveError::Failed("Cannot move a drive root".into()))?;
    let dest = dest_dir.join(name);
    if path_exists(&dest) {
        return Err(MoveError::Failed(format!(
            "\"{}\" already exists",
            dest.display()
        )));
    }
    if dest.starts_with(src) {
        return Err(MoveError::Failed("Cannot move a folder into itself".into()));
    }
    if progress.is_cancelled() {
        return Err(MoveError::Cancelled);
    }
    if std::fs::rename(src, &dest).is_ok() {
        return Ok(Moved {
            dest,
            leftover: Vec::new(),
        });
    }
    if let Err(e) = copy_tracked(src, &dest, progress) {
        let cancelled = e.kind() == io::ErrorKind::Interrupted && progress.is_cancelled();
        let mut cleanup = Vec::new();
        remove_tree(&dest, &OpProgress::default(), &mut cleanup);
        let reason = if cancelled {
            "Cancelled".to_string()
        } else {
            format!("Copy failed: {e}")
        };
        return Err(match cleanup.first() {
            None if cancelled => MoveError::Cancelled,
            None => MoveError::Failed(reason),
            Some((p, ce)) => MoveError::Failed(format!(
                "{reason}, and the partial copy could not be fully removed ({}: {ce})",
                p.display()
            )),
        });
    }
    let mut leftover = Vec::new();
    remove_tree(src, &OpProgress::default(), &mut leftover);
    Ok(Moved { dest, leftover })
}

fn cancelled_err() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled")
}

fn copy_tracked(src: &Path, dest: &Path, progress: &OpProgress) -> io::Result<()> {
    if progress.is_cancelled() {
        return Err(cancelled_err());
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tracked(&entry.path(), &dest.join(entry.file_name()), progress)?;
        }
    } else {
        copy_file(src, dest, meta.len(), progress)?;
    }
    progress.add(1, 0);
    Ok(())
}

/// Files below this size go through `std::fs::copy` (the OS fast path, which
/// also keeps attributes); larger ones are copied in chunks so the progress
/// bar moves and cancel takes effect mid-file.
const CHUNKED_COPY_MIN: u64 = 16 << 20;
const CHUNK: usize = 4 << 20;

fn copy_file(src: &Path, dest: &Path, len: u64, progress: &OpProgress) -> io::Result<()> {
    if len < CHUNKED_COPY_MIN {
        let n = std::fs::copy(src, dest)?;
        progress.add(0, n);
        return Ok(());
    }
    let mut reader = std::fs::File::open(src)?;
    let meta = reader.metadata()?;
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if progress.is_cancelled() {
            return Err(cancelled_err());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        writer.write_all(&buf[..n])?;
        progress.add(0, n as u64);
    }
    writer.flush()?;
    if let Ok(modified) = meta.modified() {
        let _ = writer.set_modified(modified);
    }
    drop(writer);
    std::fs::set_permissions(dest, meta.permissions())?;
    Ok(())
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
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fastdir-fsops-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `root/a.txt` (5 bytes), `root/sub/b.bin` (10), `root/sub/c.ro` (3,
    /// read-only).
    fn sample(root: &Path) {
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("sub/b.bin"), [0u8; 10]).unwrap();
        let ro = root.join("sub/c.ro");
        std::fs::write(&ro, b"abc").unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
    }

    #[test]
    fn permanent_delete_counts_items_and_bytes() {
        let dir = temp_dir();
        let target = dir.join("t");
        sample(&target);
        let p = OpProgress::default();
        let report = delete_tracked(&target, true, &p);
        assert!(report.complete(), "{report:?}");
        assert!(!target.exists());
        // 3 files + `sub` + `t` itself.
        assert_eq!(p.items.load(Ordering::Relaxed), 5);
        assert_eq!(p.bytes.load(Ordering::Relaxed), 18);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancelled_delete_leaves_the_tree() {
        let dir = temp_dir();
        let target = dir.join("t");
        sample(&target);
        let p = OpProgress::default();
        p.cancel();
        let report = delete_tracked(&target, true, &p);
        assert!(report.cancelled);
        assert!(report.failed.is_empty());
        assert!(target.join("a.txt").exists());
        delete_tracked(&target, true, &OpProgress::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copy_then_remove_moves_everything() {
        let dir = temp_dir();
        let src = dir.join("src");
        let dest_dir = dir.join("dest");
        sample(&src);
        std::fs::create_dir(&dest_dir).unwrap();
        // Exercise the copy fallback directly (rename would succeed here).
        let p = OpProgress::default();
        copy_tracked(&src, &dest_dir.join("src"), &p).unwrap();
        assert_eq!(p.bytes.load(Ordering::Relaxed), 18);
        assert_eq!(p.items.load(Ordering::Relaxed), 5);
        assert_eq!(
            std::fs::read(dest_dir.join("src/sub/b.bin")).unwrap(),
            [0u8; 10]
        );
        delete_tracked(&dir, true, &OpProgress::default());
    }

    #[test]
    fn move_into_tracked_renames_and_rejects_conflicts() {
        let dir = temp_dir();
        let src = dir.join("src");
        let dest_dir = dir.join("dest");
        sample(&src);
        std::fs::create_dir(&dest_dir).unwrap();
        let moved = move_into_tracked(&src, &dest_dir, &OpProgress::default()).unwrap();
        assert_eq!(moved.dest, dest_dir.join("src"));
        assert!(moved.leftover.is_empty());
        assert!(!src.exists());
        assert!(moved.dest.join("a.txt").exists());

        std::fs::create_dir(&src).unwrap();
        assert!(matches!(
            move_into_tracked(&src, &dest_dir, &OpProgress::default()),
            Err(MoveError::Failed(_))
        ));
        let p = OpProgress::default();
        p.cancel();
        std::fs::remove_dir(&src).unwrap();
        std::fs::create_dir(dir.join("other")).unwrap();
        assert_eq!(
            move_into_tracked(&dir.join("other"), &dest_dir, &p).unwrap_err(),
            MoveError::Cancelled
        );
        delete_tracked(&dir, true, &OpProgress::default());
    }

    #[test]
    fn cancelled_chunked_copy_is_interrupted() {
        let dir = temp_dir();
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![1u8; CHUNKED_COPY_MIN as usize]).unwrap();
        let p = OpProgress::default();
        p.cancel();
        let err = copy_file(&big, &dir.join("copy.bin"), CHUNKED_COPY_MIN, &p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        delete_tracked(&dir, true, &OpProgress::default());
    }
}

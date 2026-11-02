//! Parallel directory scanner.
//!
//! Every directory is listed on a work-stealing thread pool (rayon); its
//! sub-directories are fanned out as independent tasks, so all cores stay
//! busy and the disk queue stays deep. On Windows the listing uses
//! `FindFirstFileExW` with `FIND_FIRST_EX_LARGE_FETCH` and the basic info
//! level, which returns name, size, attributes and timestamps for a whole
//! batch of entries in one kernel call — no per-file `stat`.

use crate::model::{Node, Tree};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Live counters shared between the scan threads and the UI.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct ScanResult {
    pub tree: Tree,
    pub elapsed: std::time::Duration,
    pub cancelled: bool,
}

/// A pool sized for I/O: listing directories blocks in the kernel, so more
/// threads than cores keeps the device queue full (especially on NVMe).
fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        rayon::ThreadPoolBuilder::new()
            .num_threads((cores * 2).clamp(4, 64))
            .thread_name(|i| format!("fastdir-scan-{i}"))
            .stack_size(8 * 1024 * 1024)
            .build()
            .expect("scan thread pool")
    })
}

/// Scan `root` (blocking). Call from a background thread.
pub fn scan(root: PathBuf, progress: Arc<Progress>) -> ScanResult {
    let start = Instant::now();
    let tree = pool().install(|| {
        let name: Box<str> = root.to_string_lossy().into();
        let mut node = scan_dir(&root, name, 0, &progress);
        node.expanded = true;
        Tree::new(node, root.clone())
    });
    ScanResult {
        tree,
        elapsed: start.elapsed(),
        cancelled: progress.cancelled(),
    }
}

/// Scan a single directory into a detached node (used after a move so the
/// destination reflects reality).
pub fn scan_subtree(path: &Path) -> Node {
    let progress = Progress::default();
    let name: Box<str> = path
        .file_name()
        .map(|n| n.to_string_lossy().into())
        .unwrap_or_else(|| path.to_string_lossy().into());
    pool().install(|| scan_dir(path, name, 0, &progress))
}

fn scan_dir(path: &Path, name: Box<str>, modified: i64, progress: &Progress) -> Node {
    let mut node = Node::dir(name, modified);
    if progress.cancelled() {
        return node;
    }
    if let Some(mut cur) = progress.current.try_lock() {
        cur.clear();
        cur.push_str(&path.to_string_lossy());
    }

    let mut subdirs: Vec<(Box<str>, i64)> = Vec::new();
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    let ok = list_dir(path, |entry| match entry {
        Entry::File { name, size, modified } => {
            files += 1;
            bytes += size;
            node.children.push(Node::file(name, size, modified));
        }
        Entry::Dir { name, modified } => subdirs.push((name, modified)),
        Entry::Link { name, modified } => {
            // Junctions / symlinked dirs: list them but never follow (avoids
            // cycles and double counting, same as WinDirStat).
            node.children.push(Node::dir(name, modified));
        }
    });
    if !ok {
        node.error = true;
        progress.errors.fetch_add(1, Ordering::Relaxed);
    }
    progress.files.fetch_add(files, Ordering::Relaxed);
    progress.bytes.fetch_add(bytes, Ordering::Relaxed);
    progress
        .dirs
        .fetch_add(subdirs.len() as u64, Ordering::Relaxed);

    if !subdirs.is_empty() {
        let children: Vec<Node> = subdirs
            .into_par_iter()
            .map(|(name, modified)| {
                let child_path = path.join(&*name);
                scan_dir(&child_path, name, modified, progress)
            })
            .collect();
        node.children.extend(children);
    }
    node.children.shrink_to_fit();
    node.finalize();
    node
}

enum Entry {
    File {
        name: Box<str>,
        size: u64,
        modified: i64,
    },
    Dir {
        name: Box<str>,
        modified: i64,
    },
    Link {
        name: Box<str>,
        modified: i64,
    },
}

#[cfg(windows)]
fn list_dir(path: &Path, mut f: impl FnMut(Entry)) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        WIN32_FIND_DATAW,
    };

    // Extended-length prefix lifts the MAX_PATH limit.
    let mut wide: Vec<u16> = Vec::with_capacity(path.as_os_str().len() + 8);
    let s = path.as_os_str();
    let raw: Vec<u16> = s.encode_wide().collect();
    let has_prefix = raw.starts_with(&[b'\\' as u16, b'\\' as u16]);
    if !has_prefix {
        wide.extend(r"\\?\".encode_utf16());
    }
    wide.extend(raw.iter().map(|&c| if c == b'/' as u16 { b'\\' as u16 } else { c }));
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(b'*' as u16);
    wide.push(0);

    fn filetime_to_unix(ft: FILETIME) -> i64 {
        let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        // 100ns intervals since 1601-01-01.
        (t / 10_000_000) as i64 - 11_644_473_600
    }

    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let handle = FindFirstFileExW(
            wide.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        );
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        loop {
            let len = data
                .cFileName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(data.cFileName.len());
            let name_w = &data.cFileName[..len];
            let is_dot = name_w == [b'.' as u16] || name_w == [b'.' as u16, b'.' as u16];
            if !is_dot {
                let name: Box<str> = String::from_utf16_lossy(name_w).into_boxed_str();
                let modified = filetime_to_unix(data.ftLastWriteTime);
                let attrs = data.dwFileAttributes;
                if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        f(Entry::Link { name, modified });
                    } else {
                        f(Entry::Dir { name, modified });
                    }
                } else {
                    let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
                    f(Entry::File {
                        name,
                        size,
                        modified,
                    });
                }
            }
            if FindNextFileW(handle, &mut data) == 0 {
                break;
            }
        }
        FindClose(handle);
    }
    true
}

#[cfg(not(windows))]
fn list_dir(path: &Path, mut f: impl FnMut(Entry)) -> bool {
    let Ok(rd) = std::fs::read_dir(path) else {
        return false;
    };
    for entry in rd.flatten() {
        let name: Box<str> = entry.file_name().to_string_lossy().into();
        let Ok(ft) = entry.file_type() else { continue };
        let meta = entry.metadata().ok();
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if ft.is_symlink() {
            f(Entry::Link { name, modified });
        } else if ft.is_dir() {
            f(Entry::Dir { name, modified });
        } else {
            f(Entry::File {
                name,
                size: meta.map(|m| m.len()).unwrap_or(0),
                modified,
            });
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fastdir-scanner-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_files_and_subdirectories() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("sub/empty.dat"), b"").unwrap();

        let result = scan(dir.clone(), Arc::new(Progress::default()));
        assert!(!result.cancelled);
        assert_eq!(result.tree.root.files, 3);
        assert_eq!(result.tree.root.dirs, 1);
        assert_eq!(result.tree.root.size, 15);
        assert!(result.tree.find(&dir.join("sub/b.bin")).is_some());

        let sub = result.tree.find(&dir.join("sub")).unwrap();
        assert_eq!(result.tree.get(&sub).unwrap().size, 10);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_subtree_returns_a_detached_node() {
        let dir = temp_dir();
        std::fs::write(dir.join("one.txt"), b"1234").unwrap();
        let node = scan_subtree(&dir);
        assert!(node.is_dir());
        assert_eq!(node.files, 1);
        assert_eq!(node.size, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let progress = Arc::new(Progress::default());
        progress.cancel.store(true, Ordering::Relaxed);
        let result = scan(dir.clone(), progress);
        assert!(result.cancelled);
        assert_eq!(result.tree.root.files, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_directory_is_marked_as_an_error() {
        let dir = temp_dir().join("does-not-exist");
        let result = scan(dir, Arc::new(Progress::default()));
        assert!(result.tree.root.error);
    }
}

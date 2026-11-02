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
use std::collections::HashSet;
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

/// File IDs already seen during a scan. Hard links share their ID, so only
/// the first link adds bytes to the tree; the rest are marked as duplicates.
///
/// The set is sharded so the parallel scan threads rarely contend. Entries are
/// only inserted for files that can actually contribute bytes.
struct LinkTracker {
    shards: [Mutex<HashSet<u64>>; 16],
}

impl Default for LinkTracker {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(HashSet::new())),
        }
    }
}

impl LinkTracker {
    /// `true` when `id` was seen before. A zero ID means "unknown" and is
    /// never treated as a duplicate.
    fn is_duplicate(&self, id: u64) -> bool {
        if id == 0 {
            return false;
        }
        let shard = (id.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60) as usize;
        !self.shards[shard].lock().insert(id)
    }
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
    let links = LinkTracker::default();
    let tree = pool().install(|| {
        let name: Box<str> = root.to_string_lossy().into();
        let mut node = scan_dir(&root, name, 0, &progress, &links);
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
    let links = LinkTracker::default();
    pool().install(|| scan_dir(path, name, 0, &progress, &links))
}

/// Allocated size of a single file (compressed/sparse aware where the OS
/// exposes it). Used for files that were moved into the tree without a scan.
#[cfg(windows)]
pub fn allocated_size(path: &Path) -> u64 {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
    };

    let meta = std::fs::metadata(path).ok();
    let logical = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let attrs = meta.as_ref().map(|m| m.file_attributes()).unwrap_or(0);
    if attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0
        && let Some(size) = compressed_size(path)
    {
        return size;
    }
    round_up(logical, cluster_size(path))
}

#[cfg(unix)]
pub fn allocated_size(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.blocks().saturating_mul(512))
        .unwrap_or(0)
}

#[cfg(not(any(windows, unix)))]
pub fn allocated_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn scan_dir(
    path: &Path,
    name: Box<str>,
    modified: i64,
    progress: &Progress,
    links: &LinkTracker,
) -> Node {
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
        Entry::File {
            name,
            size,
            disk_size,
            modified,
            file_id,
        } => {
            files += 1;
            bytes += size;
            // A zero-length file shares no blocks, so its link count is
            // irrelevant; skip it to keep the tracker small.
            if size > 0 && links.is_duplicate(file_id) {
                node.children.push(Node::hard_link(name, modified));
            } else {
                node.children
                    .push(Node::file_on_disk(name, size, disk_size, modified));
            }
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
                scan_dir(&child_path, name, modified, progress, links)
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
        disk_size: u64,
        modified: i64,
        /// Unique per file (shared by hard links), or 0 when unknown.
        file_id: u64,
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
fn wide_extended(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let mut wide: Vec<u16> = Vec::with_capacity(raw.len() + 8);
    // Extended-length prefix lifts the MAX_PATH limit.
    if !raw.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        wide.extend(r"\\?\".encode_utf16());
    }
    wide.extend(
        raw.iter()
            .map(|&c| if c == b'/' as u16 { b'\\' as u16 } else { c }),
    );
    wide
}

#[cfg(windows)]
fn wide_z(path: &Path) -> Vec<u16> {
    let mut wide = wide_extended(path);
    wide.push(0);
    wide
}

/// Convert a Windows 100ns-since-1601 timestamp to Unix seconds.
#[cfg(windows)]
fn filetime_i64_to_unix(t: i64) -> i64 {
    t / 10_000_000 - 11_644_473_600
}

/// The volume root (`C:\` or `\\server\share\`) for `path`.
#[cfg(windows)]
fn volume_root(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    // Normalise the extended-length forms back to plain paths.
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix(r"\\") {
        let mut parts = rest.splitn(3, '\\');
        if let (Some(server), Some(share)) = (parts.next(), parts.next()) {
            return format!(r"\\{server}\{share}\");
        }
    } else if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return format!("{}\\", &s[..2]);
    }
    s
}

#[cfg(windows)]
fn cluster_size(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;

    let root = volume_root(path);
    let mut wide: Vec<u16> = std::ffi::OsStr::new(&root).encode_wide().collect();
    wide.push(0);
    let (mut sectors, mut bytes) = (0u32, 0u32);
    let ok = unsafe {
        GetDiskFreeSpaceW(
            wide.as_ptr(),
            &mut sectors,
            &mut bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        0
    } else {
        sectors as u64 * bytes as u64
    }
}

#[cfg(windows)]
fn round_up(n: u64, cluster: u64) -> u64 {
    if n == 0 || cluster == 0 {
        n
    } else {
        n.div_ceil(cluster) * cluster
    }
}

/// Actual allocated size of a compressed or sparse file.
#[cfg(windows)]
fn compressed_size(path: &Path) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};

    let wide = wide_z(path);
    let mut high = 0u32;
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == INVALID_FILE_SIZE {
        // `INVALID_FILE_SIZE` is also a legal size: only fail when
        // GetLastError says something went wrong.
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err != 0 {
            return None;
        }
    }
    Some(((high as u64) << 32) | low as u64)
}

/// List a directory with `GetFileInformationByHandleEx`, which returns the
/// file ID and allocated size of every entry in the same batch as the name,
/// size and timestamps. Falling back to `FindFirstFileExW` keeps filesystems
/// that lack `FileIdBothDirectoryInfo` working.
#[cfg(windows)]
fn list_dir(path: &Path, mut f: impl FnMut(Entry)) -> bool {
    if list_dir_by_id(path, &mut f) {
        return true;
    }
    list_dir_find(path, f)
}

#[cfg(windows)]
fn list_dir_by_id(path: &Path, f: &mut impl FnMut(Entry)) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
        GetFileInformationByHandleEx, OPEN_EXISTING,
    };

    let wide = wide_z(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }

    // 128 KiB, 8-byte aligned so the FILE_ID_BOTH_DIR_INFO casts are valid.
    let mut buf = vec![0u64; 16 * 1024];
    let mut restart = true;
    let mut listed_any = false;
    let cluster = cluster_size(path);
    loop {
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                class,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
            )
        };
        if ok == 0 {
            let code = std::io::Error::last_os_error().raw_os_error();
            unsafe { CloseHandle(handle) };
            if restart && code != Some(ERROR_NO_MORE_FILES as i32) {
                // Unsupported information class (or a denied directory): let
                // the caller retry with FindFirstFileExW.
                return false;
            }
            // An empty (or finished) directory is a success.
            return listed_any || restart;
        }
        restart = false;
        listed_any = true;

        let mut offset = 0usize;
        loop {
            let info = unsafe {
                &*((buf.as_ptr() as *const u8).add(offset) as *const FILE_ID_BOTH_DIR_INFO)
            };
            let len = info.FileNameLength as usize / 2;
            let name_w = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), len) };
            let is_dot = name_w == [b'.' as u16] || name_w == [b'.' as u16, b'.' as u16];
            if !is_dot {
                let name: Box<str> = String::from_utf16_lossy(name_w).into_boxed_str();
                let modified = filetime_i64_to_unix(info.LastWriteTime);
                let attrs = info.FileAttributes;
                if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        f(Entry::Link { name, modified });
                    } else {
                        f(Entry::Dir { name, modified });
                    }
                } else {
                    let size = info.EndOfFile.max(0) as u64;
                    let alloc = info.AllocationSize.max(0) as u64;
                    // `AllocationSize` is exact for compressed/sparse files.
                    // For ordinary files it can be the tiny MFT-resident size,
                    // so round the logical size up to the cluster instead.
                    let special =
                        attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0;
                    let disk_size = if special {
                        if alloc > 0 {
                            alloc
                        } else if size == 0 {
                            0
                        } else {
                            compressed_size(&path.join(&*name))
                                .unwrap_or_else(|| round_up(size, cluster))
                        }
                    } else {
                        round_up(size, cluster)
                    };
                    f(Entry::File {
                        name,
                        size,
                        disk_size,
                        modified,
                        file_id: info.FileId as u64,
                    });
                }
            }
            let next = info.NextEntryOffset as usize;
            if next == 0 || offset + next >= buf.len() * 8 {
                break;
            }
            offset += next;
        }
    }
}

#[cfg(windows)]
fn list_dir_find(path: &Path, mut f: impl FnMut(Entry)) -> bool {
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_SPARSE_FILE, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
        FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
    };

    let mut wide = wide_extended(path);
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(b'*' as u16);
    wide.push(0);

    fn filetime_to_unix(ft: FILETIME) -> i64 {
        let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        filetime_i64_to_unix(t as i64)
    }

    let cluster = cluster_size(path);
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
                    let disk_size =
                        if attrs & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
                            compressed_size(&path.join(&*name))
                                .unwrap_or_else(|| round_up(size, cluster))
                        } else {
                            round_up(size, cluster)
                        };
                    f(Entry::File {
                        name,
                        size,
                        disk_size,
                        modified,
                        file_id: 0,
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
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        #[cfg(unix)]
        let (disk_size, file_id) = {
            use std::os::unix::fs::MetadataExt;
            match &meta {
                // Hard links share (dev, inode). `ino` alone is unique on the
                // single volume a scan covers.
                Some(m) => (
                    m.blocks().saturating_mul(512),
                    if m.nlink() > 1 { m.ino() } else { 0 },
                ),
                None => (size, 0),
            }
        };
        #[cfg(not(unix))]
        let (disk_size, file_id) = (size, 0);
        if ft.is_symlink() {
            f(Entry::Link { name, modified });
        } else if ft.is_dir() {
            f(Entry::Dir { name, modified });
        } else {
            f(Entry::File {
                name,
                size,
                disk_size,
                modified,
                file_id,
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
    fn records_the_allocated_size_on_disk() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("empty.dat"), b"").unwrap();

        let result = scan(dir.clone(), Arc::new(Progress::default()));
        let a = result.tree.find(&dir.join("a.txt")).unwrap();
        let a = result.tree.get(&a).unwrap();
        assert_eq!(a.size, 5);
        assert!(a.disk_size >= a.size, "disk size was {}", a.disk_size);
        assert_eq!(allocated_size(&dir.join("a.txt")), a.disk_size);
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const COMPRESSED: u32 = 0x800;
            const SPARSE: u32 = 0x200;
            let attrs = std::fs::metadata(dir.join("a.txt"))
                .unwrap()
                .file_attributes();
            if attrs & (COMPRESSED | SPARSE) == 0 {
                let cluster = cluster_size(&dir);
                assert!(cluster > 0);
                assert_eq!(a.disk_size % cluster, 0);
            }
        }

        let empty = result.tree.find(&dir.join("empty.dat")).unwrap();
        let empty = result.tree.get(&empty).unwrap();
        assert_eq!(empty.size, 0);
        assert_eq!(empty.disk_size, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hard_links_are_counted_once_and_marked() {
        let dir = temp_dir();
        let original = dir.join("original.bin");
        std::fs::write(&original, vec![0u8; 4096]).unwrap();
        if std::fs::hard_link(&original, dir.join("link.bin")).is_err() {
            // The test filesystem does not support hard links.
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }

        let result = scan(dir.clone(), Arc::new(Progress::default()));
        assert_eq!(result.tree.root.files, 2);
        assert_eq!(result.tree.root.size, 4096);
        let links: Vec<_> = result
            .tree
            .root
            .children
            .iter()
            .filter(|n| n.hard_link)
            .collect();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].size, 0);
        assert_eq!(links[0].disk_size, 0);

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

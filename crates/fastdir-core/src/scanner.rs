//! Parallel directory scanner.
//!
//! Every directory is listed on a work-stealing thread pool (rayon); its
//! sub-directories are fanned out as independent tasks, so all cores stay
//! busy and the disk queue stays deep. On Windows the listing uses
//! `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`, which returns
//! name, size, allocation size, attributes, timestamps and the file ID for a
//! whole batch of entries in one kernel call — no per-file `stat`, and no 8.3
//! short names. Filesystems that reject that class fall back to
//! `FindFirstFileExW` with `FIND_FIRST_EX_LARGE_FETCH` and the basic info level.
//!
//! Anything that is constant for a volume (cluster size, whether the file-ID
//! class works) is looked up once per scan and passed down, never per folder.

use crate::model::{Arena, DirBuilder, NO_OWNER, Node, OwnerTable, Subtree, Tree};
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
#[cfg(windows)]
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

/// Result of a scan that wrote into a caller-owned live tree.
pub struct LiveScanResult {
    pub elapsed: std::time::Duration,
    pub cancelled: bool,
}

/// File IDs already seen during a scan. Hard links share their ID, so only
/// the first link adds bytes to the tree; the rest are marked as duplicates.
///
/// The set is sharded so the parallel scan threads rarely contend. Entries are
/// only inserted for files that can actually contribute bytes.
struct LinkTracker {
    shards: [Mutex<HashSet<u128>>; 16],
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
    /// never treated as a duplicate. IDs are 128-bit so ReFS file IDs (which
    /// do not fit in 64 bits) stay unique; NTFS and Unix only use the low half.
    fn is_duplicate(&self, id: u128) -> bool {
        if id == 0 {
            return false;
        }
        let folded = (id as u64) ^ ((id >> 64) as u64);
        let shard = (folded.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60) as usize;
        !self.shards[shard].lock().insert(id)
    }
}

/// Resolves and interns the owning account of scanned entries.
///
/// Every arena of one scan gets a snapshot of the same table, so the `u16`
/// owner indices stored with each block line up across top-level subtrees.
/// Lookups are cached: a whole scan almost always sees a handful of accounts.
struct OwnerCollector {
    /// uid -> user name (Unix), or raw SID -> display name (Windows).
    #[cfg(unix)]
    names: Mutex<HashMap<u32, Box<str>>>,
    #[cfg(windows)]
    names: Mutex<HashMap<Box<str>, Box<str>>>,
    #[cfg(not(any(unix, windows)))]
    names: Mutex<HashMap<u32, Box<str>>>,
    table: Mutex<OwnerTable>,
}

impl OwnerCollector {
    fn new() -> Self {
        Self {
            names: Mutex::new(HashMap::new()),
            table: Mutex::new(OwnerTable::default()),
        }
    }

    /// The table as it stands now, for one finished arena.
    fn snapshot(&self) -> OwnerTable {
        self.table.lock().clone()
    }

    /// The owner of the entry at `path` (whose listing reported `uid`).
    #[cfg(unix)]
    fn owner(&self, _path: &Path, uid: u32) -> u16 {
        if uid == u32::MAX {
            return NO_OWNER;
        }
        let mut names = self.names.lock();
        let name = names.entry(uid).or_insert_with(|| user_name(uid));
        self.table.lock().intern(name)
    }

    /// The owner of the entry at `path`.
    #[cfg(windows)]
    fn owner(&self, path: &Path, _uid: u32) -> u16 {
        let Some((sid, display)) = owner_of_path(path) else {
            return NO_OWNER;
        };
        let mut names = self.names.lock();
        if !names.contains_key(&sid) {
            names.insert(sid.clone(), display);
        }
        self.table.lock().intern(&names[&sid])
    }

    #[cfg(not(any(unix, windows)))]
    fn owner(&self, _path: &Path, _uid: u32) -> u16 {
        NO_OWNER
    }

    /// The owner of the scanned root folder itself.
    #[cfg(unix)]
    fn owner_of_root(&self, path: &Path) -> u16 {
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata(path).map(|m| m.uid()).unwrap_or(u32::MAX);
        self.owner(path, uid)
    }

    /// The owner of the scanned root folder itself.
    #[cfg(not(unix))]
    fn owner_of_root(&self, path: &Path) -> u16 {
        self.owner(path, 0)
    }
}

/// The user name for a Unix uid, or the raw uid when it cannot be resolved.
#[cfg(unix)]
fn user_name(uid: u32) -> Box<str> {
    use std::ffi::CStr;
    let mut buf = vec![0u8; 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
            return uid.to_string().into();
        }
        return unsafe { CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .into_owned()
            .into();
    }
}

/// The raw SID (used as the cache key) and display name of a file's owner.
/// The name is `DOMAIN\user`, or the raw SID when the account cannot be
/// resolved (deleted accounts, unreachable domain controllers).
#[cfg(windows)]
fn owner_of_path(path: &Path) -> Option<(Box<str>, Box<str>)> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSID};

    let wide = wide_z(path);
    let mut sid: PSID = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut sid,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || sid.is_null() {
        if !descriptor.is_null() {
            unsafe { LocalFree(descriptor) };
        }
        return None;
    }

    let mut raw: *mut u16 = std::ptr::null_mut();
    let sid_string: Option<Box<str>> = if unsafe { ConvertSidToStringSidW(sid, &mut raw) } != 0 {
        let text = wide_to_string(raw);
        unsafe { LocalFree(raw.cast()) };
        Some(text.into())
    } else {
        None
    };

    // Resolve the SID to `DOMAIN\user`; buffer sizes are in UTF-16 units.
    let mut name = vec![0u16; 256];
    let mut domain = vec![0u16; 256];
    let mut name_len = name.len() as u32;
    let mut domain_len = domain.len() as u32;
    let mut kind = 0;
    let mut ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    if ok == 0
        && std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        name.resize(name_len as usize, 0);
        domain.resize(domain_len as usize, 0);
        name_len = name.len() as u32;
        domain_len = domain.len() as u32;
        ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid,
                name.as_mut_ptr(),
                &mut name_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut kind,
            )
        };
    }
    let resolved: Option<Box<str>> = (ok != 0).then(|| {
        let user = String::from_utf16_lossy(&name[..name_len as usize]);
        let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
        if domain.is_empty() {
            user.into()
        } else {
            format!("{domain}\\{user}").into()
        }
    });

    unsafe { LocalFree(descriptor) };
    let sid = sid_string?;
    Some((sid.clone(), resolved.unwrap_or(sid)))
}

/// Decode a NUL-terminated UTF-16 buffer into a string.
#[cfg(windows)]
fn wide_to_string(wide: *const u16) -> String {
    let mut len = 0usize;
    while unsafe { *wide.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) })
}

/// What to scan and with how many threads. Built by the UI from the
/// persisted settings; the scanner itself never reads global state.
///
/// Field defaults preserve the historical behaviour (scan on one automatic
/// pool, never follow junctions, stay on the starting volume).
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    /// Folder/file names to skip. Each pattern is matched case-insensitively
    /// against the entry's name (`*` and `?` wildcards), e.g. `$Recycle.Bin`.
    /// The scan root itself is never excluded.
    pub excluded_globs: Vec<String>,
    /// Skip hidden and system entries (dot-files on Unix).
    pub skip_hidden_system: bool,
    /// Descend into junctions / symlinked folders. Off by default: following
    /// them can double-count files and loop forever, so every entered folder
    /// is remembered by its canonical path and only visited once.
    pub follow_junctions: bool,
    /// Descend into mount points on other volumes. Off by default, so a scan
    /// stays on the volume it started on; a mount point on another volume is
    /// listed but not entered. Only relevant when following junctions on
    /// Windows or for Unix mounts.
    pub cross_volumes: bool,
    /// Scan threads, or `None` for automatic (`cores * 2`, clamped 4..=64).
    /// A fixed number keeps a slow HDD or network share responsive.
    pub threads: Option<usize>,
    /// Record the owning account of every entry (the Owner column and the
    /// per-user totals). Off by default: on Windows this needs one extra
    /// security query per file, which is expensive, while Unix gets the uid
    /// from the listing it already does.
    pub collect_owners: bool,
}

impl ScanOptions {
    /// `true` when the entry `name` matches one of the exclusion patterns.
    pub fn is_excluded(&self, name: &str) -> bool {
        self.excluded_globs.iter().any(|p| matches_glob(p, name))
    }
}

/// Case-insensitive `*` / `?` wildcard match of `pattern` against `name`.
pub fn matches_glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    let n: Vec<char> = name.chars().flat_map(|c| c.to_lowercase()).collect();
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ni;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Identity of the volume `path` lives on, for the cross-volume check.
#[cfg(windows)]
fn volume_key(path: &Path) -> Option<String> {
    Some(volume_root(path))
}

/// Identity of the volume `path` lives on, for the cross-volume check.
#[cfg(not(windows))]
fn volume_key(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| format!("dev:{}", m.dev()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Facts about one volume that do not change during a scan. Looked up once
/// per volume instead of once per directory: a `GetDiskFreeSpaceW` call per
/// folder used to double the time of a full-drive scan.
struct Volume {
    /// Allocation unit used to round logical sizes up to the size on disk,
    /// or 0 when unknown (sizes are then reported as-is).
    #[cfg_attr(not(windows), allow(dead_code))]
    cluster: u64,
    /// Cleared the first time the volume rejects the file-ID listing class
    /// (FAT, some network shares), so later folders go straight to the
    /// `FindFirstFileExW` fallback instead of failing twice per folder.
    #[cfg_attr(not(windows), allow(dead_code))]
    by_id: AtomicBool,
}

impl Volume {
    #[cfg(windows)]
    fn for_path(path: &Path) -> Self {
        Self {
            cluster: cluster_size(path),
            by_id: AtomicBool::new(true),
        }
    }

    #[cfg(not(windows))]
    fn for_path(_path: &Path) -> Self {
        Self {
            cluster: 0,
            by_id: AtomicBool::new(false),
        }
    }
}

/// Per-scan policy plus the loop protection for followed junctions.
struct ScanContext<'a> {
    options: &'a ScanOptions,
    /// Canonical paths of folders already entered (only used when following).
    visited: &'a Mutex<HashSet<PathBuf>>,
    /// Volume of the scan root; sub-folders on other volumes are pruned
    /// unless `cross_volumes` is set.
    root_volume: Option<String>,
    /// Cached facts about the scan root's volume.
    root_info: Arc<Volume>,
    /// Cached facts about other volumes reached through followed junctions,
    /// keyed by [`volume_key`].
    volumes: Mutex<HashMap<String, Arc<Volume>>>,
    /// Owner resolution, present only when the scan collects owners.
    owners: Option<OwnerCollector>,
}

impl<'a> ScanContext<'a> {
    fn for_scan(
        root: &Path,
        options: &'a ScanOptions,
        visited: &'a Mutex<HashSet<PathBuf>>,
    ) -> Self {
        let canonical = std::fs::canonicalize(root).ok();
        if let Some(ref c) = canonical {
            visited.lock().insert(c.clone());
        }
        let resolved = canonical.as_deref().unwrap_or(root);
        let root_volume = volume_key(resolved);
        let root_info = Arc::new(Volume::for_path(resolved));
        Self {
            options,
            visited,
            root_volume,
            root_info,
            volumes: Mutex::new(HashMap::new()),
            owners: options.collect_owners.then(OwnerCollector::new),
        }
    }

    /// Cached [`Volume`] facts for the volume `path` lives on.
    fn volume_info(&self, path: &Path) -> Arc<Volume> {
        let Some(key) = volume_key(path) else {
            return self.root_info.clone();
        };
        if self.root_volume.as_deref() == Some(key.as_str()) {
            return self.root_info.clone();
        }
        self.volumes
            .lock()
            .entry(key)
            .or_insert_with(|| Arc::new(Volume::for_path(path)))
            .clone()
    }

    fn same_volume(&self, path: &Path) -> bool {
        match (volume_key(path), &self.root_volume) {
            (Some(a), Some(b)) => a == *b,
            // Unknowable (permissions, exotic FS): allow, the listing will
            // fail gracefully if it really cannot be read.
            _ => true,
        }
    }

    /// Whether the junction / symlink `name` inside `parent` may be entered,
    /// and if so the volume its target lives on. Records the target so a
    /// loop (A -> B -> A) is only visited once.
    fn follow_dir(&self, parent: &Path, name: &str) -> Option<Arc<Volume>> {
        let child = parent.join(name);
        let canonical = std::fs::canonicalize(&child).ok()?;
        if !canonical.is_dir() {
            return None;
        }
        if !self.options.cross_volumes && !self.same_volume(&canonical) {
            return None;
        }
        let volume = self.volume_info(&canonical);
        self.visited.lock().insert(canonical).then_some(volume)
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

/// How many threads the automatic pool uses (also the `threads` default).
pub fn default_thread_count() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores * 2).clamp(4, 64)
}

/// Runs `f` on the automatic pool, or on a throwaway pool with `threads`
/// when the user picked a fixed count (e.g. to keep a slow HDD responsive).
pub(crate) fn with_threads<R: Send>(threads: Option<usize>, f: impl FnOnce() -> R + Send) -> R {
    let mut f = Some(f);
    match threads.map(|n| n.clamp(1, 64)) {
        None => pool().install(f.take().expect("scan closure")),
        Some(n) => match rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("fastdir-scan-{i}"))
            .stack_size(8 * 1024 * 1024)
            .build()
        {
            Ok(pool) => pool.install(f.take().expect("scan closure")),
            Err(_) => pool().install(f.take().expect("scan closure")),
        },
    }
}

/// Scan `root` (blocking). Call from a background thread.
pub fn scan(root: PathBuf, progress: Arc<Progress>, options: &ScanOptions) -> ScanResult {
    let start = Instant::now();
    let tree = with_threads(options.threads, || {
        let mut sub = scan_detached(&root, &root.to_string_lossy(), &progress, options);
        sub.root_mut().set_expanded(true);
        Tree::new(sub, root.clone())
    });
    ScanResult {
        tree,
        elapsed: start.elapsed(),
        cancelled: progress.cancelled(),
    }
}

/// Scan `root` into `live`, which the caller can read while the scan runs.
///
/// Each finished top-level directory is moved into `live` as soon as its
/// subtree is complete, so the UI can show and browse it before the scan
/// ends. The root's own files are inserted up front. The final tree is the
/// same object `live` holds: nothing is built twice.
pub fn scan_live(
    root: PathBuf,
    progress: Arc<Progress>,
    live: Arc<RwLock<Tree>>,
    options: &ScanOptions,
) -> LiveScanResult {
    let start = Instant::now();
    let name = root.to_string_lossy();
    with_threads(options.threads, || {
        // One tracker across all top-level folders, so hard links are still
        // counted only once when their two names live in different folders.
        let links = LinkTracker::default();
        let visited = Mutex::new(HashSet::new());
        let ctx = ScanContext::for_scan(&root, options, &visited);
        let Listing { node, subdirs } = if progress.cancelled() {
            Listing {
                node: DirBuilder::new(0),
                subdirs: Vec::new(),
            }
        } else {
            list_one(&root, 0, &progress, &links, &ctx, &ctx.root_info)
        };
        let mut arena = Arena::default();
        let root_node = node.commit(&mut arena);
        let mut sub = match &ctx.owners {
            Some(owners) => {
                let root_owner = owners.owner_of_root(&root);
                arena.set_owners(owners.snapshot());
                Subtree::new_owned(arena, &name, root_node, root_owner)
            }
            None => Subtree::new(arena, &name, root_node),
        };
        sub.root_mut().set_expanded(true);
        // Keep the category table the caller set on the empty tree (the UI
        // applies `[categories]` overrides there before the scan starts).
        let categories = live.read().categories().clone();
        let mut tree = Tree::new(sub, root.clone());
        tree.set_category_map(categories);
        *live.write() = tree;

        subdirs.into_par_iter().for_each(|sub| {
            let child_path = root.join(&*sub.name);
            let volume = sub.volume.as_deref().unwrap_or(&ctx.root_info);
            let arena = Mutex::new(Arena::default());
            let child = scan_dir(
                &child_path,
                sub.modified,
                &progress,
                &links,
                &arena,
                &ctx,
                volume,
            );
            let mut arena = arena.into_inner();
            let sub = match &ctx.owners {
                Some(owners) => {
                    arena.set_owners(owners.snapshot());
                    Subtree::new_owned(arena, &sub.name, child, sub.owner)
                }
                None => Subtree::new(arena, &sub.name, child),
            };
            live.write().adopt(&[], sub);
        });

        // Extension ranks were assigned as files arrived; rank them properly
        // now that the totals are final. This also bumps the generation so the
        // UI redraws the last partial image with the final colours.
        let mut tree = live.write();
        tree.rebuild_extensions();
        tree.generation = tree.generation.wrapping_add(1);
    });
    LiveScanResult {
        elapsed: start.elapsed(),
        cancelled: progress.cancelled(),
    }
}

/// Scan a single directory into a detached subtree (used after a move so the
/// destination reflects reality).
pub fn scan_subtree(path: &Path, options: &ScanOptions) -> Subtree {
    let progress = Progress::default();
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    with_threads(options.threads, || {
        scan_detached(path, &name, &progress, options)
    })
}

/// Scan `path` into a new arena. The hard-link tracker only lives for the
/// scan itself, so it is freed before the caller builds anything on top.
fn scan_detached(path: &Path, name: &str, progress: &Progress, options: &ScanOptions) -> Subtree {
    let arena = Mutex::new(Arena::default());
    let owners;
    let root = {
        let links = LinkTracker::default();
        let visited = Mutex::new(HashSet::new());
        let ctx = ScanContext::for_scan(path, options, &visited);
        owners = ctx
            .owners
            .as_ref()
            .map(|collector| (collector.owner_of_root(path), collector.snapshot()));
        scan_dir(path, 0, progress, &links, &arena, &ctx, &ctx.root_info)
    };
    let mut arena = arena.into_inner();
    match owners {
        Some((root_owner, table)) => {
            arena.set_owners(table);
            Subtree::new_owned(arena, name, root, root_owner)
        }
        None => Subtree::new(arena, name, root),
    }
}

/// Allocated size of a single file (compressed/sparse aware where the OS
/// exposes it). Used for files that were moved into the tree without a scan.
#[cfg(windows)]
pub fn allocated_size(path: &Path) -> u64 {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE,
    };

    let meta = std::fs::metadata(path).ok();
    let logical = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let attrs = meta.as_ref().map(|m| m.file_attributes()).unwrap_or(0);
    let cluster = cluster_size(path);
    // WOF-compressed files (CompactOS) expose no attribute at all: their
    // compressed size is only visible through `GetCompressedFileSizeW`.
    if attrs
        & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE | FILE_ATTRIBUTE_REPARSE_POINT)
        != 0
        && let Some(size) = compressed_size(path)
    {
        return size;
    }
    if let Some(size) = compressed_size(path)
        && size.saturating_add(cluster) <= logical
    {
        return size;
    }
    round_up(logical, cluster)
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

/// A sub-directory found while listing its parent, to be scanned next.
struct Subdir {
    name: Box<str>,
    modified: i64,
    /// The volume of a followed junction's target; `None` means the same
    /// volume as the parent.
    volume: Option<Arc<Volume>>,
    /// Owner index in the scan's table ([`NO_OWNER`] unless collected).
    owner: u16,
}

/// Entries listed for one directory, before its sub-directories are scanned.
struct Listing {
    node: DirBuilder,
    subdirs: Vec<Subdir>,
}

/// List one directory's entries into a [`DirBuilder`] and collect its
/// sub-directories for the caller to fan out. Applies the scan policy:
/// excluded and hidden entries are dropped, and junctions are only descended
/// into when the settings allow it (with loop and volume protection).
fn list_one(
    path: &Path,
    modified: i64,
    progress: &Progress,
    links: &LinkTracker,
    ctx: &ScanContext,
    volume: &Volume,
) -> Listing {
    let mut node = DirBuilder::new(modified);
    if let Some(mut cur) = progress.current.try_lock() {
        cur.clear();
        cur.push_str(&path.to_string_lossy());
    }

    let options = ctx.options;
    let mut subdirs: Vec<Subdir> = Vec::new();
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    // One owner query per listed entry, and only when the scan asked for it.
    let owner_of = |name: &str, uid: u32| match &ctx.owners {
        Some(collector) if options.collect_owners => {
            #[cfg(windows)]
            {
                collector.owner(&path.join(name), uid)
            }
            // Unix gets the uid from the listing, so no path is needed (and
            // no allocation per entry).
            #[cfg(not(windows))]
            {
                let _ = name;
                collector.owner(Path::new(""), uid)
            }
        }
        _ => NO_OWNER,
    };
    let ok = list_dir(path, volume, |entry| match entry {
        Entry::File {
            name,
            size,
            disk_size,
            modified,
            file_id,
            hidden,
            uid,
        } => {
            if hidden && options.skip_hidden_system {
                return;
            }
            if options.is_excluded(&name) {
                return;
            }
            files += 1;
            bytes += size;
            let owner = owner_of(&name, uid);
            // A zero-length file shares no blocks, so its link count is
            // irrelevant; skip it to keep the tracker small.
            if size > 0 && links.is_duplicate(file_id) {
                node.add_hard_link_owned(&name, modified, owner);
            } else {
                node.add_file_owned(&name, size, disk_size, modified, owner);
            }
        }
        Entry::Dir {
            name,
            modified,
            hidden,
            uid,
        } => {
            if hidden && options.skip_hidden_system {
                return;
            }
            if options.is_excluded(&name) {
                return;
            }
            let owner = owner_of(&name, uid);
            // On Windows a mount point is a reparse point (`Entry::Link`), so a
            // plain directory is always on its parent's volume: skip the check.
            if cfg!(not(windows)) && !options.cross_volumes && !ctx.same_volume(&path.join(&*name))
            {
                // A mount point on another volume: list it but do not enter.
                node.add_empty_dir_owned(&name, modified, owner);
            } else {
                subdirs.push(Subdir {
                    name,
                    modified,
                    volume: None,
                    owner,
                });
            }
        }
        Entry::Link {
            name,
            modified,
            hidden,
            uid,
        } => {
            if hidden && options.skip_hidden_system {
                return;
            }
            if options.is_excluded(&name) {
                return;
            }
            let owner = owner_of(&name, uid);
            // Junctions / symlinked dirs: list them but never follow unless
            // the user opted in (with a warning about double counting). Even
            // then each target is entered at most once, so a loop (A -> B ->
            // A) terminates instead of scanning forever.
            let target = if options.follow_junctions {
                ctx.follow_dir(path, &name)
            } else {
                None
            };
            match target {
                Some(target) => subdirs.push(Subdir {
                    name,
                    modified,
                    volume: Some(target),
                    owner,
                }),
                None => node.add_empty_dir_owned(&name, modified, owner),
            }
        }
    });
    if !ok {
        node.set_error();
        progress.errors.fetch_add(1, Ordering::Relaxed);
    }
    progress.files.fetch_add(files, Ordering::Relaxed);
    progress.bytes.fetch_add(bytes, Ordering::Relaxed);
    progress
        .dirs
        .fetch_add(subdirs.len() as u64, Ordering::Relaxed);
    Listing { node, subdirs }
}

/// Scan one directory (and, in parallel, everything below it). Each
/// directory's children are stored in `arena` as soon as they are final.
/// Returns the directory's node; its parent supplies the name.
fn scan_dir(
    path: &Path,
    modified: i64,
    progress: &Progress,
    links: &LinkTracker,
    arena: &Mutex<Arena>,
    ctx: &ScanContext,
    volume: &Volume,
) -> Node {
    if progress.cancelled() {
        return DirBuilder::new(modified).commit(&mut arena.lock());
    }
    let Listing { mut node, subdirs } = list_one(path, modified, progress, links, ctx, volume);

    if !subdirs.is_empty() {
        let children: Vec<(Box<str>, Node, u16)> = subdirs
            .into_par_iter()
            .map(|sub| {
                let child_path = path.join(&*sub.name);
                let child_volume = sub.volume.as_deref().unwrap_or(volume);
                let child = scan_dir(
                    &child_path,
                    sub.modified,
                    progress,
                    links,
                    arena,
                    ctx,
                    child_volume,
                );
                (sub.name, child, sub.owner)
            })
            .collect();
        for (name, child, owner) in children {
            node.add_dir_owned(&name, child, owner);
        }
    }
    // Sort outside the lock; only the copy into the arena is serialised.
    node.finish();
    node.commit(&mut arena.lock())
}

enum Entry {
    File {
        name: Box<str>,
        size: u64,
        disk_size: u64,
        modified: i64,
        /// Unique per file (shared by hard links), or 0 when unknown.
        file_id: u128,
        /// Hidden or system (Windows attributes, dot-file on Unix).
        hidden: bool,
        /// Owning uid on Unix (`u32::MAX` when unknown); unused on Windows,
        /// where the owner is queried from the path.
        uid: u32,
    },
    Dir {
        name: Box<str>,
        modified: i64,
        /// Hidden or system (Windows attributes, dot-file on Unix).
        hidden: bool,
        uid: u32,
    },
    Link {
        name: Box<str>,
        modified: i64,
        /// Hidden or system (Windows attributes, dot-file on Unix).
        hidden: bool,
        uid: u32,
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
        // A volume without a drive letter (e.g. the canonical target of a
        // junction into a folder mount): its root is `\\?\Volume{GUID}\`.
        if rest.starts_with("Volume{")
            && let Some(end) = rest.find('\\')
        {
            return format!(r"\\?\{}\", &rest[..end]);
        }
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
/// that lack `FileIdExtdDirectoryInfo` working; once a volume has rejected the
/// class, its folders skip straight to the fallback.
#[cfg(windows)]
fn list_dir(path: &Path, volume: &Volume, mut f: impl FnMut(Entry)) -> bool {
    if volume.by_id.load(Ordering::Relaxed) {
        match list_dir_by_id(path, volume.cluster, &mut f) {
            ById::Listed => return true,
            ById::Unsupported => volume.by_id.store(false, Ordering::Relaxed),
            ById::Failed => {}
        }
    }
    list_dir_find(path, volume.cluster, f)
}

/// Outcome of a [`list_dir_by_id`] attempt.
#[cfg(windows)]
enum ById {
    /// The directory was listed (possibly empty).
    Listed,
    /// The filesystem does not support the information class.
    Unsupported,
    /// Anything else (access denied, vanished folder, ...): nothing was
    /// listed, so the caller may retry with another API.
    Failed,
}

/// 128 KiB listing buffer, as `u64`s so the `FILE_ID_EXTD_DIR_INFO` casts
/// are 8-byte aligned.
#[cfg(windows)]
const LIST_BUF_WORDS: usize = 16 * 1024;

#[cfg(windows)]
thread_local! {
    /// One listing buffer per scan thread, reused for every directory. A
    /// fresh zeroed 128 KiB allocation per folder bypasses the Windows
    /// low-fragmentation heap and was a measurable cost.
    static LIST_BUF: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[cfg(windows)]
fn list_dir_by_id(path: &Path, cluster: u64, f: &mut impl FnMut(Entry)) -> ById {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
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
        return ById::Failed;
    }

    let result = LIST_BUF.with(|cell| match cell.try_borrow_mut() {
        Ok(mut buf) => {
            if buf.len() < LIST_BUF_WORDS {
                buf.resize(LIST_BUF_WORDS, 0);
            }
            read_by_id(handle, &mut buf, path, cluster, f)
        }
        // Only reachable if a callback ever lists another directory while
        // this one is being read; stay correct with a private buffer.
        Err(_) => read_by_id(handle, &mut vec![0u64; LIST_BUF_WORDS], path, cluster, f),
    });
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(windows)]
fn read_by_id(
    handle: windows_sys::Win32::Foundation::HANDLE,
    buf: &mut [u64],
    path: &Path,
    cluster: u64,
    f: &mut impl FnMut(Entry),
) -> ById {
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES,
        ERROR_NOT_SUPPORTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_SYSTEM,
        FILE_ID_EXTD_DIR_INFO, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo,
        GetFileInformationByHandleEx,
    };

    let buf_bytes = std::mem::size_of_val(buf);
    let mut restart = true;
    loop {
        // The first call uses the restart class so enumeration starts at the
        // beginning of the directory.
        let class = if restart {
            FileIdExtdDirectoryRestartInfo
        } else {
            FileIdExtdDirectoryInfo
        };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                class,
                buf.as_mut_ptr() as *mut _,
                buf_bytes as u32,
            )
        };
        if ok == 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
            if code == ERROR_NO_MORE_FILES {
                // An empty (or finished) directory is a success.
                return ById::Listed;
            }
            if !restart {
                // Entries were already delivered; keep what was listed.
                return ById::Listed;
            }
            return match code {
                ERROR_INVALID_PARAMETER
                | ERROR_INVALID_LEVEL
                | ERROR_NOT_SUPPORTED
                | ERROR_INVALID_FUNCTION => ById::Unsupported,
                _ => ById::Failed,
            };
        }
        restart = false;

        let base = buf.as_ptr() as *const u8;
        let mut offset = 0usize;
        loop {
            let info = unsafe { &*(base.add(offset) as *const FILE_ID_EXTD_DIR_INFO) };
            let len = info.FileNameLength as usize / 2;
            let name_w = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), len) };
            let is_dot = name_w == [b'.' as u16] || name_w == [b'.' as u16, b'.' as u16];
            if !is_dot {
                let name: Box<str> = String::from_utf16_lossy(name_w).into_boxed_str();
                let modified = filetime_i64_to_unix(info.LastWriteTime);
                let attrs = info.FileAttributes;
                let hidden = attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
                if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        f(Entry::Link {
                            name,
                            modified,
                            hidden,
                            uid: 0,
                        });
                    } else {
                        f(Entry::Dir {
                            name,
                            modified,
                            hidden,
                            uid: 0,
                        });
                    }
                } else {
                    let size = info.EndOfFile.max(0) as u64;
                    let alloc = info.AllocationSize.max(0) as u64;
                    // `AllocationSize` is exact for compressed/sparse files.
                    // For ordinary files it can be the tiny MFT-resident size,
                    // so round the logical size up to the cluster instead.
                    // WOF (CompactOS) files expose neither flag: they report
                    // an allocation size of 0 and keep the compressed size in
                    // `GetCompressedFileSizeW`.
                    let special = attrs
                        & (FILE_ATTRIBUTE_COMPRESSED
                            | FILE_ATTRIBUTE_SPARSE_FILE
                            | FILE_ATTRIBUTE_REPARSE_POINT)
                        != 0
                        || (alloc == 0 && size > 0);
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
                    // On NTFS the low 64 bits are the file reference number;
                    // ReFS uses all 128. All-ones is `FILE_INVALID_FILE_ID`.
                    let file_id = match u128::from_le_bytes(info.FileId.Identifier) {
                        u128::MAX => 0,
                        id => id,
                    };
                    f(Entry::File {
                        name,
                        size,
                        disk_size,
                        modified,
                        file_id,
                        hidden,
                        uid: 0,
                    });
                }
            }
            let next = info.NextEntryOffset as usize;
            if next == 0 || offset + next >= buf_bytes {
                break;
            }
            offset += next;
        }
    }
}

#[cfg(windows)]
fn list_dir_find(path: &Path, cluster: u64, mut f: impl FnMut(Entry)) -> bool {
    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_SYSTEM,
        FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic, FindExSearchNameMatch,
        FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
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
                let hidden = attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
                if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        f(Entry::Link {
                            name,
                            modified,
                            hidden,
                            uid: 0,
                        });
                    } else {
                        f(Entry::Dir {
                            name,
                            modified,
                            hidden,
                            uid: 0,
                        });
                    }
                } else {
                    let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
                    let disk_size = if attrs
                        & (FILE_ATTRIBUTE_COMPRESSED
                            | FILE_ATTRIBUTE_SPARSE_FILE
                            | FILE_ATTRIBUTE_REPARSE_POINT)
                        != 0
                    {
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
                        hidden,
                        uid: 0,
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
fn list_dir(path: &Path, _volume: &Volume, mut f: impl FnMut(Entry)) -> bool {
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
                    if m.nlink() > 1 { m.ino() as u128 } else { 0 },
                ),
                None => (size, 0),
            }
        };
        #[cfg(not(unix))]
        let (disk_size, file_id) = (size, 0);
        // The owner uid comes for free from the metadata above.
        #[cfg(unix)]
        let uid = {
            use std::os::unix::fs::MetadataExt;
            meta.as_ref().map(|m| m.uid()).unwrap_or(u32::MAX)
        };
        #[cfg(not(unix))]
        let uid = 0;
        let hidden = name.starts_with('.');
        if ft.is_symlink() {
            f(Entry::Link {
                name,
                modified,
                hidden,
                uid,
            });
        } else if ft.is_dir() {
            f(Entry::Dir {
                name,
                modified,
                hidden,
                uid,
            });
        } else {
            f(Entry::File {
                name,
                size,
                disk_size,
                modified,
                file_id,
                hidden,
                uid,
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

        let result = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        assert!(!result.cancelled);
        assert_eq!(result.tree.root().files, 3);
        assert_eq!(result.tree.root().dirs, 1);
        assert_eq!(result.tree.root().size, 15);
        assert!(result.tree.root().expanded());
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

        let result = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
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

        let result = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        let tree = &result.tree;
        assert_eq!(tree.root().files, 2);
        assert_eq!(tree.root().size, 4096);
        let links: Vec<_> = tree
            .children(tree.root())
            .iter()
            .filter(|n| n.hard_link())
            .collect();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].size, 0);
        assert_eq!(links[0].disk_size, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn owners_are_collected_only_when_asked() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 10]).unwrap();

        let plain = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        assert!(!plain.tree.has_owners());

        let options = ScanOptions {
            collect_owners: true,
            ..ScanOptions::default()
        };
        let owned = scan(dir.clone(), Arc::new(Progress::default()), &options);
        let tree = &owned.tree;
        assert!(tree.has_owners(), "owners were collected");
        // Every file got an owner and the per-user totals add up.
        let totals = tree.owner_totals();
        assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), 15);
        assert_eq!(totals.iter().map(|t| t.files).sum::<u64>(), 2);
        let a = tree.find(&dir.join("a.txt")).unwrap();
        assert_ne!(tree.owner_at(&a), crate::model::NO_OWNER);
        assert!(tree.owner_name(tree.owner_at(&a)).is_some());
        // The root folder's own owner is recorded too.
        assert!(tree.owner_name(tree.root_owner()).is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_live_keeps_owner_indices_aligned_across_folders() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("one")).unwrap();
        std::fs::write(dir.join("one/a.bin"), vec![0u8; 7]).unwrap();
        std::fs::create_dir(dir.join("two")).unwrap();
        std::fs::write(dir.join("two/b.bin"), vec![0u8; 11]).unwrap();

        let options = ScanOptions {
            collect_owners: true,
            ..ScanOptions::default()
        };
        let live = Arc::new(RwLock::new(Tree::empty(dir.clone())));
        scan_live(
            dir.clone(),
            Arc::new(Progress::default()),
            live.clone(),
            &options,
        );
        let tree = live.read();
        assert!(tree.has_owners());
        // Both files must resolve to the same account (the test user), so
        // the indices from the separate top-level arenas line up.
        let a = tree.find(&dir.join("one/a.bin")).unwrap();
        let b = tree.find(&dir.join("two/b.bin")).unwrap();
        assert_eq!(tree.owner_at(&a), tree.owner_at(&b));
        let totals = tree.owner_totals();
        assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), 18);
        drop(tree);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_subtree_returns_a_detached_node() {
        let dir = temp_dir();
        std::fs::write(dir.join("one.txt"), b"1234").unwrap();
        let sub = scan_subtree(&dir, &ScanOptions::default());
        let node = sub.root();
        assert!(node.is_dir());
        assert_eq!(node.files, 1);
        assert_eq!(node.size, 4);
        assert_eq!(sub.name(&sub.children(node)[0]), "one.txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_live_publishes_the_whole_tree() {
        let dir = temp_dir();
        std::fs::write(dir.join("root.txt"), b"12345").unwrap();
        std::fs::create_dir(dir.join("one")).unwrap();
        std::fs::write(dir.join("one/a.bin"), vec![0u8; 7]).unwrap();
        std::fs::create_dir(dir.join("two")).unwrap();
        std::fs::write(dir.join("two/b.bin"), vec![0u8; 11]).unwrap();

        let live = Arc::new(RwLock::new(Tree::empty(dir.clone())));
        let categories = crate::categories::CategoryMap::with_overrides([(
            "video".to_string(),
            vec!["bin".to_string()],
        )]);
        live.write().set_category_map(categories);
        let result = scan_live(
            dir.clone(),
            Arc::new(Progress::default()),
            live.clone(),
            &ScanOptions::default(),
        );
        assert!(!result.cancelled);
        let tree = live.read();
        assert_eq!(tree.root().files, 3);
        assert_eq!(tree.root().dirs, 2);
        assert_eq!(tree.root().size, 23);
        assert!(tree.root().expanded());
        assert!(tree.find(&dir.join("one/a.bin")).is_some());
        assert!(tree.find(&dir.join("two/b.bin")).is_some());
        assert!(tree.find(&dir.join("root.txt")).is_some());
        assert_eq!(&*tree.exts[0].name, ".bin");
        assert_eq!((tree.exts[0].bytes, tree.exts[0].files), (18, 2));
        assert_eq!(&*tree.exts[1].name, ".txt");
        // The caller's category table survives the live tree being rebuilt.
        assert_eq!(
            tree.exts[0].category,
            crate::categories::FileCategory::Video
        );
        assert!(tree.generation >= 2);
        drop(tree);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_live_counts_cross_folder_hard_links_once() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("one")).unwrap();
        std::fs::create_dir(dir.join("two")).unwrap();
        let original = dir.join("one/original.bin");
        std::fs::write(&original, vec![0u8; 4096]).unwrap();
        if std::fs::hard_link(&original, dir.join("two/link.bin")).is_err() {
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }

        let live = Arc::new(RwLock::new(Tree::empty(dir.clone())));
        scan_live(
            dir.clone(),
            Arc::new(Progress::default()),
            live.clone(),
            &ScanOptions::default(),
        );
        let tree = live.read();
        assert_eq!(tree.root().files, 2);
        assert_eq!(tree.root().size, 4096);
        let links = tree
            .children(tree.root())
            .iter()
            .flat_map(|dir| tree.children(dir))
            .filter(|n| n.hard_link())
            .count();
        assert_eq!(links, 1);
        drop(tree);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let progress = Arc::new(Progress::default());
        progress.cancel.store(true, Ordering::Relaxed);
        let result = scan(dir.clone(), progress, &ScanOptions::default());
        assert!(result.cancelled);
        assert_eq!(result.tree.root().files, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_directory_is_marked_as_an_error() {
        let dir = temp_dir().join("does-not-exist");
        let result = scan(dir, Arc::new(Progress::default()), &ScanOptions::default());
        assert!(result.tree.root().error());
    }

    #[test]
    fn glob_matching_is_case_insensitive_with_wildcards() {
        assert!(matches_glob("$Recycle.Bin", "$recycle.bin"));
        assert!(matches_glob("*.tmp", "cache.TMP"));
        assert!(matches_glob("node_*", "node_modules"));
        assert!(matches_glob("node_?", "node_X"));
        assert!(!matches_glob("node_?", "node_"));
        assert!(!matches_glob("node_?", "node_modules"));
        assert!(matches_glob("*", "anything at all"));
        assert!(!matches_glob("*.git", ".gits"));
        assert!(matches_glob("System*", "system volume information"));
        assert!(!matches_glob("abc", "abcd"));
    }

    #[test]
    fn excluded_names_are_skipped_everywhere() {
        let dir = temp_dir();
        std::fs::write(dir.join("keep.txt"), b"12345").unwrap();
        std::fs::create_dir(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules/skip.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("scratch.tmp"), vec![0u8; 7]).unwrap();

        let options = ScanOptions {
            excluded_globs: vec!["node_modules".into(), "*.tmp".into()],
            ..ScanOptions::default()
        };
        let result = scan(dir.clone(), Arc::new(Progress::default()), &options);
        assert_eq!(result.tree.root().files, 1);
        assert_eq!(result.tree.root().size, 5);
        assert!(result.tree.find(&dir.join("keep.txt")).is_some());
        assert!(result.tree.find(&dir.join("node_modules")).is_none());
        assert!(result.tree.find(&dir.join("scratch.tmp")).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hidden_files_are_skipped_only_when_asked() {
        let dir = temp_dir();
        std::fs::write(dir.join("visible.txt"), b"1234").unwrap();
        std::fs::write(dir.join(".hidden"), b"12").unwrap();
        #[cfg(windows)]
        {
            // A dot-prefix is not hidden on Windows; set the attribute.
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
            let mut wide: Vec<u16> = dir.join(".hidden").as_os_str().encode_wide().collect();
            wide.push(0);
            unsafe {
                SetFileAttributesW(wide.as_ptr(), 0x2);
            }
        }

        let plain = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        assert_eq!(plain.tree.root().files, 2);

        let options = ScanOptions {
            skip_hidden_system: true,
            ..ScanOptions::default()
        };
        let hidden = scan(dir.clone(), Arc::new(Progress::default()), &options);
        assert_eq!(hidden.tree.root().files, 1);
        assert!(hidden.tree.find(&dir.join("visible.txt")).is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn followed_symlinks_are_entered_once_and_never_loop() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/a.bin"), vec![0u8; 32]).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        // A loop back to the scan root must terminate.
        std::os::unix::fs::symlink(&dir, dir.join("real/back")).unwrap();

        // Default: links are listed but never followed.
        let plain = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        assert_eq!(plain.tree.root().size, 32);

        let options = ScanOptions {
            follow_junctions: true,
            ..ScanOptions::default()
        };
        let followed = scan(dir.clone(), Arc::new(Progress::default()), &options);
        // Entered exactly once through the alias; the loop back to the root
        // is recognised as already visited.
        assert_eq!(followed.tree.root().size, 64);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fixed_thread_count_still_scans_everything() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.bin"), vec![0u8; 9]).unwrap();

        let options = ScanOptions {
            threads: Some(2),
            ..ScanOptions::default()
        };
        let result = scan(dir.clone(), Arc::new(Progress::default()), &options);
        assert_eq!(result.tree.root().files, 1);
        assert_eq!(result.tree.root().size, 9);
        assert_eq!(default_thread_count().clamp(1, 64), default_thread_count());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Reference walk with only std::fs: regular files add their length,
    /// directories recurse, symlinks / junctions are counted as a single
    /// empty directory (like the scanner) and never followed.
    fn walk_reference(root: &Path) -> (u64, u32, u32) {
        fn walk(dir: &Path, size: &mut u64, files: &mut u32, dirs: &mut u32) {
            let entries = match std::fs::read_dir(dir) {
                Ok(it) => it,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(ft) = entry.file_type() else { continue };
                if ft.is_symlink() {
                    // Scanner lists links as empty dirs and never descends.
                    *dirs += 1;
                    continue;
                }
                if ft.is_dir() {
                    *dirs += 1;
                    walk(&path, size, files, dirs);
                } else if ft.is_file() {
                    *files += 1;
                    *size += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        let (mut size, mut files, mut dirs) = (0u64, 0u32, 0u32);
        walk(root, &mut size, &mut files, &mut dirs);
        (size, files, dirs)
    }

    #[test]
    fn scan_totals_match_std_walk() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("b.bin"), vec![7u8; 100]).unwrap();
        std::fs::write(dir.join("empty.txt"), b"").unwrap();
        std::fs::create_dir_all(dir.join("sub/nested")).unwrap();
        std::fs::write(dir.join("sub/c.txt"), vec![1u8; 10]).unwrap();
        std::fs::write(dir.join("sub/nested/d.log"), vec![2u8; 7]).unwrap();
        std::fs::create_dir(dir.join("emptydir")).unwrap();

        let result = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        let (size, files, dirs) = walk_reference(&dir);
        assert_eq!(size, 5 + 100 + 0 + 10 + 7);
        assert_eq!(files, 5);
        // sub, sub/nested, emptydir.
        assert_eq!(dirs, 3);

        let root = result.tree.root();
        assert_eq!(root.size, size);
        assert_eq!(root.files, files);
        assert_eq!(root.dirs, dirs);
        // Every reference file is in the tree with its byte length.
        for rel in [
            "a.txt",
            "b.bin",
            "empty.txt",
            "sub/c.txt",
            "sub/nested/d.log",
        ] {
            let p = result
                .tree
                .find(&dir.join(rel))
                .unwrap_or_else(|| panic!("missing {rel}"));
            let node = result.tree.get(&p).unwrap();
            let expected = std::fs::metadata(dir.join(rel)).unwrap().len();
            assert_eq!(node.size, expected, "{rel}");
        }
        // Empty locations exist and are empty.
        assert_eq!(result.tree.find(&dir.join("emptydir")).unwrap().len(), 1);
        let empty = result.tree.find(&dir.join("empty.txt")).unwrap();
        assert_eq!(result.tree.get(&empty).unwrap().size, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(unix)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(windows)]
    fn symlink_dir(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(original, link)
    }

    #[cfg(windows)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(original, link)
    }

    #[cfg(not(any(unix, windows)))]
    fn symlink_dir(_original: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no symlinks",
        ))
    }

    #[cfg(not(any(unix, windows)))]
    fn symlink_file(_original: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no symlinks",
        ))
    }

    #[test]
    fn symlinks_and_junctions_are_listed_but_not_followed() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/real.bin"), vec![3u8; 32]).unwrap();
        std::fs::write(dir.join("file_target.txt"), b"1234567890123").unwrap();

        // Privilege-restricted filesystems (Windows without developer mode)
        // cannot create symlinks: skip rather than fail.
        if symlink_dir(&dir.join("real"), &dir.join("link_to_real")).is_err() {
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        if symlink_file(&dir.join("file_target.txt"), &dir.join("link_to_file.txt")).is_err() {
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }

        let result = scan(
            dir.clone(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        );
        let tree = &result.tree;
        // Only real bytes are counted; links contribute nothing (a file symlink
        // is listed but never adds bytes, so the total never double-counts).
        assert_eq!(tree.root().size, 32 + 13);
        let (size, files, dirs) = walk_reference(&dir);
        assert_eq!(tree.root().size, size);
        // Windows lists a file symlink as a (zero-byte) file while Unix lists
        // it as an empty dir entry; both count one item, so compare totals.
        assert_eq!(
            tree.root().files + tree.root().dirs,
            files + dirs,
            "files={} dirs={} vs ref files={} dirs={}",
            tree.root().files,
            tree.root().dirs,
            files,
            dirs
        );
        // Real content is found exactly once (no double count through the link).
        assert!(tree.find(&dir.join("real/real.bin")).is_some());
        assert!(tree.find(&dir.join("link_to_real/real.bin")).is_none());
        // Links are listed as empty entries with no children.
        for link in ["link_to_real", "link_to_file.txt"] {
            let p = tree
                .find(&dir.join(link))
                .unwrap_or_else(|| panic!("missing {link}"));
            let node = tree.get(&p).unwrap();
            assert_eq!(node.size, 0, "{link}");
            assert!(tree.children(node).is_empty(), "{link}");
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

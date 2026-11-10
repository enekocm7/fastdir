//! Find groups of byte-identical files under a scanned tree.
//!
//! Hashing is expensive, so the work is narrowed down in three stages:
//!
//! 1. group the files of the tree by size (free: the scanner already knows it);
//! 2. hash the first and last 64 KB of every file in a group of two or more;
//! 3. only for the files that still match, hash the whole file (BLAKE3).
//!
//! Zero-byte files are skipped (they waste nothing) and so are extra hard
//! links: a scan already marks all but the first link to a file, and those
//! bytes are counted once.
//!
//! Everything here is GUI-free and cancellable, so the CLI, the MCP server
//! and the GUI's Duplicates tab share it. The GUI uses [`collect`] and
//! [`find_candidates`] separately so the tree lock is only held while the
//! candidates are listed, not while files are hashed.

use crate::model::Tree;
use crate::scanner::with_threads;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Hashing files below this size is not worth the I/O by default.
pub const DEFAULT_MIN_SIZE: u64 = 1024 * 1024;

/// Bytes read from the start and from the end of a file for the first pass.
pub const SAMPLE_BYTES: u64 = 64 * 1024;

/// Read buffer for the whole-file hash.
const READ_CHUNK: usize = 256 * 1024;

/// What to look for and how hard to work.
#[derive(Clone, Debug)]
pub struct DuplicateOptions {
    /// Smallest file to consider. Zero-byte files are always skipped.
    pub min_size: u64,
    /// Only these extensions (`.mp4`, `mp4` and `MP4` all mean `.mp4`).
    /// Empty means every extension.
    pub extensions: Vec<String>,
    /// Most groups to report (`None` = all). `total_groups` and
    /// `total_wasted` still describe every group found.
    pub limit: Option<usize>,
    /// Hash threads, or `None` for the automatic scan pool.
    pub threads: Option<usize>,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            min_size: DEFAULT_MIN_SIZE,
            extensions: Vec::new(),
            limit: None,
            threads: None,
        }
    }
}

/// Live counters for the progress line and Ctrl+C.
#[derive(Default)]
pub struct DuplicateProgress {
    /// Bytes read for hashing (the sample pass counts too).
    pub bytes_hashed: AtomicU64,
    /// Files whose content was hashed at least once.
    pub files_hashed: AtomicU64,
    pub cancel: AtomicBool,
    /// File currently being hashed, for the progress line.
    pub current: Mutex<String>,
}

impl DuplicateProgress {
    #[inline]
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// One copy inside a duplicate group.
#[derive(Clone, Debug)]
pub struct DuplicateFile {
    pub path: PathBuf,
    pub modified: i64,
}

/// Files with the same size and the same content.
#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub size: u64,
    /// BLAKE3 of the whole file, lower-case hexadecimal.
    pub hash: String,
    /// The copies, sorted by path.
    pub files: Vec<DuplicateFile>,
}

impl DuplicateGroup {
    /// Bytes wasted by keeping all but one copy.
    pub fn wasted(&self) -> u64 {
        self.size
            .saturating_mul(self.files.len().saturating_sub(1) as u64)
    }
}

/// The result of [`find`].
#[derive(Clone, Debug, Default)]
pub struct DuplicateReport {
    /// At most [`DuplicateOptions::limit`] groups, wasted bytes first.
    pub groups: Vec<DuplicateGroup>,
    /// Every group found, before the limit was applied.
    pub total_groups: usize,
    /// Reclaimable bytes across every group found.
    pub total_wasted: u64,
    /// Files that passed the size and extension filters.
    pub candidates: u64,
    /// Bytes read while hashing (samples included).
    pub bytes_hashed: u64,
    /// Files that could not be read (deleted, permission denied, ...).
    pub hash_errors: u64,
    pub elapsed: Duration,
    pub cancelled: bool,
}

impl DuplicateReport {
    /// Keep only the copies for which `keep` returns true, dropping groups
    /// left with a single copy and recomputing the totals. Used after files
    /// were deleted, moved or replaced with hard links, so the report keeps
    /// describing what is still reclaimable.
    pub fn retain_files(&mut self, mut keep: impl FnMut(&DuplicateFile) -> bool) {
        let before: u64 = self.groups.iter().map(DuplicateGroup::wasted).sum();
        let groups_before = self.groups.len();
        for group in &mut self.groups {
            group.files.retain(&mut keep);
        }
        self.groups.retain(|group| group.files.len() >= 2);
        let after: u64 = self.groups.iter().map(DuplicateGroup::wasted).sum();
        self.total_wasted = self.total_wasted.saturating_sub(before - after);
        self.total_groups = self
            .total_groups
            .saturating_sub(groups_before - self.groups.len());
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    size: u64,
    modified: i64,
}

/// Files of a tree that passed the size and extension filters, ready to be
/// hashed by [`find_candidates`] without the tree.
#[derive(Clone, Debug, Default)]
pub struct Candidates(Vec<Candidate>);

impl Candidates {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// List the files under `root` in `tree` that [`find_candidates`] should
/// look at. Cheap: no file is opened.
pub fn collect(tree: &Tree, root: &[u32], options: &DuplicateOptions) -> Candidates {
    if !tree.get(root).is_some_and(|node| node.is_dir()) {
        return Candidates::default();
    }
    let extensions = normalise_extensions(&options.extensions);
    Candidates(collect_candidates(tree, root, options, &extensions))
}

/// Find byte-identical files under `root` in `tree`.
pub fn find(
    tree: &Tree,
    root: &[u32],
    options: &DuplicateOptions,
    progress: Arc<DuplicateProgress>,
) -> DuplicateReport {
    let start = Instant::now();
    let candidates = collect(tree, root, options);
    let mut report = find_candidates(candidates, options, progress);
    report.elapsed = start.elapsed();
    report
}

/// Hash `candidates` (from [`collect`]) and group the identical ones.
pub fn find_candidates(
    candidates: Candidates,
    options: &DuplicateOptions,
    progress: Arc<DuplicateProgress>,
) -> DuplicateReport {
    let start = Instant::now();
    let mut report = DuplicateReport::default();
    let candidates = candidates.0;
    report.candidates = candidates.len() as u64;

    // Stage 1: size. A file without a twin of the same size cannot match.
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        by_size.entry(candidate.size).or_default().push(candidate);
    }
    let same_size: Vec<Candidate> = by_size
        .into_values()
        .filter(|group| group.len() >= 2)
        .flatten()
        .collect();
    if same_size.len() < 2 {
        report.elapsed = start.elapsed();
        return report;
    }

    let errors = AtomicU64::new(0);

    // Stage 2: the first and last 64 KB.
    let sampled: Vec<(u64, [u8; 32], Candidate)> = with_threads(options.threads, || {
        same_size
            .par_iter()
            .filter_map(|candidate| {
                match sample_hash(candidate, &progress) {
                    Ok(Some(hash)) => Some((candidate.size, *hash.as_bytes(), candidate.clone())),
                    Ok(None) => None, // cancelled
                    Err(_) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .collect()
    });
    if progress.cancelled() {
        report.cancelled = true;
        report.bytes_hashed = progress.bytes_hashed.load(Ordering::Relaxed);
        report.hash_errors = errors.load(Ordering::Relaxed);
        report.elapsed = start.elapsed();
        return report;
    }

    let mut by_sample: HashMap<(u64, [u8; 32]), Vec<Candidate>> = HashMap::new();
    for (size, hash, candidate) in sampled {
        by_sample.entry((size, hash)).or_default().push(candidate);
    }
    let still_matching: Vec<Candidate> = by_sample
        .into_values()
        .filter(|group| group.len() >= 2)
        .flatten()
        .collect();
    if still_matching.len() < 2 {
        report.bytes_hashed = progress.bytes_hashed.load(Ordering::Relaxed);
        report.hash_errors = errors.load(Ordering::Relaxed);
        report.elapsed = start.elapsed();
        return report;
    }

    // Stage 3: the whole file.
    let full: Vec<(u64, [u8; 32], Candidate)> = with_threads(options.threads, || {
        still_matching
            .par_iter()
            .filter_map(|candidate| match full_hash(candidate, &progress) {
                Ok(Some(hash)) => Some((candidate.size, *hash.as_bytes(), candidate.clone())),
                Ok(None) => None, // cancelled
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    None
                }
            })
            .collect()
    });

    report.bytes_hashed = progress.bytes_hashed.load(Ordering::Relaxed);
    report.hash_errors = errors.load(Ordering::Relaxed);
    if progress.cancelled() {
        report.cancelled = true;
        report.elapsed = start.elapsed();
        return report;
    }

    let mut by_content: HashMap<(u64, [u8; 32]), Vec<Candidate>> = HashMap::new();
    for (size, hash, candidate) in full {
        by_content.entry((size, hash)).or_default().push(candidate);
    }

    let mut groups: Vec<DuplicateGroup> = by_content
        .into_iter()
        .filter(|(_, files)| files.len() >= 2)
        .map(|((size, hash), mut files)| {
            files.sort_unstable_by(|a, b| {
                a.path
                    .to_string_lossy()
                    .to_lowercase()
                    .cmp(&b.path.to_string_lossy().to_lowercase())
            });
            DuplicateGroup {
                size,
                hash: hex(&hash),
                files: files
                    .into_iter()
                    .map(|candidate| DuplicateFile {
                        path: candidate.path,
                        modified: candidate.modified,
                    })
                    .collect(),
            }
        })
        .collect();

    groups.sort_unstable_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.hash.cmp(&b.hash))
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
    report.total_groups = groups.len();
    report.total_wasted = groups.iter().map(DuplicateGroup::wasted).sum();
    if let Some(limit) = options.limit {
        groups.truncate(limit);
    }
    report.groups = groups;
    report.elapsed = start.elapsed();
    report
}

fn collect_candidates(
    tree: &Tree,
    root: &[u32],
    options: &DuplicateOptions,
    extensions: &HashSet<String>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Some(root_node) = tree.get(root) else {
        return out;
    };
    let mut stack: Vec<(Vec<u32>, &crate::model::Node)> = vec![(root.to_vec(), root_node)];
    while let Some((path, dir)) = stack.pop() {
        for (index, child) in tree.children(dir).iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(index as u32);
            if child.is_dir() {
                stack.push((child_path, child));
                continue;
            }
            if child.is_special() || child.hard_link() {
                continue;
            }
            if child.size == 0 || child.size < options.min_size {
                continue;
            }
            if !extensions.is_empty() {
                let Some(ext) = tree.ext_name(child) else {
                    continue;
                };
                if !extensions.contains(ext) {
                    continue;
                }
            }
            if let Some(fs_path) = tree.fs_path(&child_path) {
                out.push(Candidate {
                    path: fs_path,
                    size: child.size,
                    modified: child.modified(),
                });
            }
        }
    }
    out
}

/// Lower-case extensions, each with its leading dot.
fn normalise_extensions(input: &[String]) -> HashSet<String> {
    input
        .iter()
        .filter_map(|raw| {
            let trimmed = raw.trim().rsplit('.').next().unwrap_or("").trim();
            (!trimmed.is_empty()).then(|| format!(".{}", trimmed.to_lowercase()))
        })
        .collect()
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Hash the first and last [`SAMPLE_BYTES`] of a file. `Ok(None)` means the
/// progress flag asked us to stop.
fn sample_hash(
    candidate: &Candidate,
    progress: &DuplicateProgress,
) -> io::Result<Option<blake3::Hash>> {
    if progress.cancelled() {
        return Ok(None);
    }
    set_current(progress, &candidate.path);
    let mut file = File::open(&candidate.path)?;
    if file.metadata()?.len() != candidate.size {
        // The file changed since the scan; its scanned size no longer
        // describes it, so it cannot be compared with its old twins.
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    if !hash_span(
        &mut file,
        0,
        candidate.size.min(SAMPLE_BYTES),
        &mut hasher,
        progress,
    )? {
        return Ok(None);
    }
    if candidate.size > SAMPLE_BYTES {
        let last = candidate.size.min(SAMPLE_BYTES);
        if !hash_span(
            &mut file,
            candidate.size - last,
            last,
            &mut hasher,
            progress,
        )? {
            return Ok(None);
        }
    }
    progress.files_hashed.fetch_add(1, Ordering::Relaxed);
    Ok(Some(hasher.finalize()))
}

/// Hash the whole file. `Ok(None)` means cancelled.
fn full_hash(
    candidate: &Candidate,
    progress: &DuplicateProgress,
) -> io::Result<Option<blake3::Hash>> {
    if progress.cancelled() {
        return Ok(None);
    }
    set_current(progress, &candidate.path);
    let mut file = File::open(&candidate.path)?;
    if file.metadata()?.len() != candidate.size {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    if !hash_span(&mut file, 0, candidate.size, &mut hasher, progress)? {
        return Ok(None);
    }
    // A file that grew since the scan: do not let the extra bytes change the
    // hash silently, just stop at the scanned size. A file that shrank reads
    // shorter than expected and simply will not match its old twin.
    progress.files_hashed.fetch_add(1, Ordering::Relaxed);
    Ok(Some(hasher.finalize()))
}

/// Feed `len` bytes at `offset` into `hasher`, counting them and checking for
/// cancellation between chunks. `Ok(false)` means cancelled.
fn hash_span(
    file: &mut File,
    offset: u64,
    len: u64,
    hasher: &mut blake3::Hasher,
    progress: &DuplicateProgress,
) -> io::Result<bool> {
    if len == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; READ_CHUNK.min(len as usize).max(1)];
    let mut remaining = len;
    while remaining > 0 {
        if progress.cancelled() {
            return Ok(false);
        }
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
        progress
            .bytes_hashed
            .fetch_add(read as u64, Ordering::Relaxed);
    }
    Ok(true)
}

fn set_current(progress: &DuplicateProgress, path: &std::path::Path) {
    if let Some(mut current) = progress.current.try_lock() {
        current.clear();
        current.push_str(&path.to_string_lossy());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{self, Progress, ScanOptions};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("fastdir-duplicates-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scanned(dir: &Path) -> Tree {
        scanner::scan(
            dir.to_path_buf(),
            Arc::new(Progress::default()),
            &ScanOptions::default(),
        )
        .tree
    }

    fn options() -> DuplicateOptions {
        DuplicateOptions {
            min_size: 1,
            ..DuplicateOptions::default()
        }
    }

    fn run(dir: &Path, options: &DuplicateOptions) -> DuplicateReport {
        let tree = scanned(dir);
        find(&tree, &[], options, Arc::new(DuplicateProgress::default()))
    }

    fn names(group: &DuplicateGroup) -> Vec<String> {
        group
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn identical_files_are_one_group() {
        let dir = TempDir::new("identical");
        dir.write("a.bin", b"same content");
        dir.write("b.bin", b"same content");
        dir.write("c.bin", b"other content");
        let report = run(&dir.0, &options());
        assert_eq!(report.total_groups, 1);
        assert_eq!(report.groups.len(), 1);
        let group = &report.groups[0];
        assert_eq!(group.size, 12);
        assert_eq!(group.wasted(), 12);
        assert_eq!(report.total_wasted, 12);
        assert_eq!(names(group), ["a.bin", "b.bin"]);
        assert_eq!(group.hash.len(), 64);
        assert!(group.hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn same_size_different_content_is_not_a_group() {
        let dir = TempDir::new("different");
        dir.write("a.bin", b"aaaaaaaa");
        dir.write("b.bin", b"bbbbbbbb");
        let report = run(&dir.0, &options());
        assert_eq!(report.total_groups, 0);
        assert!(report.groups.is_empty());
        assert_eq!(report.candidates, 2);
        // Stage one hashed both, stage three was never needed.
        assert!(report.bytes_hashed <= 16);
    }

    #[test]
    fn identical_prefix_and_suffix_but_a_different_middle_do_not_match() {
        let dir = TempDir::new("middle");
        let mut a = vec![b'x'; 200 * 1024];
        let mut b = a.clone();
        a[100 * 1024] = b'a';
        b[100 * 1024] = b'b';
        dir.write("a.bin", &a);
        dir.write("b.bin", &b);
        let report = run(&dir.0, &options());
        assert_eq!(
            report.total_groups, 0,
            "the samples match, the contents do not"
        );
        // Both files were fully hashed: the sample pass only reads 128 KB.
        assert!(report.bytes_hashed > 2 * 128 * 1024);
    }

    #[test]
    fn zero_byte_files_are_skipped() {
        let dir = TempDir::new("empty");
        dir.write("a.bin", b"");
        dir.write("b.bin", b"");
        let report = run(
            &dir.0,
            &DuplicateOptions {
                min_size: 0,
                ..DuplicateOptions::default()
            },
        );
        assert_eq!(report.candidates, 0);
        assert_eq!(report.total_groups, 0);
    }

    #[test]
    fn hard_links_count_as_one_file() {
        let dir = TempDir::new("hardlink");
        let original = dir.write("original.bin", b"hard linked content");
        if fs::hard_link(&original, dir.0.join("link.bin")).is_err() {
            return; // no hard links on this file system
        }
        dir.write("copy.bin", b"hard linked content");

        let tree = scanned(&dir.0);
        let links: Vec<_> = tree
            .children(tree.root())
            .iter()
            .filter(|node| node.hard_link())
            .map(|node| tree.name(node).to_string())
            .collect();
        assert_eq!(links.len(), 1, "the scan marks one of the two links");

        let report = find(
            &tree,
            &[],
            &options(),
            Arc::new(DuplicateProgress::default()),
        );
        assert_eq!(report.total_groups, 1);
        let group = &report.groups[0];
        assert_eq!(group.files.len(), 2, "the two links count once");
        let group_names = names(group);
        assert!(group_names.contains(&"copy.bin".to_string()));
        assert_eq!(
            group_names
                .iter()
                .filter(|name| *name == "original.bin" || *name == "link.bin")
                .count(),
            1,
            "only one of the two hard links is listed: {group_names:?}"
        );
    }

    #[test]
    fn min_size_and_extensions_filter_candidates() {
        let dir = TempDir::new("filters");
        dir.write("a.mp4", b"12345678");
        dir.write("b.mp4", b"12345678");
        dir.write("a.txt", b"12345678");
        dir.write("b.txt", b"12345678");
        let report = run(
            &dir.0,
            &DuplicateOptions {
                min_size: 8,
                extensions: vec!["MP4".into(), ".txt".into()],
                ..DuplicateOptions::default()
            },
        );
        assert_eq!(report.candidates, 4);
        assert_eq!(report.total_groups, 1);
        assert_eq!(report.groups[0].files.len(), 4);

        let small = run(
            &dir.0,
            &DuplicateOptions {
                min_size: 9,
                ..DuplicateOptions::default()
            },
        );
        assert_eq!(small.candidates, 0);
        assert_eq!(small.total_groups, 0);

        let only_mp4 = run(
            &dir.0,
            &DuplicateOptions {
                min_size: 1,
                extensions: vec!["mp4".into()],
                ..DuplicateOptions::default()
            },
        );
        assert_eq!(only_mp4.candidates, 2);
        assert_eq!(only_mp4.total_groups, 1);
        assert_eq!(names(&only_mp4.groups[0]), ["a.mp4", "b.mp4"]);
    }

    #[test]
    fn the_group_limit_keeps_the_biggest_but_counts_everything() {
        let dir = TempDir::new("limit");
        dir.write("small-a.bin", &[b's'; 10]);
        dir.write("small-b.bin", &[b's'; 10]);
        dir.write("big-a.bin", &[b'b'; 100]);
        dir.write("big-b.bin", &[b'b'; 100]);
        let report = run(
            &dir.0,
            &DuplicateOptions {
                min_size: 1,
                limit: Some(1),
                ..DuplicateOptions::default()
            },
        );
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].size, 100);
        assert_eq!(report.total_groups, 2);
        assert_eq!(report.total_wasted, 110);
    }

    #[test]
    fn a_subfolder_limits_the_search() {
        let dir = TempDir::new("subtree");
        fs::create_dir_all(dir.0.join("one")).unwrap();
        fs::create_dir_all(dir.0.join("two")).unwrap();
        fs::write(dir.0.join("one").join("a.bin"), b"same").unwrap();
        fs::write(dir.0.join("one").join("b.bin"), b"same").unwrap();
        fs::write(dir.0.join("two").join("c.bin"), b"same").unwrap();
        fs::write(dir.0.join("two").join("d.bin"), b"same").unwrap();
        let tree = scanned(&dir.0);
        let one = tree.find(&dir.0.join("one")).unwrap();
        let report = find(
            &tree,
            &one,
            &options(),
            Arc::new(DuplicateProgress::default()),
        );
        assert_eq!(report.total_groups, 1);
        assert_eq!(names(&report.groups[0]), ["a.bin", "b.bin"]);
    }

    #[test]
    fn collect_then_hash_matches_find() {
        let dir = TempDir::new("split");
        dir.write("a.bin", b"split content");
        dir.write("b.bin", b"split content");
        dir.write("c.bin", b"other");
        let tree = scanned(&dir.0);
        let candidates = collect(&tree, &[], &options());
        assert_eq!(candidates.len(), 3);
        drop(tree);
        let report = find_candidates(
            candidates,
            &options(),
            Arc::new(DuplicateProgress::default()),
        );
        assert_eq!(report.total_groups, 1);
        assert_eq!(names(&report.groups[0]), ["a.bin", "b.bin"]);
    }

    #[test]
    fn retain_files_drops_singletons_and_updates_totals() {
        let dir = TempDir::new("retain");
        dir.write("a1.bin", &[b'a'; 10]);
        dir.write("a2.bin", &[b'a'; 10]);
        dir.write("a3.bin", &[b'a'; 10]);
        dir.write("b1.bin", &[b'b'; 100]);
        dir.write("b2.bin", &[b'b'; 100]);
        let mut report = run(&dir.0, &options());
        assert_eq!(report.total_groups, 2);
        assert_eq!(report.total_wasted, 100 + 20);

        // One extra copy of "a" goes away: the group stays, wasting less.
        report.retain_files(|f| !f.path.ends_with("a3.bin"));
        assert_eq!(report.total_groups, 2);
        assert_eq!(report.total_wasted, 100 + 10);

        // "b" is left with a single copy: the group disappears.
        report.retain_files(|f| !f.path.ends_with("b2.bin"));
        assert_eq!(report.total_groups, 1);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(names(&report.groups[0]), ["a1.bin", "a2.bin"]);
        assert_eq!(report.total_wasted, 10);
    }

    #[test]
    fn cancellation_stops_hashing() {
        let dir = TempDir::new("cancel");
        // Two large files keep the whole-file pass busy long enough for the
        // monitor thread to flip the flag while it runs.
        let big = vec![0u8; 32 * 1024 * 1024];
        dir.write("a.bin", &big);
        dir.write("b.bin", &big);
        let tree = scanned(&dir.0);
        let progress = Arc::new(DuplicateProgress::default());
        let flag = progress.clone();
        let monitor = std::thread::spawn(move || {
            while flag.bytes_hashed.load(Ordering::Relaxed) < 2 * SAMPLE_BYTES {
                std::thread::yield_now();
            }
            flag.cancel.store(true, Ordering::Relaxed);
        });
        let report = find(
            &tree,
            &[],
            &DuplicateOptions {
                min_size: 1,
                threads: Some(1),
                ..DuplicateOptions::default()
            },
            progress.clone(),
        );
        monitor.join().unwrap();
        assert!(report.cancelled);
        assert!(report.groups.is_empty());
        assert!(
            report.bytes_hashed < 2 * 32 * 1024 * 1024,
            "hashing stopped early, got {} bytes",
            report.bytes_hashed
        );
    }
}

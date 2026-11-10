//! Scan history: timestamped snapshots of a scan, their retention, and the
//! size of one folder across them (the "growth over time" series).
//!
//! Layout of a snapshot folder (`base`):
//!
//! ```text
//! base/
//!   <root key>/                 one folder per scanned root
//!     root.txt                  the root's full path
//!     20261110T081500Z.json     compact JSON export (see `export`)
//!     20261111T081500Z.json
//! ```
//!
//! File names carry the UTC scan time, so listing a history never opens the
//! (possibly large) documents. A snapshot is an ordinary JSON export: it can
//! be opened in the GUI, compared with *Compare with a saved scan…* and
//! passed to `fastdir-cli diff`.
//!
//! Reading a folder's size from a snapshot ([`folder_size`]) streams the
//! document and only materialises the nodes on the way to that folder, so a
//! growth series over many whole-drive snapshots needs little memory.

use crate::export::{self, ExportOptions};
use crate::model::Tree;
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Name of the file that records which root a snapshot sub-folder belongs to.
pub const ROOT_FILE: &str = "root.txt";

/// `strftime` pattern of a snapshot file name (without the extension).
const STAMP: &str = "%Y%m%dT%H%M%SZ";

/// One saved snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// The JSON document.
    pub path: PathBuf,
    /// The folder or drive that was scanned.
    pub root: PathBuf,
    /// When it was scanned, Unix seconds (UTC).
    pub scanned_at: i64,
}

/// How many snapshots to keep per root. The newest snapshot is always kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// Keep at most this many (`None` = no limit).
    pub keep_count: Option<usize>,
    /// Remove snapshots older than this many seconds (`None` = no limit).
    pub max_age_secs: Option<i64>,
}

impl Retention {
    /// From the settings' `keep_count` / `max_age_days`, where `0` means
    /// "no limit".
    pub fn from_limits(keep_count: u32, max_age_days: u32) -> Self {
        Self {
            keep_count: (keep_count > 0).then_some(keep_count as usize),
            max_age_secs: (max_age_days > 0).then_some(i64::from(max_age_days) * 86_400),
        }
    }

    /// Whether this retention never removes anything.
    pub fn is_unlimited(&self) -> bool {
        self.keep_count.is_none() && self.max_age_secs.is_none()
    }
}

/// Sizes of one folder (or file) in a snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderSize {
    pub size: u64,
    pub disk_size: u64,
    pub files: u64,
    pub dirs: u64,
}

/// One point of a growth series: a snapshot and the folder's size in it
/// (`None` when the folder did not exist then, or the document could not be
/// read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrowthPoint {
    pub snapshot: SnapshotInfo,
    pub size: Option<FolderSize>,
}

/// Current time, Unix seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The path as compared between snapshots: separators unified, trailing
/// separators dropped (except on a bare root such as `C:\` or `/`) and, on
/// Windows, lower-cased, since paths there are case-insensitive.
fn normalized(path: &Path) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text = text.replace('/', "\\").to_lowercase();
    }
    let sep = std::path::MAIN_SEPARATOR;
    while text.len() > 1 && text.ends_with(sep) && !text.ends_with(&format!(":{sep}")) {
        text.pop();
    }
    if cfg!(windows) && text.len() == 2 && text.ends_with(':') {
        text.push(sep);
    }
    text
}

/// Stable folder name for `root`: a readable slug plus a hash of the
/// normalized path, e.g. `C_Users-1a2b3c4d5e6f7a8b`.
pub fn root_key(root: &Path) -> String {
    let norm = normalized(root);
    // FNV-1a: tiny, stable across versions and platforms.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in norm.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let mut slug = String::new();
    for c in norm.chars() {
        let c = if c.is_alphanumeric() || c == '-' || c == '.' {
            c
        } else {
            '_'
        };
        if c == '_' && slug.ends_with('_') {
            continue;
        }
        slug.push(c);
    }
    // Keep the end of long paths: the folder's own name is the useful part.
    let len = slug.chars().count();
    let slug: String = slug.chars().skip(len.saturating_sub(48)).collect();
    let slug = slug.trim_matches(|c| c == '_' || c == '.');
    let slug = if slug.is_empty() { "root" } else { slug };
    format!("{slug}-{hash:016x}")
}

/// The sub-folder of `base` that holds the snapshots of `root`.
pub fn root_folder(base: &Path, root: &Path) -> PathBuf {
    base.join(root_key(root))
}

/// File name (without folder) for a snapshot taken at `scanned_at`.
pub fn file_name(scanned_at: i64) -> String {
    let stamp = chrono::DateTime::from_timestamp(scanned_at, 0)
        .unwrap_or_default()
        .format(STAMP)
        .to_string();
    format!("{stamp}.json")
}

/// The scan time encoded in a snapshot file name, or `None` for any other
/// file. Accepts the `-2`, `-3`, … suffixes added on collisions.
pub fn parse_file_name(name: &str) -> Option<i64> {
    parse_name(name).map(|(at, _)| at)
}

/// The scan time and the collision suffix (`1` without one) of a snapshot
/// file name.
fn parse_name(name: &str) -> Option<(i64, u32)> {
    let stem = name.strip_suffix(".json")?;
    let (stamp, n) = match stem.split_once('-') {
        Some((stamp, n)) => (stamp, n.parse().ok()?),
        None => (stem, 1),
    };
    chrono::NaiveDateTime::parse_from_str(stamp, STAMP)
        .ok()
        .map(|dt| (dt.and_utc().timestamp(), n))
}

/// Chronological order, then the order the snapshots were saved in.
fn sort_snapshots(list: &mut [SnapshotInfo]) {
    let suffix = |s: &SnapshotInfo| {
        s.path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(parse_name)
            .map_or(0, |(_, n)| n)
    };
    list.sort_by(|a, b| {
        a.scanned_at
            .cmp(&b.scanned_at)
            .then_with(|| suffix(a).cmp(&suffix(b)))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// Save `tree` as a snapshot under `base`, stamped `scanned_at`. Writes to
/// a temporary file first, so an interrupted save never leaves a truncated
/// snapshot behind. Returns the new file.
pub fn save(
    tree: &Tree,
    scanned_at: i64,
    base: &Path,
    options: &ExportOptions,
) -> io::Result<PathBuf> {
    let folder = root_folder(base, &tree.root_path);
    std::fs::create_dir_all(&folder)?;
    let root_file = folder.join(ROOT_FILE);
    if !root_file.exists() {
        std::fs::write(&root_file, tree.root_path.to_string_lossy().as_bytes())?;
    }
    let mut dest = folder.join(file_name(scanned_at));
    let mut n = 2;
    while dest.exists() {
        let stem = file_name(scanned_at);
        let stem = stem.trim_end_matches(".json");
        dest = folder.join(format!("{stem}-{n}.json"));
        n += 1;
    }
    let tmp = dest.with_extension("json.tmp");
    let written = std::fs::File::create(&tmp).and_then(|file| {
        let out = io::BufWriter::with_capacity(1 << 20, file);
        export::export_json_compact(tree, &[], options, scanned_at, out)
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(dest)
}

/// The snapshots in one root sub-folder, oldest first.
fn list_folder(folder: &Path, root: &Path) -> io::Result<Vec<SnapshotInfo>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(scanned_at) = name.to_str().and_then(parse_file_name) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        out.push(SnapshotInfo {
            path: entry.path(),
            root: root.to_path_buf(),
            scanned_at,
        });
    }
    sort_snapshots(&mut out);
    Ok(out)
}

/// The snapshots of `root` under `base`, oldest first. Empty when there are
/// none (including when `base` does not exist).
pub fn list(base: &Path, root: &Path) -> io::Result<Vec<SnapshotInfo>> {
    let folder = root_folder(base, root);
    match list_folder(&folder, root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        other => other,
    }
}

/// Every root with a snapshot folder under `base`, as `(root, folder)`.
pub fn roots(base: &Path) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    let entries = match std::fs::read_dir(base) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let folder = entry.path();
        let Ok(text) = std::fs::read_to_string(folder.join(ROOT_FILE)) else {
            continue;
        };
        let root = PathBuf::from(text.trim());
        if !root.as_os_str().is_empty() {
            out.push((root, folder));
        }
    }
    out.sort();
    Ok(out)
}

/// `target` relative to `root` as name components, or `None` when `target`
/// is not `root` or below it.
pub fn relative_components(root: &Path, target: &Path) -> Option<Vec<String>> {
    let root = PathBuf::from(normalized(root));
    let target = PathBuf::from(normalized(target));
    let rel = target.strip_prefix(&root).ok()?;
    Some(
        rel.components()
            .filter_map(|c| match c {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect(),
    )
}

/// All snapshots that contain `target`: those of `target` itself and of
/// every folder or drive above it, oldest first.
pub fn list_covering(base: &Path, target: &Path) -> io::Result<Vec<SnapshotInfo>> {
    let mut out = Vec::new();
    for (root, folder) in roots(base)? {
        if relative_components(&root, target).is_some() {
            out.extend(list_folder(&folder, &root)?);
        }
    }
    sort_snapshots(&mut out);
    Ok(out)
}

/// Which of `snapshots` (oldest first) `retention` removes at `now`, as
/// indices. The newest snapshot is always kept.
pub fn select_prune(snapshots: &[SnapshotInfo], retention: &Retention, now: i64) -> Vec<usize> {
    let Some(newest) = snapshots.len().checked_sub(1) else {
        return Vec::new();
    };
    let mut keep: Vec<bool> = vec![true; snapshots.len()];
    if let Some(max_age) = retention.max_age_secs {
        let cutoff = now.saturating_sub(max_age);
        for (i, snap) in snapshots.iter().enumerate() {
            if i != newest && snap.scanned_at < cutoff {
                keep[i] = false;
            }
        }
    }
    if let Some(limit) = retention.keep_count {
        let limit = limit.max(1);
        let mut kept = keep.iter().filter(|k| **k).count();
        for k in keep.iter_mut() {
            if kept <= limit {
                break;
            }
            if *k {
                *k = false;
                kept -= 1;
            }
        }
    }
    (0..snapshots.len()).filter(|&i| !keep[i]).collect()
}

/// Apply `retention` to the snapshots of `root`, deleting the ones it
/// drops. Returns the removed files.
pub fn prune(
    base: &Path,
    root: &Path,
    retention: &Retention,
    now: i64,
) -> io::Result<Vec<PathBuf>> {
    if retention.is_unlimited() {
        return Ok(Vec::new());
    }
    let snapshots = list(base, root)?;
    let mut removed = Vec::new();
    for i in select_prune(&snapshots, retention, now) {
        let path = &snapshots[i].path;
        std::fs::remove_file(path)?;
        removed.push(path.clone());
    }
    Ok(removed)
}

/// The size of the entry at `rel` (name components below the document's
/// root; empty = the root) in the snapshot `file`, or `None` when it is not
/// in that snapshot.
pub fn folder_size(file: &Path, rel: &[String]) -> io::Result<Option<FolderSize>> {
    let bytes = std::fs::read(file)?;
    folder_size_in(&bytes, rel)
}

/// [`folder_size`] on an in-memory document.
pub fn folder_size_in(json: &[u8], rel: &[String]) -> io::Result<Option<FolderSize>> {
    let wanted: Vec<String> = rel.iter().map(|c| fold_case(c)).collect();
    let mut de = serde_json::Deserializer::from_slice(json);
    let found = DocSeed { rel: &wanted }.deserialize(&mut de).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("invalid snapshot: {e}"))
    })?;
    Ok(found)
}

/// The growth series of `target`: its size in every snapshot that covers
/// it, oldest first. Unreadable snapshots appear with no size.
pub fn growth(base: &Path, target: &Path) -> io::Result<Vec<GrowthPoint>> {
    let snapshots = list_covering(base, target)?;
    Ok(snapshots
        .into_iter()
        .map(|snapshot| {
            let size = relative_components(&snapshot.root, target)
                .and_then(|rel| folder_size(&snapshot.path, &rel).ok().flatten());
            GrowthPoint { snapshot, size }
        })
        .collect())
}

/// Names compare case-insensitively on Windows, exactly elsewhere.
fn fold_case(name: &str) -> String {
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

// --- Streaming lookup -------------------------------------------------------

/// Top-level keys of both document flavours: this crate's export (`root` is
/// the tree) and the CLI's `fastdir.scan` (`root` is a path, `tree` the tree).
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum DocKey {
    Root,
    Tree,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum NodeKey {
    Name,
    Size,
    DiskSize,
    Files,
    #[serde(alias = "folders")]
    Dirs,
    Children,
    #[serde(other)]
    Other,
}

struct DocSeed<'a> {
    rel: &'a [String],
}

impl<'de> DeserializeSeed<'de> for DocSeed<'_> {
    type Value = Option<FolderSize>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for DocSeed<'_> {
    type Value = Option<FolderSize>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a FastDir scan document")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        while let Some(key) = map.next_key::<DocKey>()? {
            match key {
                DocKey::Root | DocKey::Tree => {
                    if let Some(size) = map.next_value_seed(RootSeed { rel: self.rel })? {
                        found = Some(size);
                    }
                }
                DocKey::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(found)
    }
}

/// `root`: a path string (CLI documents) or the root node (exports).
struct RootSeed<'a> {
    rel: &'a [String],
}

impl<'de> DeserializeSeed<'de> for RootSeed<'_> {
    type Value = Option<FolderSize>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for RootSeed<'_> {
    type Value = Option<FolderSize>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a root path or a tree node")
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        NodeSeed {
            name: None,
            rel: self.rel,
        }
        .visit_map(map)
    }
}

/// One node on the way to the target. `name` is the (case-folded) name the
/// node must have to be on the path; `None` for the document's root.
struct NodeSeed<'a> {
    name: Option<&'a str>,
    rel: &'a [String],
}

impl<'de> DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = Option<FolderSize>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for NodeSeed<'_> {
    type Value = Option<FolderSize>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a tree node")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        // Both writers put `name` before `children`, so by the time the
        // children arrive we know whether this node is on the path.
        let mut matched = self.name.is_none();
        let mut sizes = FolderSize::default();
        let mut found = None;
        while let Some(key) = map.next_key::<NodeKey>()? {
            match key {
                NodeKey::Name => match self.name {
                    Some(expected) => matched = map.next_value_seed(NameIs(expected))?,
                    None => {
                        map.next_value::<IgnoredAny>()?;
                    }
                },
                NodeKey::Size => sizes.size = map.next_value()?,
                NodeKey::DiskSize => sizes.disk_size = map.next_value()?,
                NodeKey::Files => sizes.files = map.next_value()?,
                NodeKey::Dirs => sizes.dirs = map.next_value()?,
                NodeKey::Children if matched && !self.rel.is_empty() => {
                    found = map.next_value_seed(ChildrenSeed { rel: self.rel })?;
                }
                NodeKey::Children | NodeKey::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(if matched && self.rel.is_empty() {
            Some(sizes)
        } else {
            found
        })
    }
}

/// Children of a node on the path: the one named `rel[0]` is searched for
/// `rel[1..]`, the others are skipped.
struct ChildrenSeed<'a> {
    rel: &'a [String],
}

impl<'de> DeserializeSeed<'de> for ChildrenSeed<'_> {
    type Value = Option<FolderSize>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for ChildrenSeed<'_> {
    type Value = Option<FolderSize>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of tree nodes")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let (first, rest) = self.rel.split_first().expect("non-empty path");
        let mut found = None;
        while let Some(result) = seq.next_element_seed(NodeSeed {
            name: Some(first),
            rel: rest,
        })? {
            if found.is_none() {
                found = result;
            }
        }
        Ok(found)
    }
}

/// Whether a JSON string equals `expected` (already case-folded), without
/// allocating for the common, unescaped case.
struct NameIs<'a>(&'a str);

impl<'de> DeserializeSeed<'de> for NameIs<'_> {
    type Value = bool;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for NameIs<'_> {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a name")
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<bool, E> {
        Ok(if cfg!(windows) {
            v.len() == self.0.len() && v.eq_ignore_ascii_case(self.0) || v.to_lowercase() == self.0
        } else {
            v == self.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arena, DirBuilder, Subtree};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fastdir-history-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// root / { Sub / { a.txt (a), b.bin 15 }, c.txt 30 }
    fn tree(root: &Path, a: u64) -> Tree {
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(1_700_000_000);
        sub.add_file("a.txt", a, a, 1_700_000_100);
        sub.add_file("b.bin", 15, 15, 1_700_000_200);
        let sub = sub.commit(&mut arena);
        let mut top = DirBuilder::new(1_700_000_300);
        top.add_dir("Sub", sub);
        top.add_file("c.txt", 30, 30, 1_700_000_400);
        let top = top.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", top), root.to_path_buf())
    }

    fn info(at: i64) -> SnapshotInfo {
        SnapshotInfo {
            path: PathBuf::from(file_name(at)),
            root: PathBuf::from("/r"),
            scanned_at: at,
        }
    }

    #[test]
    fn file_names_round_trip_and_sort_chronologically() {
        let at = 1_762_765_200; // 2025-11-10T09:00:00Z
        assert_eq!(file_name(at), "20251110T090000Z.json");
        assert_eq!(parse_file_name(&file_name(at)), Some(at));
        assert_eq!(parse_file_name("20251110T090000Z-2.json"), Some(at));
        assert_eq!(parse_file_name("20251110T090000Z.json.tmp"), None);
        assert_eq!(parse_file_name("root.txt"), None);
        assert_eq!(parse_file_name("20251110T090000Z-x.json"), None);
        assert!(file_name(at) < file_name(at + 1));
    }

    #[test]
    fn root_keys_are_stable_and_distinct() {
        let a = root_key(Path::new("/data/projects"));
        assert_eq!(a, root_key(Path::new("/data/projects/")));
        assert_ne!(a, root_key(Path::new("/data/project")));
        assert!(a.starts_with("data_projects-"), "{a}");
        assert!(!root_key(Path::new("/")).is_empty());
        let long = root_key(Path::new(
            "/home/someone/projects/clients/acme/archive/2026/q3/the-folder-name",
        ));
        assert!(long.contains("the-folder-name-"), "{long}");
        #[cfg(windows)]
        {
            assert_eq!(
                root_key(Path::new("C:\\Users")),
                root_key(Path::new("c:/users"))
            );
            assert_eq!(root_key(Path::new("D:")), root_key(Path::new("D:\\")));
        }
    }

    #[test]
    fn relative_components_require_an_ancestor() {
        let root = Path::new("/data");
        assert_eq!(relative_components(root, Path::new("/data")), Some(vec![]));
        assert_eq!(
            relative_components(root, Path::new("/data/a/b")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(relative_components(root, Path::new("/database")), None);
        assert_eq!(
            relative_components(Path::new("/data/a"), Path::new("/data")),
            None
        );
    }

    #[test]
    fn retention_keeps_the_newest_and_respects_both_limits() {
        let day = 86_400;
        let now = 100 * day;
        let snaps: Vec<_> = [10, 50, 90, 95, 99].iter().map(|d| info(d * day)).collect();
        // Nothing configured: nothing removed.
        assert!(select_prune(&snaps, &Retention::default(), now).is_empty());
        // Count only: the oldest go first.
        let r = Retention::from_limits(3, 0);
        assert_eq!(select_prune(&snaps, &r, now), vec![0, 1]);
        // Age only.
        let r = Retention::from_limits(0, 30);
        assert_eq!(select_prune(&snaps, &r, now), vec![0, 1]);
        // Both: age first, then count.
        let r = Retention::from_limits(2, 30);
        assert_eq!(select_prune(&snaps, &r, now), vec![0, 1, 2]);
        // Everything is too old, but the newest survives.
        let r = Retention::from_limits(0, 1);
        assert_eq!(select_prune(&snaps, &r, 1000 * day), vec![0, 1, 2, 3]);
        assert!(select_prune(&[], &r, now).is_empty());
    }

    #[test]
    fn save_list_prune_and_growth() {
        let base = temp_dir("save");
        let root = base.join("scanned");
        let opts = ExportOptions::default();
        let day = 86_400;
        let t0 = 1_762_765_200;
        let first = save(&tree(&root, 5), t0, &base, &opts).unwrap();
        save(&tree(&root, 500), t0 + day, &base, &opts).unwrap();
        // Same second: a suffix keeps both.
        let dup = save(&tree(&root, 1000), t0 + day, &base, &opts).unwrap();
        assert!(
            dup.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-2.json")
        );

        let listed = list(&base, &root).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].path, first);
        assert_eq!(listed[0].scanned_at, t0);
        assert_eq!(listed[0].root, root);
        assert!(list(&base, &base.join("other")).unwrap().is_empty());
        assert_eq!(roots(&base).unwrap().len(), 1);

        // A snapshot is an ordinary export.
        let imported = export::import_json_with_meta(std::fs::File::open(&first).unwrap()).unwrap();
        assert_eq!(imported.scanned_at, t0);
        assert_eq!(imported.tree.root().size, 50);

        // Growth of a sub-folder, matched by relative path.
        let series = growth(&base, &root.join("Sub")).unwrap();
        let sizes: Vec<_> = series.iter().map(|p| p.size.map(|s| s.size)).collect();
        assert_eq!(sizes, vec![Some(20), Some(515), Some(1015)]);
        assert_eq!(series[0].size.unwrap().files, 2);
        // A missing entry has no size; an unrelated path has no history.
        let series = growth(&base, &root.join("nope")).unwrap();
        assert!(series.iter().all(|p| p.size.is_none()));
        assert!(growth(&base, &base.join("elsewhere")).unwrap().is_empty());

        // Retention removes the oldest.
        let removed = prune(&base, &root, &Retention::from_limits(2, 0), t0 + 2 * day).unwrap();
        assert_eq!(removed, vec![first]);
        assert_eq!(list(&base, &root).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn folder_size_reads_cli_documents_too() {
        let doc = br#"{
            "schema": "fastdir.scan", "version": 1, "root": "/x",
            "tree": {"name": "/x", "type": "dir", "size": 10, "disk_size": 12,
              "files": 1, "folders": 1, "children": [
                {"name": "skip", "type": "dir", "size": 1, "disk_size": 1,
                 "children": [{"name": "deep", "type": "file", "size": 1, "disk_size": 1}]},
                {"name": "keep", "type": "dir", "size": 9, "disk_size": 11, "files": 1,
                 "folders": 0, "children": [{"name": "f", "type": "file", "size": 9, "disk_size": 11}]}
            ]}
        }"#;
        let root = folder_size_in(doc, &[]).unwrap().unwrap();
        assert_eq!((root.size, root.disk_size, root.dirs), (10, 12, 1));
        let keep = folder_size_in(doc, &["keep".to_string()]).unwrap().unwrap();
        assert_eq!((keep.size, keep.disk_size), (9, 11));
        let file = folder_size_in(doc, &["keep".to_string(), "f".to_string()]).unwrap();
        assert_eq!(file.map(|s| s.size), Some(9));
        assert_eq!(folder_size_in(doc, &["deep".to_string()]).unwrap(), None);
        assert!(folder_size_in(b"not json", &[]).is_err());
    }
}

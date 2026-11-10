//! Cleanup suggestions: well-known places where disk space can be reclaimed
//! with little risk.
//!
//! [`suggest`] looks at a scanned [`Tree`] (no file is opened) and lists:
//!
//! * temp folders and browser caches, whose *contents* can go;
//! * `node_modules` folders, and Cargo `target` folders (a `target` next to a
//!   `Cargo.toml`), which build tools recreate;
//! * the Windows Update download cache and `Windows.old`;
//! * entries of the Downloads folder that have not been touched for a while.
//!
//! Where those places are is described by [`KnownLocations`]: [`KnownLocations::detect`]
//! fills it in for the current user and platform, tests build it by hand.
//! Only locations that are part of the scan are reported, so sizes come
//! straight from the tree and deleting a suggestion updates the tree in place.
//!
//! Nothing here deletes anything: callers turn a [`Suggestion`] into the
//! delete / recycle operations they already have, after asking the user.

use crate::model::{Node, NodePath, Tree};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Downloads untouched for this many days count as stale by default.
pub const DEFAULT_STALE_DAYS: u32 = 90;

/// Suggestions smaller than this are not worth listing by default.
pub const DEFAULT_MIN_SIZE: u64 = 1024 * 1024;

const SECS_PER_DAY: i64 = 24 * 60 * 60;

/// What kind of reclaimable space a suggestion is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CleanupKind {
    /// A temp folder: its contents are disposable.
    Temp,
    /// A browser's on-disk cache.
    BrowserCache,
    /// A `node_modules` folder (`npm install` recreates it).
    NodeModules,
    /// A Cargo `target` folder (`cargo build` recreates it).
    BuildTarget,
    /// Downloaded Windows Update packages that were already installed.
    WindowsUpdate,
    /// The previous Windows installation kept after an upgrade.
    WindowsOld,
    /// Old files and folders in the Downloads folder.
    StaleDownloads,
}

impl CleanupKind {
    pub const ALL: [CleanupKind; 7] = [
        CleanupKind::Temp,
        CleanupKind::BrowserCache,
        CleanupKind::NodeModules,
        CleanupKind::BuildTarget,
        CleanupKind::WindowsUpdate,
        CleanupKind::WindowsOld,
        CleanupKind::StaleDownloads,
    ];

    /// Stable machine-readable name.
    pub fn id(self) -> &'static str {
        match self {
            CleanupKind::Temp => "temp",
            CleanupKind::BrowserCache => "browser_cache",
            CleanupKind::NodeModules => "node_modules",
            CleanupKind::BuildTarget => "build_target",
            CleanupKind::WindowsUpdate => "windows_update",
            CleanupKind::WindowsOld => "windows_old",
            CleanupKind::StaleDownloads => "stale_downloads",
        }
    }
}

/// One file or folder a suggestion would remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupTarget {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Bytes it accounts for in the scan.
    pub size: u64,
    /// Files and folders it stands for, itself included.
    pub items: u64,
}

/// A place to free space, with what removing it would delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: CleanupKind,
    /// The folder the suggestion is about. For [`CleanupKind::Temp`] and
    /// friends this folder itself is kept and only its contents go.
    pub location: PathBuf,
    /// What gets deleted: the contents of `location`, `location` itself, or
    /// the stale entries inside it.
    pub targets: Vec<CleanupTarget>,
    /// Total bytes of `targets`.
    pub size: u64,
    /// Total items of `targets`.
    pub items: u64,
}

impl Suggestion {
    /// Keep only the targets for which `keep` returns true, letting it update
    /// a target's size and item count, and recompute the totals. Used after
    /// files were deleted or moved, so the suggestion keeps describing what
    /// is still there.
    pub fn retain_targets(&mut self, mut keep: impl FnMut(&mut CleanupTarget) -> bool) {
        self.targets.retain_mut(|target| keep(target));
        self.size = self.targets.iter().map(|t| t.size).sum();
        self.items = self.targets.iter().map(|t| t.items).sum();
    }
}

/// The well-known folders to look for. Every list may be empty.
#[derive(Clone, Debug, Default)]
pub struct KnownLocations {
    /// Folders whose contents are disposable.
    pub temp: Vec<PathBuf>,
    /// Browser cache folders (contents are disposable).
    pub browser_caches: Vec<PathBuf>,
    /// Windows Update download cache folders (contents are disposable).
    pub windows_update: Vec<PathBuf>,
    /// `Windows.old` folders (removed as a whole).
    pub windows_old: Vec<PathBuf>,
    /// Downloads folders (only stale entries are suggested).
    pub downloads: Vec<PathBuf>,
}

impl KnownLocations {
    /// The locations for the current user and platform. Looks at the
    /// environment and lists browser profile folders; it does not scan.
    pub fn detect() -> Self {
        let mut known = Self::default();
        let env_path = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        known.temp.push(std::env::temp_dir());
        let home = env_path("USERPROFILE").or_else(|| env_path("HOME"));
        if let Some(home) = &home {
            known.downloads.push(home.join("Downloads"));
        }
        if cfg!(windows) {
            let local = env_path("LOCALAPPDATA");
            let windir = env_path("WINDIR").or_else(|| env_path("SystemRoot"));
            if let Some(local) = &local {
                known.temp.push(local.join("Temp"));
                known.browser_caches.extend(chromium_caches(&[
                    local.join(r"Google\Chrome\User Data"),
                    local.join(r"Microsoft\Edge\User Data"),
                    local.join(r"BraveSoftware\Brave-Browser\User Data"),
                    local.join(r"Vivaldi\User Data"),
                ]));
                known
                    .browser_caches
                    .extend(firefox_caches(&local.join(r"Mozilla\Firefox\Profiles")));
            }
            if let Some(windir) = &windir {
                known.temp.push(windir.join("Temp"));
                known
                    .windows_update
                    .push(windir.join(r"SoftwareDistribution\Download"));
            }
            let system_drive = env_path("SystemDrive")
                .map(|d| {
                    let mut root = d.into_os_string();
                    root.push("\\");
                    PathBuf::from(root)
                })
                .or_else(|| {
                    windir
                        .as_ref()
                        .and_then(|w| w.parent().map(Path::to_path_buf))
                });
            if let Some(drive) = system_drive {
                known.windows_old.push(drive.join("Windows.old"));
            }
        } else if let Some(home) = &home {
            let cache = env_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
            for dir in [
                "google-chrome",
                "chromium",
                "BraveSoftware",
                "microsoft-edge",
                "mozilla",
            ] {
                known.browser_caches.push(cache.join(dir));
            }
            let mac = home.join("Library/Caches");
            for dir in ["Google/Chrome", "Firefox", "com.apple.Safari"] {
                known.browser_caches.push(mac.join(dir));
            }
        }
        dedupe(&mut known.temp);
        dedupe(&mut known.browser_caches);
        known
    }
}

fn dedupe(paths: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.to_string_lossy().to_lowercase()));
}

/// Cache folders of every profile of Chromium-based browsers.
fn chromium_caches(user_data: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in user_data {
        let Ok(entries) = std::fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "Default" && !name.starts_with("Profile ") {
                continue;
            }
            for cache in ["Cache", "Code Cache", "GPUCache"] {
                out.push(entry.path().join(cache));
            }
        }
    }
    out
}

/// `cache2` folders of Firefox profiles.
fn firefox_caches(profiles: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(profiles) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("cache2"))
        .collect()
}

/// How [`suggest`] decides what is worth listing.
#[derive(Clone, Debug)]
pub struct CleanupOptions {
    /// Downloads older than this many days are stale.
    pub stale_days: u32,
    /// The current time, seconds since the Unix epoch.
    pub now: i64,
    /// Skip suggestions that would free less than this.
    pub min_size: u64,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            stale_days: DEFAULT_STALE_DAYS,
            now: chrono::Utc::now().timestamp(),
            min_size: DEFAULT_MIN_SIZE,
        }
    }
}

/// Reclaimable places inside `tree`, biggest first.
pub fn suggest(tree: &Tree, known: &KnownLocations, options: &CleanupOptions) -> Vec<Suggestion> {
    let mut out = Vec::new();
    // Known locations are suggested as such; the walk below does not look
    // inside them again, so nothing is listed twice.
    let mut claimed: HashSet<NodePath> = HashSet::new();
    let mut claim = |dir: &Path| -> Option<NodePath> {
        let path = tree.find(dir)?;
        let node = tree.get(&path)?;
        // The scan root itself is never a cleanup candidate.
        (node.is_dir() && !path.is_empty()).then(|| {
            claimed.insert(path.clone());
            path
        })
    };

    let contents_of: [(&[PathBuf], CleanupKind); 3] = [
        (&known.temp, CleanupKind::Temp),
        (&known.browser_caches, CleanupKind::BrowserCache),
        (&known.windows_update, CleanupKind::WindowsUpdate),
    ];
    for (dirs, kind) in contents_of {
        for dir in dirs {
            if let Some(path) = claim(dir) {
                let targets = children_where(tree, &path, |_| true);
                out.push(suggestion(kind, dir.clone(), targets));
            }
        }
    }
    for dir in &known.windows_old {
        if let Some(path) = claim(dir)
            && let Some(target) = target_of(tree, &path)
        {
            out.push(suggestion(
                CleanupKind::WindowsOld,
                dir.clone(),
                vec![target],
            ));
        }
    }
    let cutoff = options.now - i64::from(options.stale_days) * SECS_PER_DAY;
    for dir in &known.downloads {
        if let Some(path) = claim(dir) {
            let targets = children_where(tree, &path, |node| {
                node.modified() > 0 && node.modified() < cutoff
            });
            out.push(suggestion(
                CleanupKind::StaleDownloads,
                dir.clone(),
                targets,
            ));
        }
    }

    find_project_folders(tree, &claimed, &mut out);

    out.retain(|s| !s.targets.is_empty() && s.size > 0 && s.size >= options.min_size);
    out.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.location.cmp(&b.location))
    });
    out
}

fn suggestion(kind: CleanupKind, location: PathBuf, targets: Vec<CleanupTarget>) -> Suggestion {
    let size = targets.iter().map(|t| t.size).sum();
    let items = targets.iter().map(|t| t.items).sum();
    Suggestion {
        kind,
        location,
        targets,
        size,
        items,
    }
}

fn target_of(tree: &Tree, path: &[u32]) -> Option<CleanupTarget> {
    let node = tree.get(path)?;
    if node.is_special() {
        return None;
    }
    Some(CleanupTarget {
        path: tree.fs_path(path)?,
        is_dir: node.is_dir(),
        size: node.size,
        items: if node.is_dir() { node.items() + 1 } else { 1 },
    })
}

/// Targets for the children of the directory at `dir` that pass `keep`.
fn children_where(tree: &Tree, dir: &[u32], keep: impl Fn(&Node) -> bool) -> Vec<CleanupTarget> {
    let Some(node) = tree.get(dir) else {
        return Vec::new();
    };
    let mut path = dir.to_vec();
    let mut out = Vec::new();
    for (i, child) in tree.children(node).iter().enumerate() {
        if child.is_special() || !keep(child) {
            continue;
        }
        path.push(i as u32);
        out.extend(target_of(tree, &path));
        path.pop();
    }
    out
}

/// `node_modules` folders and Cargo `target` folders anywhere in the tree,
/// except inside the already `claimed` locations. A matching folder is not
/// searched further: nested ones are part of it.
fn find_project_folders(tree: &Tree, claimed: &HashSet<NodePath>, out: &mut Vec<Suggestion>) {
    let mut stack: Vec<(&Node, NodePath)> = vec![(tree.root(), Vec::new())];
    while let Some((dir, path)) = stack.pop() {
        let children = tree.children(dir);
        for (i, child) in children.iter().enumerate() {
            if !child.is_dir() {
                continue;
            }
            let mut child_path = path.clone();
            child_path.push(i as u32);
            if claimed.contains(&child_path) {
                continue;
            }
            let name = tree.name(child);
            let kind = if name.eq_ignore_ascii_case("node_modules") {
                Some(CleanupKind::NodeModules)
            } else if name == "target" && has_file(tree, children, "Cargo.toml") {
                Some(CleanupKind::BuildTarget)
            } else {
                None
            };
            match kind {
                Some(kind) => {
                    if let Some(target) = target_of(tree, &child_path) {
                        let location = target.path.clone();
                        out.push(suggestion(kind, location, vec![target]));
                    }
                }
                None if child.has_children() => stack.push((child, child_path)),
                None => {}
            }
        }
    }
}

fn has_file(tree: &Tree, siblings: &[Node], name: &str) -> bool {
    siblings
        .iter()
        .any(|n| !n.is_dir() && tree.name(n).eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arena, DirBuilder, Subtree};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = SECS_PER_DAY;

    fn dir(arena: &mut Arena, build: impl FnOnce(&mut DirBuilder)) -> Node {
        let mut b = DirBuilder::new(NOW);
        build(&mut b);
        b.commit(arena)
    }

    /// /root
    ///   Temp/            { a.tmp 2M, cache/ { b.tmp 3M } }
    ///   Downloads/       { old.iso 10M (200 days), new.zip 4M (1 day) }
    ///   proj/            { Cargo.toml, target/ { debug.bin 20M }, web/node_modules/ { x.js 6M } }
    ///   lone/target/     { y.bin 7M }   (no Cargo.toml: not a build folder)
    fn sample() -> Tree {
        let mb = 1024 * 1024;
        let mut arena = Arena::default();
        let temp_cache = dir(&mut arena, |b| b.add_file("b.tmp", 3 * mb, 3 * mb, NOW));
        let temp = dir(&mut arena, |b| {
            b.add_file("a.tmp", 2 * mb, 2 * mb, NOW);
            b.add_dir("cache", temp_cache);
        });
        let downloads = dir(&mut arena, |b| {
            b.add_file("old.iso", 10 * mb, 10 * mb, NOW - 200 * DAY);
            b.add_file("new.zip", 4 * mb, 4 * mb, NOW - DAY);
        });
        let debug = dir(&mut arena, |b| {
            b.add_file("debug.bin", 20 * mb, 20 * mb, NOW)
        });
        let node_modules = dir(&mut arena, |b| b.add_file("x.js", 6 * mb, 6 * mb, NOW));
        let web = dir(&mut arena, |b| b.add_dir("node_modules", node_modules));
        let proj = dir(&mut arena, |b| {
            b.add_file("Cargo.toml", 100, 4096, NOW);
            b.add_dir("target", debug);
            b.add_dir("web", web);
        });
        let lone_target = dir(&mut arena, |b| b.add_file("y.bin", 7 * mb, 7 * mb, NOW));
        let lone = dir(&mut arena, |b| b.add_dir("target", lone_target));
        let root = dir(&mut arena, |b| {
            b.add_dir("Temp", temp);
            b.add_dir("Downloads", downloads);
            b.add_dir("proj", proj);
            b.add_dir("lone", lone);
        });
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn known() -> KnownLocations {
        KnownLocations {
            temp: vec![PathBuf::from("/root/Temp")],
            downloads: vec![PathBuf::from("/root/Downloads")],
            ..KnownLocations::default()
        }
    }

    fn options() -> CleanupOptions {
        CleanupOptions {
            stale_days: 90,
            now: NOW,
            min_size: 1,
        }
    }

    fn by_kind(list: &[Suggestion], kind: CleanupKind) -> Vec<&Suggestion> {
        list.iter().filter(|s| s.kind == kind).collect()
    }

    #[test]
    fn lists_every_kind_biggest_first() {
        let mb = 1024 * 1024;
        let list = suggest(&sample(), &known(), &options());
        let kinds: Vec<_> = list.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                CleanupKind::BuildTarget,
                CleanupKind::StaleDownloads,
                CleanupKind::NodeModules,
                CleanupKind::Temp,
            ]
        );
        let sizes: Vec<_> = list.iter().map(|s| s.size).collect();
        assert_eq!(sizes, [20 * mb, 10 * mb, 6 * mb, 5 * mb]);
    }

    #[test]
    fn temp_suggests_its_contents_not_the_folder() {
        let list = suggest(&sample(), &known(), &options());
        let temp = by_kind(&list, CleanupKind::Temp)[0];
        assert_eq!(temp.location, PathBuf::from("/root/Temp"));
        let paths: Vec<_> = temp.targets.iter().map(|t| t.path.clone()).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&PathBuf::from("/root/Temp/a.tmp")));
        assert!(paths.contains(&PathBuf::from("/root/Temp/cache")));
        assert!(!paths.contains(&PathBuf::from("/root/Temp")));
        // The cache folder counts itself and its file.
        assert_eq!(temp.items, 1 + 2);
    }

    #[test]
    fn downloads_only_stale_entries() {
        let list = suggest(&sample(), &known(), &options());
        let downloads = by_kind(&list, CleanupKind::StaleDownloads)[0];
        let paths: Vec<_> = downloads.targets.iter().map(|t| t.path.clone()).collect();
        assert_eq!(paths, [PathBuf::from("/root/Downloads/old.iso")]);

        let strict = CleanupOptions {
            stale_days: 365,
            ..options()
        };
        assert!(
            by_kind(
                &suggest(&sample(), &known(), &strict),
                CleanupKind::StaleDownloads
            )
            .is_empty()
        );
    }

    #[test]
    fn cargo_target_needs_a_cargo_toml_next_to_it() {
        let list = suggest(&sample(), &known(), &options());
        let targets = by_kind(&list, CleanupKind::BuildTarget);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].location, PathBuf::from("/root/proj/target"));
        assert!(targets[0].targets[0].is_dir);
        assert!(
            !list
                .iter()
                .any(|s| s.location == *"/root/lone/target")
        );
    }

    #[test]
    fn node_modules_found_at_any_depth() {
        let list = suggest(&sample(), &known(), &options());
        let found = by_kind(&list, CleanupKind::NodeModules);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].location,
            PathBuf::from("/root/proj/web/node_modules")
        );
    }

    #[test]
    fn known_locations_are_not_searched_again() {
        // A node_modules inside the temp folder is part of the temp cleanup.
        let mb = 1024 * 1024;
        let mut arena = Arena::default();
        let nm = dir(&mut arena, |b| b.add_file("z.js", 2 * mb, 2 * mb, NOW));
        let temp = dir(&mut arena, |b| b.add_dir("node_modules", nm));
        let root = dir(&mut arena, |b| b.add_dir("Temp", temp));
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));
        let list = suggest(&tree, &known(), &options());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, CleanupKind::Temp);
    }

    #[test]
    fn locations_outside_the_scan_are_skipped() {
        let known = KnownLocations {
            temp: vec![PathBuf::from("/elsewhere/Temp")],
            ..KnownLocations::default()
        };
        let list = suggest(&sample(), &known, &options());
        assert!(by_kind(&list, CleanupKind::Temp).is_empty());
    }

    #[test]
    fn small_suggestions_are_dropped() {
        let mb = 1024 * 1024;
        let opts = CleanupOptions {
            min_size: 8 * mb,
            ..options()
        };
        let list = suggest(&sample(), &known(), &opts);
        assert!(list.iter().all(|s| s.size >= 8 * mb));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn scan_root_is_never_a_candidate() {
        let known = KnownLocations {
            temp: vec![PathBuf::from("/root")],
            ..KnownLocations::default()
        };
        let list = suggest(&sample(), &known, &options());
        assert!(by_kind(&list, CleanupKind::Temp).is_empty());
    }

    #[test]
    fn windows_old_is_removed_as_a_whole() {
        let mb = 1024 * 1024;
        let mut arena = Arena::default();
        let old = dir(&mut arena, |b| b.add_file("big.dat", 9 * mb, 9 * mb, NOW));
        let root = dir(&mut arena, |b| b.add_dir("Windows.old", old));
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));
        let known = KnownLocations {
            windows_old: vec![PathBuf::from("/root/Windows.old")],
            ..KnownLocations::default()
        };
        let list = suggest(&tree, &known, &options());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, CleanupKind::WindowsOld);
        assert_eq!(list[0].targets.len(), 1);
        assert_eq!(list[0].targets[0].path, PathBuf::from("/root/Windows.old"));
    }

    #[test]
    fn retain_targets_recomputes_totals() {
        let mut list = suggest(&sample(), &known(), &options());
        let temp = list
            .iter_mut()
            .find(|s| s.kind == CleanupKind::Temp)
            .unwrap();
        let before = temp.size;
        temp.retain_targets(|t| t.path.ends_with("a.tmp"));
        assert_eq!(temp.targets.len(), 1);
        assert_eq!(temp.size, 2 * 1024 * 1024);
        assert_eq!(temp.items, 1);
        assert!(temp.size < before);
        temp.retain_targets(|_| false);
        assert_eq!((temp.size, temp.items), (0, 0));
    }

    #[test]
    fn detect_does_not_panic_and_finds_a_temp_folder() {
        let known = KnownLocations::detect();
        assert!(!known.temp.is_empty());
    }
}

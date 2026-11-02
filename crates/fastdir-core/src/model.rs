//! The in-memory directory tree produced by a scan, plus the bookkeeping that
//! keeps it consistent when the user deletes, renames or moves items.

use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// No extension (directories, or files without a dot).
pub const NO_EXT: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    /// Pseudo-item for a drive scan: bytes Windows reports as free.
    FreeSpace,
    /// Pseudo-item for a drive scan: used bytes minus scanned bytes.
    Unknown,
}

/// Display names for the drive pseudo-items (WinDirStat style).
pub const FREE_SPACE_NAME: &str = "<Free space>";
pub const UNKNOWN_NAME: &str = "<Unknown>";

/// One file or directory. Children are always kept sorted by size, largest
/// first, which is what both the treemap and the default list order need.
#[derive(Debug)]
pub struct Node {
    pub name: Box<str>,
    /// Logical size in bytes (sum of counted descendants for directories).
    ///
    /// Hard-link duplicates are counted once: later links have `size == 0`
    /// and [`Node::hard_link`] set.
    pub size: u64,
    /// Allocated size in bytes, i.e. what the file actually occupies on disk
    /// (sum of counted descendants for directories). This accounts for
    /// compression, sparse files and cluster slack.
    pub disk_size: u64,
    /// Last write time, seconds since the Unix epoch (max of descendants for dirs).
    pub modified: i64,
    /// Number of files below (1 for a file).
    pub files: u32,
    /// Number of directories below (not counting itself).
    pub dirs: u32,
    /// Rank of this file's extension in [`Tree::exts`] (by total bytes).
    pub ext: u16,
    pub kind: NodeKind,
    /// UI state: whether this row is expanded in the tree view.
    pub expanded: bool,
    /// The directory could not be read (access denied, etc.).
    pub error: bool,
    /// This file is another hard link to a file already counted elsewhere in
    /// the scan. Its bytes are deliberately left out of aggregates.
    pub hard_link: bool,
    pub children: Vec<Node>,
}

impl Node {
    /// A file whose on-disk size is unknown; assume it matches the logical size.
    pub fn file(name: Box<str>, size: u64, modified: i64) -> Self {
        Self::file_on_disk(name, size, size, modified)
    }

    /// A file with a known logical and allocated (on-disk) size.
    pub fn file_on_disk(name: Box<str>, size: u64, disk_size: u64, modified: i64) -> Self {
        Self {
            name,
            size,
            disk_size,
            modified,
            files: 1,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::File,
            expanded: false,
            error: false,
            hard_link: false,
            children: Vec::new(),
        }
    }

    /// A later hard link to a file that was already counted. It still counts
    /// as a file, but adds no bytes to any total.
    pub fn hard_link(name: Box<str>, modified: i64) -> Self {
        Self {
            name,
            size: 0,
            disk_size: 0,
            modified,
            files: 1,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::File,
            expanded: false,
            error: false,
            hard_link: true,
            children: Vec::new(),
        }
    }

    pub fn dir(name: Box<str>, modified: i64) -> Self {
        Self {
            name,
            size: 0,
            disk_size: 0,
            modified,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::Dir,
            expanded: false,
            error: false,
            hard_link: false,
            children: Vec::new(),
        }
    }

    /// Pseudo-item for free bytes on a scanned drive. Counts as neither a
    /// file nor a folder so aggregates stay correct.
    pub fn free_space(size: u64) -> Self {
        Self {
            name: FREE_SPACE_NAME.into(),
            size,
            disk_size: size,
            modified: 0,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::FreeSpace,
            expanded: false,
            error: false,
            hard_link: false,
            children: Vec::new(),
        }
    }

    /// Pseudo-item for used-but-unscanned bytes on a scanned drive.
    pub fn unknown(size: u64) -> Self {
        Self {
            name: UNKNOWN_NAME.into(),
            size,
            disk_size: size,
            modified: 0,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::Unknown,
            expanded: false,
            error: false,
            hard_link: false,
            children: Vec::new(),
        }
    }

    #[inline]
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }

    /// Either drive pseudo-item (`<Free space>` or `<Unknown>`).
    #[inline]
    pub fn is_special(&self) -> bool {
        matches!(self.kind, NodeKind::FreeSpace | NodeKind::Unknown)
    }

    /// Items below this node (files + directories).
    #[inline]
    pub fn items(&self) -> u64 {
        self.files as u64 + self.dirs as u64
    }

    /// Recompute this directory's aggregates from its (already final) children
    /// and sort them by size.
    pub fn finalize(&mut self) {
        if !self.is_dir() {
            return;
        }
        self.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        let mut size = 0u64;
        let mut disk_size = 0u64;
        let mut files = 0u32;
        let mut dirs = 0u32;
        let mut modified = self.modified;
        for c in &self.children {
            size += c.size;
            disk_size += c.disk_size;
            files = files.saturating_add(c.files);
            dirs = dirs.saturating_add(c.dirs + c.is_dir() as u32);
            modified = modified.max(c.modified);
        }
        self.size = size;
        self.disk_size = disk_size;
        self.files = files;
        self.dirs = dirs;
        self.modified = modified;
    }

    /// The largest file under this node, used to colour treemap cells that
    /// are too small to subdivide.
    pub fn dominant_ext(&self) -> u16 {
        let mut n = self;
        loop {
            if !n.is_dir() {
                return n.ext;
            }
            match n.children.first() {
                Some(c) => n = c,
                None => return NO_EXT,
            }
        }
    }
}

/// Index path from the root to a node: `[]` is the root itself.
pub type NodePath = Vec<u32>;

#[derive(Clone, Debug)]
pub struct ExtStat {
    /// Lower-case extension including the dot, e.g. `.mp4`.
    pub name: Box<str>,
    pub bytes: u64,
    pub files: u64,
}

/// A finished scan.
pub struct Tree {
    pub root: Node,
    pub root_path: PathBuf,
    /// Extensions sorted by total bytes, largest first. A file's `ext` indexes this.
    pub exts: Vec<ExtStat>,
    ext_index: HashMap<Box<str>, u16>,
    /// Bumped on every mutation so caches (treemap image) know to refresh.
    pub generation: u64,
}

pub fn extension_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    let ext = &name[dot..];
    if ext.len() > 16 || ext.contains(' ') {
        return None;
    }
    Some(ext.to_lowercase())
}

impl Tree {
    pub fn new(root: Node, root_path: PathBuf) -> Self {
        let mut tree = Self {
            root,
            root_path,
            exts: Vec::new(),
            ext_index: HashMap::new(),
            generation: 1,
        };
        tree.build_extensions();
        tree
    }

    /// Collect extension statistics in parallel, rank them by bytes and stamp
    /// every file with its rank.
    fn build_extensions(&mut self) {
        fn collect(node: &Node) -> HashMap<String, (u64, u64)> {
            if !node.is_dir() {
                let mut m = HashMap::new();
                if let Some(e) = extension_of(&node.name) {
                    m.insert(e, (node.size, 1));
                }
                return m;
            }
            node.children
                .par_iter()
                .with_min_len(64)
                .map(collect)
                .reduce(HashMap::new, |mut a, b| {
                    if a.len() < b.len() {
                        return merge(b, a);
                    }
                    merge_into(&mut a, b);
                    a
                })
        }
        fn merge(
            mut a: HashMap<String, (u64, u64)>,
            b: HashMap<String, (u64, u64)>,
        ) -> HashMap<String, (u64, u64)> {
            merge_into(&mut a, b);
            a
        }
        fn merge_into(a: &mut HashMap<String, (u64, u64)>, b: HashMap<String, (u64, u64)>) {
            for (k, (bytes, files)) in b {
                let e = a.entry(k).or_insert((0, 0));
                e.0 += bytes;
                e.1 += files;
            }
        }

        let stats = collect(&self.root);
        let mut exts: Vec<ExtStat> = stats
            .into_iter()
            .map(|(name, (bytes, files))| ExtStat {
                name: name.into_boxed_str(),
                bytes,
                files,
            })
            .collect();
        exts.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        exts.truncate((NO_EXT - 1) as usize);
        self.ext_index = exts
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i as u16))
            .collect();
        self.exts = exts;

        fn stamp(node: &mut Node, index: &HashMap<Box<str>, u16>) {
            if node.is_dir() {
                node.children
                    .par_iter_mut()
                    .with_min_len(64)
                    .for_each(|c| stamp(c, index));
            } else {
                node.ext = extension_of(&node.name)
                    .and_then(|e| index.get(e.as_str()).copied())
                    .unwrap_or(NO_EXT);
            }
        }
        let index = std::mem::take(&mut self.ext_index);
        stamp(&mut self.root, &index);
        self.ext_index = index;
    }

    pub fn get(&self, path: &[u32]) -> Option<&Node> {
        let mut n = &self.root;
        for &i in path {
            n = n.children.get(i as usize)?;
        }
        Some(n)
    }

    pub fn get_mut(&mut self, path: &[u32]) -> Option<&mut Node> {
        let mut n = &mut self.root;
        for &i in path {
            n = n.children.get_mut(i as usize)?;
        }
        Some(n)
    }

    /// The file-system path of a node.
    pub fn fs_path(&self, path: &[u32]) -> Option<PathBuf> {
        let mut p = self.root_path.clone();
        let mut n = &self.root;
        for &i in path {
            n = n.children.get(i as usize)?;
            p.push(&*n.name);
        }
        Some(p)
    }

    /// Find the node for a file-system path inside the scanned root.
    pub fn find(&self, target: &Path) -> Option<NodePath> {
        let rel = target.strip_prefix(&self.root_path).ok()?;
        let mut out = Vec::new();
        let mut n = &self.root;
        for comp in rel.components() {
            let name = comp.as_os_str().to_string_lossy();
            let (i, c) = n
                .children
                .iter()
                .enumerate()
                .find(|(_, c)| c.name.eq_ignore_ascii_case(&name))?;
            out.push(i as u32);
            n = c;
        }
        Some(out)
    }

    fn ext_id(&mut self, name: &str) -> u16 {
        let Some(e) = extension_of(name) else {
            return NO_EXT;
        };
        if let Some(&id) = self.ext_index.get(e.as_str()) {
            return id;
        }
        if self.exts.len() >= (NO_EXT - 1) as usize {
            return NO_EXT;
        }
        let id = self.exts.len() as u16;
        let name: Box<str> = e.into_boxed_str();
        self.exts.push(ExtStat {
            name: name.clone(),
            bytes: 0,
            files: 0,
        });
        self.ext_index.insert(name, id);
        id
    }

    fn account_exts(&mut self, node: &Node, add: bool) {
        fn walk(node: &Node, exts: &mut [ExtStat], add: bool) {
            if node.is_dir() {
                for c in &node.children {
                    walk(c, exts, add);
                }
            } else if let Some(e) = exts.get_mut(node.ext as usize) {
                if add {
                    e.bytes += node.size;
                    e.files += 1;
                } else {
                    e.bytes = e.bytes.saturating_sub(node.size);
                    e.files = e.files.saturating_sub(1);
                }
            }
        }
        walk(node, &mut self.exts, add);
    }

    /// Re-aggregate and re-sort every directory on `path` from the deepest up.
    /// Returns the new index path of the node that was at `path` (its position
    /// among siblings can change when sizes change).
    fn refresh_ancestors(&mut self, path: &[u32]) -> NodePath {
        let mut new_path: NodePath = path.to_vec();
        for depth in (0..=path.len()).rev() {
            // Track the child on the path by name through the re-sort.
            let child_name = (depth < path.len()).then(|| {
                let parent = self.get(&new_path[..depth]).unwrap();
                parent.children[new_path[depth] as usize].name.clone()
            });
            let node = self.get_mut(&new_path[..depth]).unwrap();
            node.finalize();
            if let Some(name) = child_name {
                if let Some(ix) = node.children.iter().position(|c| c.name == name) {
                    new_path[depth] = ix as u32;
                }
            }
        }
        self.generation += 1;
        new_path
    }

    /// Detach a node from the tree, fixing up all aggregates. Pseudo-items
    /// (`<Free space>`, `<Unknown>`) cannot be removed. When a real node is
    /// removed from a drive scan, free space grows by the same amount so the
    /// volume total stays constant.
    pub fn remove(&mut self, path: &[u32]) -> Option<Node> {
        let (&last, parent_path) = path.split_last()?;
        // Peek first so we can reject pseudo-items without mutating.
        if self.get(path).is_some_and(|n| n.is_special()) {
            return None;
        }
        let removed_size = self.get(path).map(|n| n.size)?;
        let parent = self.get_mut(parent_path)?;
        if last as usize >= parent.children.len() {
            return None;
        }
        let node = parent.children.remove(last as usize);
        self.account_exts(&node, false);
        if !node.is_special() {
            self.grow_free(removed_size);
        }
        self.refresh_ancestors(parent_path);
        Some(node)
    }

    /// Attach a node under the directory at `dir_path`. Returns the new path.
    /// Inserting a real node into a drive scan shrinks free space so the
    /// volume total stays constant; inserting a pseudo-item leaves it alone.
    pub fn insert(&mut self, dir_path: &[u32], mut node: Node) -> Option<NodePath> {
        fn restamp(tree: &mut Tree, n: &mut Node) {
            if n.is_dir() {
                for c in &mut n.children {
                    restamp(tree, c);
                }
            } else if !n.is_special() {
                n.ext = tree.ext_id(&n.name);
            }
        }
        let inserted_size = node.size;
        let inserted_special = node.is_special();
        restamp(self, &mut node);
        self.account_exts(&node, true);
        let name = node.name.clone();
        let ix = {
            let dir = self.get_mut(dir_path)?;
            if !dir.is_dir() {
                return None;
            }
            dir.children.retain(|c| !c.name.eq_ignore_ascii_case(&name));
            dir.children.push(node);
            dir.children.len() as u32 - 1
        };
        if !inserted_special {
            self.shrink_free(inserted_size);
        }
        let mut p = dir_path.to_vec();
        p.push(ix);
        Some(self.refresh_ancestors(&p))
    }

    /// Rename a node in place. Returns its new path.
    pub fn rename(&mut self, path: &[u32], new_name: &str) -> Option<NodePath> {
        // Pseudo-items cannot be renamed (remove would reject them anyway).
        if self.get(path).is_some_and(|n| n.is_special()) {
            return None;
        }
        let node = self.remove(path)?;
        let mut node = node;
        node.name = new_name.into();
        self.insert(&path[..path.len() - 1], node)
    }

    /// Attach `<Free space>` and `<Unknown>` under the root for a drive scan
    /// (WinDirStat style). `total` and `free` are the volume figures from the
    /// OS at scan time; `total == 0` means "not a drive" and does nothing.
    /// Only positive entries are added. Re-attaching replaces old entries.
    pub fn attach_volume_info(&mut self, total: u64, free: u64) {
        if total == 0 {
            return;
        }
        self.root.children.retain(|c| !c.is_special());
        // `root.size` is still the scanned total here (no specials yet).
        let scanned = self.root.size;
        let used = total.saturating_sub(free);
        let unknown = used.saturating_sub(scanned);
        if free > 0 {
            self.root.children.push(Node::free_space(free));
        }
        if unknown > 0 {
            self.root.children.push(Node::unknown(unknown));
        }
        if free > 0 || unknown > 0 {
            self.root.finalize();
            self.generation += 1;
        }
    }

    fn free_index(&self) -> Option<usize> {
        self.root
            .children
            .iter()
            .position(|c| c.kind == NodeKind::FreeSpace)
    }

    fn grow_free(&mut self, delta: u64) {
        if delta == 0 {
            return;
        }
        if let Some(ix) = self.free_index() {
            let free = &mut self.root.children[ix];
            free.size = free.size.saturating_add(delta);
        }
    }

    fn shrink_free(&mut self, delta: u64) {
        if delta == 0 {
            return;
        }
        if let Some(ix) = self.free_index() {
            let free = &mut self.root.children[ix];
            free.size = free.size.saturating_sub(delta);
        }
    }

    /// Whether this is a drive scan with pseudo-items attached.
    pub fn is_drive_scan(&self) -> bool {
        self.root.children.iter().any(|c| c.is_special())
    }

    /// Bytes in `<Free space>`, if present.
    pub fn free_bytes(&self) -> Option<u64> {
        self.root
            .children
            .iter()
            .find(|c| c.kind == NodeKind::FreeSpace)
            .map(|c| c.size)
    }

    /// Bytes in `<Unknown>`, if present.
    pub fn unknown_bytes(&self) -> Option<u64> {
        self.root
            .children
            .iter()
            .find(|c| c.kind == NodeKind::Unknown)
            .map(|c| c.size)
    }

    /// Real scanned bytes (excluding pseudo-items).
    pub fn scanned_bytes(&self) -> u64 {
        let pseudo: u64 = self
            .root
            .children
            .iter()
            .filter(|c| c.is_special())
            .map(|c| c.size)
            .sum();
        self.root.size.saturating_sub(pseudo)
    }

    /// Real scanned bytes as allocated on disk (excluding pseudo-items).
    pub fn scanned_disk_bytes(&self) -> u64 {
        let pseudo: u64 = self
            .root
            .children
            .iter()
            .filter(|c| c.is_special())
            .map(|c| c.disk_size)
            .sum();
        self.root.disk_size.saturating_sub(pseudo)
    }

    /// Used bytes on the volume (scanned + unknown), or `root.size` when this
    /// is not a drive scan.
    pub fn used_bytes(&self) -> u64 {
        if self.is_drive_scan() {
            self.scanned_bytes() + self.unknown_bytes().unwrap_or(0)
        } else {
            self.root.size
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> Tree {
        let mut root = Node::dir("root".into(), 100);
        let mut sub = Node::dir("sub".into(), 50);
        sub.children.push(Node::file("a.txt".into(), 5, 5));
        sub.children.push(Node::file("b.bin".into(), 15, 6));
        sub.finalize();
        root.children.push(sub);
        root.children.push(Node::file("c.txt".into(), 30, 7));
        root.finalize();
        Tree::new(root, PathBuf::from("/root"))
    }

    #[test]
    fn extension_parsing() {
        assert_eq!(extension_of("movie.MP4").as_deref(), Some(".mp4"));
        assert_eq!(extension_of("archive.tar.gz").as_deref(), Some(".gz"));
        assert_eq!(extension_of("no-extension"), None);
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("trailing."), None);
        assert_eq!(extension_of("with space.txt"), Some(".txt".into()));
        assert_eq!(extension_of("name.a b"), None);
        assert_eq!(extension_of("name.verylongextension"), None);
    }

    #[test]
    fn finalize_aggregates_children_largest_first() {
        let tree = sample_tree();
        assert_eq!(tree.root.size, 50);
        assert_eq!(tree.root.files, 3);
        assert_eq!(tree.root.dirs, 1);
        assert_eq!(tree.root.children.len(), 2);
        assert_eq!(&*tree.root.children[0].name, "c.txt");
        assert_eq!(&*tree.root.children[1].name, "sub");
        assert_eq!(tree.root.children[1].size, 20);
    }

    #[test]
    fn extension_stats_are_ranked_and_stamped() {
        let tree = sample_tree();
        assert_eq!(tree.exts.len(), 2);
        assert_eq!(&*tree.exts[0].name, ".txt");
        assert_eq!(tree.exts[0].bytes, 35);
        assert_eq!(tree.exts[0].files, 2);
        assert_eq!(&*tree.exts[1].name, ".bin");
        assert_eq!(tree.exts[1].bytes, 15);

        let txt = tree.find(Path::new("/root/c.txt")).unwrap();
        assert_eq!(tree.get(&txt).unwrap().ext, 0);
        let bin = tree.find(Path::new("/root/sub/b.bin")).unwrap();
        assert_eq!(tree.get(&bin).unwrap().ext, 1);
    }

    #[test]
    fn get_and_fs_path_round_trip() {
        let tree = sample_tree();
        let path = tree.find(Path::new("/root/sub/a.txt")).unwrap();
        assert_eq!(tree.get(&path).unwrap().name.as_ref(), "a.txt");
        assert_eq!(
            tree.fs_path(&path).unwrap(),
            PathBuf::from("/root/sub/a.txt")
        );
        assert_eq!(tree.get(&[99]).map(|_| ()), None);
    }

    #[test]
    fn find_is_case_insensitive_on_windows_paths() {
        let tree = sample_tree();
        assert!(tree.find(Path::new("/root/SUB/A.TXT")).is_some());
    }

    #[test]
    fn remove_updates_aggregates_and_generation() {
        let mut tree = sample_tree();
        let before = tree.generation;
        let path = tree.find(Path::new("/root/sub")).unwrap();
        let removed = tree.remove(&path).unwrap();
        assert_eq!(removed.size, 20);
        assert_eq!(tree.root.size, 30);
        assert_eq!(tree.root.files, 1);
        assert_eq!(tree.root.dirs, 0);
        assert!(tree.generation > before);
        assert_eq!(tree.find(Path::new("/root/sub")), None);
        assert_eq!(
            tree.exts.iter().find(|e| &*e.name == ".bin").unwrap().bytes,
            0
        );
    }

    #[test]
    fn insert_adds_new_extension() {
        let mut tree = sample_tree();
        let node = Node::file("d.log".into(), 100, 8);
        let path = tree.insert(&[], node).unwrap();
        assert_eq!(tree.get(&path).unwrap().name.as_ref(), "d.log");
        assert_eq!(tree.root.size, 150);
        assert_eq!(tree.root.children[0].name.as_ref(), "d.log");
        let log = tree.exts.iter().find(|e| &*e.name == ".log").unwrap();
        assert_eq!(log.bytes, 100);
        assert_eq!(log.files, 1);
    }

    #[test]
    fn rename_moves_the_node_and_keeps_its_contents() {
        let mut tree = sample_tree();
        let path = tree.find(Path::new("/root/sub")).unwrap();
        let new_path = tree.rename(&path, "renamed").unwrap();
        assert_eq!(tree.get(&new_path).unwrap().name.as_ref(), "renamed");
        assert!(tree.find(Path::new("/root/renamed/a.txt")).is_some());
        assert_eq!(tree.find(Path::new("/root/sub")), None);
        assert_eq!(tree.root.size, 50);
    }

    #[test]
    fn dominant_ext_descends_to_the_largest_file() {
        let tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!(tree.get(&sub).unwrap().dominant_ext(), 1); // .bin
    }

    #[test]
    fn disk_size_aggregates_separately_from_logical_size() {
        let mut root = Node::dir("root".into(), 0);
        let mut sub = Node::dir("sub".into(), 0);
        sub.children
            .push(Node::file_on_disk("small.txt".into(), 10, 4096, 1));
        sub.children
            .push(Node::file_on_disk("sparse.bin".into(), 100_000, 8192, 2));
        sub.finalize();
        root.children.push(sub);
        root.finalize();
        let tree = Tree::new(root, PathBuf::from("/root"));
        assert_eq!(tree.root.size, 100_010);
        assert_eq!(tree.root.disk_size, 12_288);
        assert_eq!(tree.scanned_disk_bytes(), 12_288);
    }

    #[test]
    fn hard_link_duplicates_count_as_files_but_not_bytes() {
        let mut root = Node::dir("root".into(), 0);
        root.children
            .push(Node::file_on_disk("first.bin".into(), 100, 4096, 1));
        root.children.push(Node::hard_link("second.bin".into(), 1));
        root.finalize();
        assert_eq!(root.size, 100);
        assert_eq!(root.disk_size, 4096);
        assert_eq!(root.files, 2);
        assert_eq!(root.children[1].size, 0);
        assert!(root.children[1].hard_link);
    }

    #[test]
    fn attach_volume_info_adds_free_and_unknown() {
        let mut tree = sample_tree(); // scanned = 50
        assert!(!tree.is_drive_scan());
        tree.attach_volume_info(200, 100);
        // used = 100, unknown = 50, free = 100
        assert!(tree.is_drive_scan());
        assert_eq!(tree.free_bytes(), Some(100));
        assert_eq!(tree.unknown_bytes(), Some(50));
        assert_eq!(tree.scanned_bytes(), 50);
        assert_eq!(tree.used_bytes(), 100);
        assert_eq!(tree.root.size, 200);
        // Aggregates ignore pseudo-items.
        assert_eq!(tree.root.files, 3);
        assert_eq!(tree.root.dirs, 1);
        // Extension stats are untouched.
        assert_eq!(tree.exts.len(), 2);
        // Sorted largest-first: free (100) first.
        assert_eq!(&*tree.root.children[0].name, FREE_SPACE_NAME);
    }

    #[test]
    fn attach_volume_info_skips_zero_entries() {
        let mut tree = sample_tree(); // scanned = 50
        tree.attach_volume_info(50, 0);
        // used = 50, unknown = 0, free = 0 -> no pseudo-items.
        assert!(!tree.is_drive_scan());
        assert_eq!(tree.root.size, 50);

        let mut tree = sample_tree();
        tree.attach_volume_info(0, 0);
        assert!(!tree.is_drive_scan());
    }

    #[test]
    fn pseudo_items_cannot_be_removed_or_renamed() {
        let mut tree = sample_tree();
        tree.attach_volume_info(200, 100);
        let free_path: NodePath = vec![0]; // free (100) is largest
        assert!(tree.get(&free_path).unwrap().is_special());
        assert!(tree.remove(&free_path).is_none());
        assert!(tree.rename(&free_path, "x").is_none());
        assert!(tree.is_drive_scan());
    }

    #[test]
    fn remove_grows_free_to_keep_total_constant() {
        let mut tree = sample_tree(); // scanned 50
        tree.attach_volume_info(200, 100); // total 200
        let path = tree.find(Path::new("/root/c.txt")).unwrap(); // size 30
        tree.remove(&path).unwrap();
        assert_eq!(tree.scanned_bytes(), 20);
        assert_eq!(tree.free_bytes(), Some(130));
        assert_eq!(tree.root.size, 200);
    }

    #[test]
    fn insert_shrinks_free_to_keep_total_constant() {
        let mut tree = sample_tree();
        tree.attach_volume_info(200, 100);
        tree.insert(&[], Node::file("d.log".into(), 40, 8)).unwrap();
        assert_eq!(tree.scanned_bytes(), 90);
        assert_eq!(tree.free_bytes(), Some(60));
        assert_eq!(tree.root.size, 200);
    }
}

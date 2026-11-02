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
}

/// One file or directory. Children are always kept sorted by size, largest
/// first, which is what both the treemap and the default list order need.
#[derive(Debug)]
pub struct Node {
    pub name: Box<str>,
    /// Logical size in bytes (sum of descendants for directories).
    pub size: u64,
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
    pub children: Vec<Node>,
}

impl Node {
    pub fn file(name: Box<str>, size: u64, modified: i64) -> Self {
        Self {
            name,
            size,
            modified,
            files: 1,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::File,
            expanded: false,
            error: false,
            children: Vec::new(),
        }
    }

    pub fn dir(name: Box<str>, modified: i64) -> Self {
        Self {
            name,
            size: 0,
            modified,
            files: 0,
            dirs: 0,
            ext: NO_EXT,
            kind: NodeKind::Dir,
            expanded: false,
            error: false,
            children: Vec::new(),
        }
    }

    #[inline]
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
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
        let mut files = 0u32;
        let mut dirs = 0u32;
        let mut modified = self.modified;
        for c in &self.children {
            size += c.size;
            files = files.saturating_add(c.files);
            dirs = dirs.saturating_add(c.dirs + c.is_dir() as u32);
            modified = modified.max(c.modified);
        }
        self.size = size;
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
        fn merge(mut a: HashMap<String, (u64, u64)>, b: HashMap<String, (u64, u64)>) -> HashMap<String, (u64, u64)> {
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

    /// Detach a node from the tree, fixing up all aggregates.
    pub fn remove(&mut self, path: &[u32]) -> Option<Node> {
        let (&last, parent_path) = path.split_last()?;
        let parent = self.get_mut(parent_path)?;
        if last as usize >= parent.children.len() {
            return None;
        }
        let node = parent.children.remove(last as usize);
        self.account_exts(&node, false);
        self.refresh_ancestors(parent_path);
        Some(node)
    }

    /// Attach a node under the directory at `dir_path`. Returns the new path.
    pub fn insert(&mut self, dir_path: &[u32], mut node: Node) -> Option<NodePath> {
        fn restamp(tree: &mut Tree, n: &mut Node) {
            if n.is_dir() {
                for c in &mut n.children {
                    restamp(tree, c);
                }
            } else {
                n.ext = tree.ext_id(&n.name);
            }
        }
        restamp(self, &mut node);
        self.account_exts(&node, true);
        let name = node.name.clone();
        let dir = self.get_mut(dir_path)?;
        if !dir.is_dir() {
            return None;
        }
        dir.children.retain(|c| !c.name.eq_ignore_ascii_case(&name));
        dir.children.push(node);
        let ix = dir.children.len() as u32 - 1;
        let mut p = dir_path.to_vec();
        p.push(ix);
        Some(self.refresh_ancestors(&p))
    }

    /// Rename a node in place. Returns its new path.
    pub fn rename(&mut self, path: &[u32], new_name: &str) -> Option<NodePath> {
        let node = self.remove(path)?;
        let mut node = node;
        node.name = new_name.into();
        self.insert(&path[..path.len() - 1], node)
    }
}

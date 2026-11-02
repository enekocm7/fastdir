//! The in-memory directory tree produced by a scan, plus the bookkeeping that
//! keeps it consistent when the user deletes, renames or moves items.
//!
//! # Layout
//!
//! A scan of a whole drive holds millions of entries, so the tree is stored
//! compactly in an [`Arena`]:
//!
//! - A [`Node`] is 40 bytes of plain data with no heap allocation of its own.
//! - A directory's children are one boxed slice (a *block*), kept sorted by
//!   size, so they can be handed out as a plain slice. A directory refers to
//!   its block by a `u32` id.
//! - Every name lives in a shared, append-only list of 1 MiB string chunks; a
//!   node stores only a `u32` offset and a 10-bit length into it.
//!
//! The scanner writes each directory's block straight into the arena when the
//! directory is done, so there is no second copy of the tree at any point.

use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

/// No extension (directories, or files without a dot).
pub const NO_EXT: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Dir = 0,
    File = 1,
    /// Pseudo-item for a drive scan: bytes Windows reports as free.
    FreeSpace = 2,
    /// Pseudo-item for a drive scan: used bytes minus scanned bytes.
    Unknown = 3,
}

/// Display names for the drive pseudo-items (WinDirStat style).
pub const FREE_SPACE_NAME: &str = "<Free space>";
pub const UNKNOWN_NAME: &str = "<Unknown>";

/// Longest name a node can hold, in UTF-8 bytes. File-system names are at
/// most 255 UTF-16 units (765 UTF-8 bytes); only an unusually long scan root
/// path could be cut short.
pub const MAX_NAME_LEN: usize = (1 << NAME_LEN_BITS) - 1;
const NAME_LEN_BITS: u16 = 10;
const NAME_LEN_MASK: u16 = (1 << NAME_LEN_BITS) - 1;
const KIND_SHIFT: u16 = NAME_LEN_BITS;
const EXPANDED: u16 = 1 << 12;
const ERROR: u16 = 1 << 13;
const HARD_LINK: u16 = 1 << 14;

/// `block` of a node without children.
const NO_BLOCK: u32 = u32::MAX;

/// Names are packed into chunks of this many bytes; a name never straddles
/// two chunks. A name offset is `chunk << CHUNK_BITS | offset_in_chunk`.
const CHUNK_BITS: u32 = 20;
const CHUNK_SIZE: usize = 1 << CHUNK_BITS;

/// One file or directory. Its name and children live in the owning
/// [`Tree`] (or [`Subtree`]): see [`Tree::name`] and [`Tree::children`].
/// Children are always kept sorted by size, largest first, which is what both
/// the treemap and the default list order need.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// Logical size in bytes (sum of counted descendants for directories).
    ///
    /// Hard-link duplicates are counted once: later links have `size == 0`
    /// and [`Node::hard_link`] set.
    pub size: u64,
    /// Allocated size in bytes, i.e. what the file actually occupies on disk
    /// (sum of counted descendants for directories). This accounts for
    /// compression, sparse files and cluster slack.
    pub disk_size: u64,
    /// Id of the block holding the children, or [`NO_BLOCK`].
    block: u32,
    /// Number of files below (1 for a file).
    pub files: u32,
    /// Number of directories below (not counting itself).
    pub dirs: u32,
    name_off: u32,
    /// Last write time in seconds since the Unix epoch, clamped to `u32`.
    modified: u32,
    /// Rank of this file's extension in [`Tree::exts`] (by total bytes).
    pub ext: u16,
    /// Name length, kind and flags (see the `*_SHIFT` / flag constants).
    bits: u16,
}

const _: () = assert!(std::mem::size_of::<Node>() == 40);

/// Seconds since the Unix epoch, clamped into the `u32` range a node stores
/// (1970 to 2106).
fn pack_time(unix: i64) -> u32 {
    unix.clamp(0, u32::MAX as i64) as u32
}

/// The longest prefix of `name` that fits in a node, cut at a character
/// boundary.
fn clip_name(name: &str) -> &str {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

impl Node {
    fn new(kind: NodeKind, size: u64, disk_size: u64, modified: i64) -> Self {
        Self {
            size,
            disk_size,
            block: NO_BLOCK,
            files: (kind == NodeKind::File) as u32,
            dirs: 0,
            name_off: 0,
            modified: pack_time(modified),
            ext: NO_EXT,
            bits: (kind as u16) << KIND_SHIFT,
        }
    }

    #[inline]
    pub fn kind(&self) -> NodeKind {
        match (self.bits >> KIND_SHIFT) & 3 {
            0 => NodeKind::Dir,
            1 => NodeKind::File,
            2 => NodeKind::FreeSpace,
            _ => NodeKind::Unknown,
        }
    }

    #[inline]
    pub fn is_dir(&self) -> bool {
        self.kind() == NodeKind::Dir
    }

    /// Either drive pseudo-item (`<Free space>` or `<Unknown>`).
    #[inline]
    pub fn is_special(&self) -> bool {
        matches!(self.kind(), NodeKind::FreeSpace | NodeKind::Unknown)
    }

    /// Whether this directory has any children.
    #[inline]
    pub fn has_children(&self) -> bool {
        self.block != NO_BLOCK
    }

    /// Items below this node (files + directories).
    #[inline]
    pub fn items(&self) -> u64 {
        self.files as u64 + self.dirs as u64
    }

    /// Last write time, seconds since the Unix epoch (max of descendants for
    /// dirs). `0` when unknown.
    #[inline]
    pub fn modified(&self) -> i64 {
        self.modified as i64
    }

    /// UI state: whether this row is expanded in the tree view.
    #[inline]
    pub fn expanded(&self) -> bool {
        self.bits & EXPANDED != 0
    }

    #[inline]
    pub fn set_expanded(&mut self, expanded: bool) {
        self.set_flag(EXPANDED, expanded);
    }

    /// The directory could not be read (access denied, etc.).
    #[inline]
    pub fn error(&self) -> bool {
        self.bits & ERROR != 0
    }

    /// This file is another hard link to a file already counted elsewhere in
    /// the scan. Its bytes are deliberately left out of aggregates.
    #[inline]
    pub fn hard_link(&self) -> bool {
        self.bits & HARD_LINK != 0
    }

    #[inline]
    fn set_flag(&mut self, flag: u16, on: bool) {
        if on {
            self.bits |= flag;
        } else {
            self.bits &= !flag;
        }
    }

    #[inline]
    fn name_len(&self) -> usize {
        (self.bits & NAME_LEN_MASK) as usize
    }

    #[inline]
    fn set_name(&mut self, off: u32, len: usize) {
        debug_assert!(len <= MAX_NAME_LEN);
        self.name_off = off;
        self.bits = (self.bits & !NAME_LEN_MASK) | len as u16;
    }

    /// Identifies a node among its siblings: every node owns its name slot.
    #[inline]
    fn name_key(&self) -> (u32, usize) {
        (self.name_off, self.name_len())
    }
}

/// Recompute a directory's aggregates from its (already final) children.
fn aggregate(dir: &mut Node, children: &[Node]) {
    let mut size = 0u64;
    let mut disk_size = 0u64;
    let mut files = 0u32;
    let mut dirs = 0u32;
    let mut modified = dir.modified;
    for c in children {
        size += c.size;
        disk_size += c.disk_size;
        files = files.saturating_add(c.files);
        dirs = dirs.saturating_add(c.dirs + c.is_dir() as u32);
        modified = modified.max(c.modified);
    }
    dir.size = size;
    dir.disk_size = disk_size;
    dir.files = files;
    dir.dirs = dirs;
    dir.modified = modified;
}

/// Sort children by size, largest first, then by name.
fn sort_by_size<'a>(children: &mut [Node], name: impl Fn(&Node) -> &'a str) {
    children.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| name(a).cmp(name(b))));
}

/// Storage for the blocks of children and the names of a tree.
#[derive(Debug, Default)]
pub struct Arena {
    blocks: Vec<Box<[Node]>>,
    /// Ids of emptied blocks, reused before growing `blocks`.
    free_blocks: Vec<u32>,
    chunks: Vec<String>,
}

#[inline]
fn name_in<'a>(chunks: &'a [String], node: &Node) -> &'a str {
    let chunk = &chunks[(node.name_off >> CHUNK_BITS) as usize];
    let start = (node.name_off as usize) & (CHUNK_SIZE - 1);
    &chunk[start..start + node.name_len()]
}

impl Arena {
    /// Store a name and point `node` at it.
    fn set_name(&mut self, node: &mut Node, name: &str) {
        let name = clip_name(name);
        let fits = self
            .chunks
            .last()
            .is_some_and(|c| c.len() + name.len() <= CHUNK_SIZE);
        if !fits {
            assert!(
                self.chunks.len() < 1 << (32 - CHUNK_BITS),
                "name storage exceeds 4 GiB"
            );
            self.chunks.push(String::with_capacity(CHUNK_SIZE));
        }
        let chunk_ix = self.chunks.len() - 1;
        let chunk = &mut self.chunks[chunk_ix];
        let off = ((chunk_ix as u32) << CHUNK_BITS) | chunk.len() as u32;
        chunk.push_str(name);
        node.set_name(off, name.len());
    }

    #[inline]
    fn name(&self, node: &Node) -> &str {
        name_in(&self.chunks, node)
    }

    #[inline]
    fn children(&self, node: &Node) -> &[Node] {
        if node.block == NO_BLOCK {
            return &[];
        }
        &self.blocks[node.block as usize]
    }

    /// Store a block of children and return its id ([`NO_BLOCK`] if empty).
    fn push_block(&mut self, nodes: Box<[Node]>) -> u32 {
        if nodes.is_empty() {
            return NO_BLOCK;
        }
        if let Some(id) = self.free_blocks.pop() {
            self.blocks[id as usize] = nodes;
            return id;
        }
        let id = u32::try_from(self.blocks.len())
            .ok()
            .filter(|&id| id != NO_BLOCK)
            .expect("too many directories");
        self.blocks.push(nodes);
        id
    }

    /// Replace the children of `dir` with `nodes`, reusing its block id.
    fn set_children(&mut self, dir: &mut Node, nodes: Vec<Node>) {
        if dir.block == NO_BLOCK {
            dir.block = self.push_block(nodes.into_boxed_slice());
        } else if nodes.is_empty() {
            self.blocks[dir.block as usize] = Box::default();
            self.free_blocks.push(dir.block);
            dir.block = NO_BLOCK;
        } else {
            self.blocks[dir.block as usize] = nodes.into_boxed_slice();
        }
    }

    /// Free every block below `node`, calling `f` for each node in its
    /// subtree (including itself).
    fn free_subtree(&mut self, node: &Node, mut f: impl FnMut(&Node)) {
        f(node);
        let mut stack = vec![node.block];
        while let Some(id) = stack.pop() {
            if id == NO_BLOCK {
                continue;
            }
            let block = std::mem::take(&mut self.blocks[id as usize]);
            self.free_blocks.push(id);
            for c in block.iter() {
                f(c);
                stack.push(c.block);
            }
        }
    }

    /// Copy `node` and its subtree from `src` into this arena, re-storing its
    /// names here. Returns the node as it is stored in this arena.
    fn import(&mut self, src: &mut Arena, node: &Node) -> Node {
        let mut n = *node;
        self.set_name(&mut n, src.name(node));
        if node.block != NO_BLOCK {
            let block = std::mem::take(&mut src.blocks[node.block as usize]);
            let children: Vec<Node> = block.iter().map(|c| self.import(src, c)).collect();
            n.block = self.push_block(children.into_boxed_slice());
        }
        n
    }
}

/// A tree of nodes with its storage: either a whole scan (inside [`Tree`])
/// or a detached piece, such as a freshly scanned folder or a single moved
/// file, ready to be spliced in with [`Tree::insert`].
#[derive(Debug)]
pub struct Subtree {
    root: Node,
    arena: Arena,
}

impl Subtree {
    /// A tree whose root is `root` (usually from [`DirBuilder::commit`] into
    /// `arena`), named `name`.
    pub fn new(mut arena: Arena, name: &str, mut root: Node) -> Self {
        arena.set_name(&mut root, name);
        Self { root, arena }
    }

    /// A file whose on-disk size is unknown; assume it matches the logical size.
    pub fn file(name: &str, size: u64, modified: i64) -> Self {
        Self::file_on_disk(name, size, size, modified)
    }

    /// A file with a known logical and allocated (on-disk) size.
    pub fn file_on_disk(name: &str, size: u64, disk_size: u64, modified: i64) -> Self {
        Self::single(name, Node::new(NodeKind::File, size, disk_size, modified))
    }

    fn single(name: &str, node: Node) -> Self {
        let mut arena = Arena::default();
        arena.chunks.push(String::with_capacity(name.len()));
        // A one-name chunk: `set_name` only starts a new chunk when full.
        let mut root = node;
        let name = clip_name(name);
        arena.chunks[0].push_str(name);
        root.set_name(0, name.len());
        Self { root, arena }
    }

    #[inline]
    pub fn root(&self) -> &Node {
        &self.root
    }

    #[inline]
    pub fn root_mut(&mut self) -> &mut Node {
        &mut self.root
    }

    /// The name of a node that belongs to this tree.
    #[inline]
    pub fn name(&self, node: &Node) -> &str {
        self.arena.name(node)
    }

    /// The children of a node that belongs to this tree, largest first.
    #[inline]
    pub fn children(&self, node: &Node) -> &[Node] {
        self.arena.children(node)
    }
}

/// Collects one directory's entries, then stores them in an [`Arena`] as a
/// block.
///
/// Entries are added in any order; [`DirBuilder::finish`] sorts them and
/// computes the directory's totals, and [`DirBuilder::commit`] moves them
/// into the arena. Sub-directories are committed first (bottom-up) and added
/// with [`DirBuilder::add_dir`].
#[derive(Debug)]
pub struct DirBuilder {
    node: Node,
    /// Names of `children`, which point into this (not into an arena) until
    /// they are committed.
    names: String,
    children: Vec<Node>,
    finished: bool,
}

impl DirBuilder {
    pub fn new(modified: i64) -> Self {
        Self {
            node: Node::new(NodeKind::Dir, 0, 0, modified),
            names: String::new(),
            children: Vec::new(),
            finished: false,
        }
    }

    fn push(&mut self, name: &str, mut node: Node) {
        let name = clip_name(name);
        node.set_name(self.names.len() as u32, name.len());
        self.names.push_str(name);
        self.children.push(node);
        self.finished = false;
    }

    /// A file with a known logical and allocated (on-disk) size.
    pub fn add_file(&mut self, name: &str, size: u64, disk_size: u64, modified: i64) {
        self.push(name, Node::new(NodeKind::File, size, disk_size, modified));
    }

    /// A later hard link to a file that was already counted. It still counts
    /// as a file, but adds no bytes to any total.
    pub fn add_hard_link(&mut self, name: &str, modified: i64) {
        let mut node = Node::new(NodeKind::File, 0, 0, modified);
        node.set_flag(HARD_LINK, true);
        self.push(name, node);
    }

    /// A directory that is listed but not descended into (junctions,
    /// symlinked directories).
    pub fn add_empty_dir(&mut self, name: &str, modified: i64) {
        self.push(name, Node::new(NodeKind::Dir, 0, 0, modified));
    }

    /// A sub-directory already committed to the same arena.
    pub fn add_dir(&mut self, name: &str, dir: Node) {
        self.push(name, dir);
    }

    /// The directory could not be read.
    pub fn set_error(&mut self) {
        self.node.set_flag(ERROR, true);
    }

    /// Sort the children and compute the directory's totals. Done by
    /// [`Self::commit`] if needed; calling it first keeps that work outside
    /// whatever lock guards the arena.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        let names = &self.names;
        sort_by_size(&mut self.children, |n| {
            &names[n.name_off as usize..n.name_off as usize + n.name_len()]
        });
        aggregate(&mut self.node, &self.children);
        self.finished = true;
    }

    /// Store the children in `arena` and return the directory's node (still
    /// unnamed: its parent names it).
    pub fn commit(mut self, arena: &mut Arena) -> Node {
        self.finish();
        let names = std::mem::take(&mut self.names);
        for c in &mut self.children {
            let start = c.name_off as usize;
            let name = &names[start..start + c.name_len()];
            arena.set_name(c, name);
        }
        self.node.block = arena.push_block(self.children.into_boxed_slice());
        self.node
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

/// One file in a flat "largest files" list.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: NodePath,
    pub name: Box<str>,
    pub size: u64,
    pub disk_size: u64,
    pub modified: i64,
    /// Rank in [`Tree::exts`], or [`NO_EXT`].
    pub ext: u16,
}

/// Where a node is stored.
#[derive(Clone, Copy)]
enum Slot {
    Root,
    /// Child `.1` of block `.0`.
    In(u32, usize),
}

/// A finished scan.
pub struct Tree {
    sub: Subtree,
    pub root_path: PathBuf,
    /// Extensions sorted by total bytes, largest first. A file's `ext` indexes this.
    pub exts: Vec<ExtStat>,
    ext_index: HashMap<Box<str>, u16>,
    /// Bumped on every mutation so caches (treemap image) know to refresh.
    pub generation: u64,
    /// Name bytes no longer used by any node.
    dead_name_bytes: usize,
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
    pub fn new(root: Subtree, root_path: PathBuf) -> Self {
        let mut tree = Self {
            sub: root,
            root_path,
            exts: Vec::new(),
            ext_index: HashMap::new(),
            generation: 1,
            dead_name_bytes: 0,
        };
        tree.build_extensions();
        tree
    }

    /// Collect extension statistics in parallel, rank them by bytes and stamp
    /// every file with its rank.
    fn build_extensions(&mut self) {
        type Stats = HashMap<String, (u64, u64)>;
        let Arena { blocks, chunks, .. } = &mut self.sub.arena;
        let chunks: &[String] = chunks;
        let stats: Stats = blocks
            .par_iter()
            .fold(HashMap::new, |mut m: Stats, block| {
                for n in block.iter().filter(|n| n.kind() == NodeKind::File) {
                    if let Some(e) = extension_of(name_in(chunks, n)) {
                        let s = m.entry(e).or_insert((0, 0));
                        s.0 += n.size;
                        s.1 += 1;
                    }
                }
                m
            })
            .reduce(HashMap::new, |a, b| {
                let (mut big, small) = if a.len() >= b.len() { (a, b) } else { (b, a) };
                for (k, (bytes, files)) in small {
                    let e = big.entry(k).or_insert((0, 0));
                    e.0 += bytes;
                    e.1 += files;
                }
                big
            });

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
        let index: HashMap<Box<str>, u16> = exts
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i as u16))
            .collect();

        blocks.par_iter_mut().for_each(|block| {
            for n in block.iter_mut().filter(|n| n.kind() == NodeKind::File) {
                n.ext = extension_of(name_in(chunks, n))
                    .and_then(|e| index.get(e.as_str()).copied())
                    .unwrap_or(NO_EXT);
            }
        });
        self.exts = exts;
        self.ext_index = index;
    }

    #[inline]
    pub fn root(&self) -> &Node {
        &self.sub.root
    }

    /// The name of a node in this tree.
    #[inline]
    pub fn name(&self, node: &Node) -> &str {
        self.sub.name(node)
    }

    /// The children of a node in this tree, largest first.
    #[inline]
    pub fn children(&self, node: &Node) -> &[Node] {
        self.sub.children(node)
    }

    /// The display name of a file's extension (including the dot), if any.
    #[inline]
    pub fn ext_name(&self, node: &Node) -> Option<&str> {
        self.exts.get(node.ext as usize).map(|e| &*e.name)
    }

    /// The `limit` largest files in the whole tree, largest first.
    ///
    /// Uses a bounded heap, so a drive scan with millions of files only keeps
    /// `limit` candidates in memory. Hard-link duplicates are left out: they
    /// add no bytes and would just repeat a file already in the list.
    pub fn top_files(&self, limit: usize) -> Vec<FileEntry> {
        if limit == 0 {
            return Vec::new();
        }
        // The smallest retained file is at the top of this min-heap.
        let mut heap: BinaryHeap<Reverse<(u64, Box<str>, NodePath)>> =
            BinaryHeap::with_capacity(limit);
        let mut stack: Vec<(NodePath, &Node)> = vec![(Vec::new(), self.root())];
        while let Some((path, dir)) = stack.pop() {
            for (i, child) in self.children(dir).iter().enumerate() {
                match child.kind() {
                    NodeKind::Dir => {
                        let mut child_path = path.clone();
                        child_path.push(i as u32);
                        stack.push((child_path, child));
                    }
                    NodeKind::File if !child.hard_link() => {
                        // Skip the (many) files that cannot beat the current
                        // cutoff before cloning their name and path.
                        let worth = heap.len() < limit
                            || heap
                                .peek()
                                .is_some_and(|Reverse((smallest, ..))| child.size >= *smallest);
                        if worth {
                            let mut child_path = path.clone();
                            child_path.push(i as u32);
                            let key = (child.size, self.name(child).into(), child_path);
                            if heap.len() < limit {
                                heap.push(Reverse(key));
                            } else if heap.peek().is_some_and(|Reverse(smallest)| key > *smallest) {
                                heap.pop();
                                heap.push(Reverse(key));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out: Vec<FileEntry> = heap
            .into_iter()
            .map(|Reverse((_, name, path))| {
                let node = self
                    .get(&path)
                    .expect("top-file path still points into the tree");
                FileEntry {
                    path,
                    name,
                    size: node.size,
                    disk_size: node.disk_size,
                    modified: node.modified(),
                    ext: node.ext,
                }
            })
            .collect();
        out.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// The extension of the largest file under `node`, used to colour
    /// treemap cells that are too small to subdivide.
    pub fn dominant_ext(&self, node: &Node) -> u16 {
        let mut n = node;
        loop {
            if !n.is_dir() {
                return n.ext;
            }
            match self.children(n).first() {
                Some(c) => n = c,
                None => return NO_EXT,
            }
        }
    }

    fn slot(&self, path: &[u32]) -> Option<Slot> {
        let mut slot = Slot::Root;
        let mut node = self.root();
        for &i in path {
            let block = node.block;
            node = self.children(node).get(i as usize)?;
            slot = Slot::In(block, i as usize);
        }
        Some(slot)
    }

    fn at(&self, slot: Slot) -> &Node {
        match slot {
            Slot::Root => &self.sub.root,
            Slot::In(b, i) => &self.sub.arena.blocks[b as usize][i],
        }
    }

    fn at_mut(&mut self, slot: Slot) -> &mut Node {
        match slot {
            Slot::Root => &mut self.sub.root,
            Slot::In(b, i) => &mut self.sub.arena.blocks[b as usize][i],
        }
    }

    pub fn get(&self, path: &[u32]) -> Option<&Node> {
        self.slot(path).map(|s| self.at(s))
    }

    /// Mutable access, e.g. to [`Node::set_expanded`]. Do not change sizes
    /// through this: aggregates would go stale.
    pub fn get_mut(&mut self, path: &[u32]) -> Option<&mut Node> {
        self.slot(path).map(|s| self.at_mut(s))
    }

    /// The file-system path of a node.
    pub fn fs_path(&self, path: &[u32]) -> Option<PathBuf> {
        let mut p = self.root_path.clone();
        let mut n = self.root();
        for &i in path {
            n = self.children(n).get(i as usize)?;
            p.push(self.name(n));
        }
        Some(p)
    }

    /// Find the node for a file-system path inside the scanned root.
    pub fn find(&self, target: &Path) -> Option<NodePath> {
        let rel = target.strip_prefix(&self.root_path).ok()?;
        let mut out = Vec::new();
        let mut n = self.root();
        for comp in rel.components() {
            let name = comp.as_os_str().to_string_lossy();
            let (i, c) = self
                .children(n)
                .iter()
                .enumerate()
                .find(|(_, c)| self.name(c).eq_ignore_ascii_case(&name))?;
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

    fn account_ext(exts: &mut [ExtStat], node: &Node, add: bool) {
        if node.kind() != NodeKind::File {
            return;
        }
        if let Some(e) = exts.get_mut(node.ext as usize) {
            if add {
                e.bytes += node.size;
                e.files += 1;
            } else {
                e.bytes = e.bytes.saturating_sub(node.size);
                e.files = e.files.saturating_sub(1);
            }
        }
    }

    /// Unlink child `k` of the directory in `dir`, freeing its subtree and
    /// taking it out of the extension stats. Aggregates are left for the
    /// caller to refresh. Returns the detached node.
    fn detach(&mut self, dir: Slot, k: usize) -> Node {
        let mut d = *self.at(dir);
        let mut children = self.children(&d).to_vec();
        let node = children.remove(k);
        let exts = &mut self.exts;
        let mut dead = 0usize;
        self.sub.arena.free_subtree(&node, |n| {
            Self::account_ext(exts, n, false);
            dead += n.name_len();
        });
        self.dead_name_bytes += dead;
        self.sub.arena.set_children(&mut d, children);
        *self.at_mut(dir) = d;
        node
    }

    /// Append `node` (already stored in this arena) as the last child of the
    /// directory in `dir`. Returns its position among its siblings.
    fn append(&mut self, dir: Slot, node: Node) -> u32 {
        let mut d = *self.at(dir);
        let mut children = self.children(&d).to_vec();
        children.push(node);
        let k = children.len() as u32 - 1;
        self.sub.arena.set_children(&mut d, children);
        *self.at_mut(dir) = d;
        k
    }

    /// Sort and re-aggregate the directory in `slot` from its children.
    fn finalize(&mut self, slot: Slot) {
        let mut dir = *self.at(slot);
        if !dir.is_dir() {
            return;
        }
        if dir.block == NO_BLOCK {
            aggregate(&mut dir, &[]);
        } else {
            let Arena { blocks, chunks, .. } = &mut self.sub.arena;
            let chunks: &[String] = chunks;
            let children = &mut blocks[dir.block as usize];
            sort_by_size(children, |n| name_in(chunks, n));
            aggregate(&mut dir, children);
        }
        *self.at_mut(slot) = dir;
    }

    /// Re-aggregate and re-sort every directory on `path` from the deepest up.
    /// Returns the new index path of the node that was at `path` (its position
    /// among siblings can change when sizes change).
    fn refresh_ancestors(&mut self, path: &[u32]) -> NodePath {
        let mut new_path: NodePath = path.to_vec();
        for depth in (0..=path.len()).rev() {
            let slot = self
                .slot(&new_path[..depth])
                .expect("ancestor path is valid");
            // Track the child on the path through the re-sort.
            let tracked = (depth < path.len())
                .then(|| self.children(self.at(slot))[new_path[depth] as usize].name_key());
            self.finalize(slot);
            if let Some(key) = tracked
                && let Some(pos) = self
                    .children(self.at(slot))
                    .iter()
                    .position(|c| c.name_key() == key)
            {
                new_path[depth] = pos as u32;
            }
        }
        self.generation += 1;
        self.maybe_compact_names();
        new_path
    }

    /// Re-store all names once unused name bytes (from removed or renamed
    /// items) make up most of the storage.
    fn maybe_compact_names(&mut self) {
        let stored: usize = self.sub.arena.chunks.iter().map(String::len).sum();
        if self.dead_name_bytes > CHUNK_SIZE && self.dead_name_bytes * 2 > stored {
            self.compact_names();
        }
    }

    fn compact_names(&mut self) {
        let arena = &mut self.sub.arena;
        let old = std::mem::take(&mut arena.chunks);
        let name = name_in(&old, &self.sub.root).to_owned();
        arena.set_name(&mut self.sub.root, &name);
        for b in 0..arena.blocks.len() {
            for i in 0..arena.blocks[b].len() {
                let mut n = arena.blocks[b][i];
                let name = name_in(&old, &n);
                arena.set_name(&mut n, name);
                arena.blocks[b][i] = n;
            }
        }
        self.dead_name_bytes = 0;
    }

    /// Detach a node from the tree, fixing up all aggregates. Pseudo-items
    /// (`<Free space>`, `<Unknown>`) cannot be removed. When a real node is
    /// removed from a drive scan, free space grows by the same amount so the
    /// volume total stays constant. Returns a copy of the removed node (its
    /// children are gone).
    pub fn remove(&mut self, path: &[u32]) -> Option<Node> {
        let (&last, parent_path) = path.split_last()?;
        let parent = self.slot(parent_path)?;
        let target = self.children(self.at(parent)).get(last as usize)?;
        if target.is_special() {
            return None;
        }
        let node = self.detach(parent, last as usize);
        self.grow_free(node.size);
        self.refresh_ancestors(parent_path);
        Some(node)
    }

    /// Attach a subtree under the directory at `dir_path`, replacing any
    /// child with the same name. Returns the new path. Inserting a real node
    /// into a drive scan shrinks free space so the volume total stays
    /// constant.
    pub fn insert(&mut self, dir_path: &[u32], sub: Subtree) -> Option<NodePath> {
        let dir = self.slot(dir_path)?;
        if !self.at(dir).is_dir() {
            return None;
        }
        let Subtree {
            root,
            arena: mut src,
        } = sub;
        // An item with the same name is being replaced.
        let existing = {
            let name = src.name(&root);
            self.children(self.at(dir))
                .iter()
                .position(|c| self.name(c).eq_ignore_ascii_case(name))
        };
        if let Some(k) = existing {
            let old = self.detach(dir, k);
            if !old.is_special() {
                self.grow_free(old.size);
            }
        }
        let node = self.sub.arena.import(&mut src, &root);
        drop(src);
        // Stamp extensions in this tree's ranking and count them.
        let node = self.stamp_new(node);
        let k = self.append(dir, node);
        self.shrink_free(node.size);
        let mut p = dir_path.to_vec();
        p.push(k);
        Some(self.refresh_ancestors(&p))
    }

    /// Assign extension ids to `node` and everything below it (just
    /// imported), and add them to the stats.
    fn stamp_new(&mut self, mut node: Node) -> Node {
        if node.kind() == NodeKind::File {
            let name = self.sub.arena.name(&node).to_owned();
            node.ext = self.ext_id(&name);
            Self::account_ext(&mut self.exts, &node, true);
            return node;
        }
        let mut stack = vec![node.block];
        while let Some(id) = stack.pop() {
            if id == NO_BLOCK {
                continue;
            }
            for i in 0..self.sub.arena.blocks[id as usize].len() {
                let mut n = self.sub.arena.blocks[id as usize][i];
                if n.kind() == NodeKind::File {
                    let name = self.sub.arena.name(&n).to_owned();
                    n.ext = self.ext_id(&name);
                    Self::account_ext(&mut self.exts, &n, true);
                    self.sub.arena.blocks[id as usize][i] = n;
                } else {
                    stack.push(n.block);
                }
            }
        }
        node
    }

    /// Rename a node in place. Returns its new path.
    pub fn rename(&mut self, path: &[u32], new_name: &str) -> Option<NodePath> {
        if path.is_empty() {
            return None;
        }
        let slot = self.slot(path)?;
        let mut node = *self.at(slot);
        // Pseudo-items cannot be renamed.
        if node.is_special() {
            return None;
        }
        self.dead_name_bytes += node.name_len();
        self.sub.arena.set_name(&mut node, new_name);
        if node.kind() == NodeKind::File {
            Self::account_ext(&mut self.exts, &node, false);
            node.ext = self.ext_id(new_name);
            Self::account_ext(&mut self.exts, &node, true);
        }
        *self.at_mut(slot) = node;
        Some(self.refresh_ancestors(path))
    }

    /// Attach `<Free space>` and `<Unknown>` under the root for a drive scan
    /// (WinDirStat style). `total` and `free` are the volume figures from the
    /// OS at scan time; `total == 0` means "not a drive" and does nothing.
    /// Only positive entries are added. Re-attaching replaces old entries.
    pub fn attach_volume_info(&mut self, total: u64, free: u64) {
        if total == 0 {
            return;
        }
        let mut changed = false;
        while let Some(k) = self
            .children(self.root())
            .iter()
            .position(|c| c.is_special())
        {
            self.detach(Slot::Root, k);
            changed = true;
        }
        self.finalize(Slot::Root);
        let scanned = self.root().size;
        let used = total.saturating_sub(free);
        let unknown = used.saturating_sub(scanned);
        for (kind, name, size) in [
            (NodeKind::FreeSpace, FREE_SPACE_NAME, free),
            (NodeKind::Unknown, UNKNOWN_NAME, unknown),
        ] {
            if size > 0 {
                let mut node = Node::new(kind, size, size, 0);
                self.sub.arena.set_name(&mut node, name);
                self.append(Slot::Root, node);
                changed = true;
            }
        }
        if changed {
            self.finalize(Slot::Root);
            self.generation += 1;
        }
    }

    fn special(&self, kind: NodeKind) -> Option<&Node> {
        self.children(self.root()).iter().find(|c| c.kind() == kind)
    }

    fn special_mut(&mut self, kind: NodeKind) -> Option<&mut Node> {
        let block = self.sub.root.block;
        if block == NO_BLOCK {
            return None;
        }
        self.sub.arena.blocks[block as usize]
            .iter_mut()
            .find(|c| c.kind() == kind)
    }

    fn grow_free(&mut self, delta: u64) {
        if let Some(free) = self.special_mut(NodeKind::FreeSpace) {
            free.size = free.size.saturating_add(delta);
        }
    }

    fn shrink_free(&mut self, delta: u64) {
        if let Some(free) = self.special_mut(NodeKind::FreeSpace) {
            free.size = free.size.saturating_sub(delta);
        }
    }

    /// Whether this is a drive scan with pseudo-items attached.
    pub fn is_drive_scan(&self) -> bool {
        self.children(self.root()).iter().any(|c| c.is_special())
    }

    /// Bytes in `<Free space>`, if present.
    pub fn free_bytes(&self) -> Option<u64> {
        self.special(NodeKind::FreeSpace).map(|c| c.size)
    }

    /// Bytes in `<Unknown>`, if present.
    pub fn unknown_bytes(&self) -> Option<u64> {
        self.special(NodeKind::Unknown).map(|c| c.size)
    }

    /// Real scanned bytes (excluding pseudo-items).
    pub fn scanned_bytes(&self) -> u64 {
        let pseudo: u64 = self
            .children(self.root())
            .iter()
            .filter(|c| c.is_special())
            .map(|c| c.size)
            .sum();
        self.root().size.saturating_sub(pseudo)
    }

    /// Real scanned bytes as allocated on disk (excluding pseudo-items).
    pub fn scanned_disk_bytes(&self) -> u64 {
        let pseudo: u64 = self
            .children(self.root())
            .iter()
            .filter(|c| c.is_special())
            .map(|c| c.disk_size)
            .sum();
        self.root().disk_size.saturating_sub(pseudo)
    }

    /// Used bytes on the volume (scanned + unknown), or the root size when
    /// this is not a drive scan.
    pub fn used_bytes(&self) -> u64 {
        if self.is_drive_scan() {
            self.scanned_bytes() + self.unknown_bytes().unwrap_or(0)
        } else {
            self.root().size
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// root (100) / { sub (50) / { a.txt 5, b.bin 15 }, c.txt 30 }
    fn sample_tree() -> Tree {
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(50);
        sub.add_file("a.txt", 5, 5, 5);
        sub.add_file("b.bin", 15, 15, 6);
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(100);
        root.add_dir("sub", sub);
        root.add_file("c.txt", 30, 30, 7);
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn flat_tree(build: impl FnOnce(&mut DirBuilder)) -> Tree {
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        build(&mut root);
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn names(tree: &Tree, node: &Node) -> Vec<String> {
        tree.children(node)
            .iter()
            .map(|c| tree.name(c).to_string())
            .collect()
    }

    #[test]
    fn node_is_compact() {
        assert_eq!(std::mem::size_of::<Node>(), 40);
    }

    #[test]
    fn flags_and_kind_share_bits_with_the_name_length() {
        let mut n = Node::new(NodeKind::Unknown, 0, 0, 0);
        n.set_name(7, MAX_NAME_LEN);
        n.set_expanded(true);
        n.set_flag(HARD_LINK, true);
        assert_eq!(n.kind(), NodeKind::Unknown);
        assert_eq!(n.name_len(), MAX_NAME_LEN);
        assert!(n.expanded() && n.hard_link() && !n.error());
        n.set_expanded(false);
        n.set_name(9, 3);
        assert_eq!(n.kind(), NodeKind::Unknown);
        assert_eq!(n.name_len(), 3);
        assert!(!n.expanded() && n.hard_link());
    }

    #[test]
    fn long_names_are_clipped_at_a_char_boundary() {
        let long = "é".repeat(MAX_NAME_LEN);
        let tree = flat_tree(|r| r.add_file(&long, 1, 1, 0));
        let name = tree.name(&tree.children(tree.root())[0]);
        assert!(name.len() <= MAX_NAME_LEN && name.len() >= MAX_NAME_LEN - 1);
        assert!(name.chars().all(|c| c == 'é'));
    }

    #[test]
    fn names_span_several_chunks() {
        let tree = flat_tree(|r| {
            for i in 0..30_000 {
                r.add_file(&format!("{i:0>60}.dat"), i, i, 0);
            }
        });
        assert!(tree.sub.arena.chunks.len() > 1);
        let p = tree
            .find(Path::new(&format!("/root/{:0>60}.dat", 12345)))
            .unwrap();
        assert_eq!(tree.get(&p).unwrap().size, 12345);
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
        let root = tree.root();
        assert_eq!(root.size, 50);
        assert_eq!(root.files, 3);
        assert_eq!(root.dirs, 1);
        assert_eq!(root.modified(), 100);
        assert_eq!(names(&tree, root), ["c.txt", "sub"]);
        assert_eq!(tree.children(root)[1].size, 20);
        assert_eq!(tree.name(root), "root");
    }

    #[test]
    fn modified_times_are_clamped() {
        let tree = flat_tree(|r| {
            r.add_file("old", 1, 1, -5);
            r.add_file("far", 1, 1, i64::MAX);
        });
        let times: Vec<i64> = tree
            .children(tree.root())
            .iter()
            .map(|c| c.modified())
            .collect();
        assert!(times.contains(&0));
        assert!(times.contains(&(u32::MAX as i64)));
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
        assert_eq!(tree.name(tree.get(&path).unwrap()), "a.txt");
        assert_eq!(
            tree.fs_path(&path).unwrap(),
            PathBuf::from("/root/sub/a.txt")
        );
        assert_eq!(tree.get(&[99]).map(|_| ()), None);
        assert_eq!(tree.get(&[0, 0]).map(|_| ()), None); // a file has no children
    }

    #[test]
    fn find_is_case_insensitive_on_windows_paths() {
        let tree = sample_tree();
        assert!(tree.find(Path::new("/root/SUB/A.TXT")).is_some());
    }

    #[test]
    fn expanded_can_be_toggled_through_get_mut() {
        let mut tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        tree.get_mut(&sub).unwrap().set_expanded(true);
        assert!(tree.get(&sub).unwrap().expanded());
        assert!(!tree.root().expanded());
    }

    #[test]
    fn remove_updates_aggregates_and_generation() {
        let mut tree = sample_tree();
        let before = tree.generation;
        let path = tree.find(Path::new("/root/sub")).unwrap();
        let removed = tree.remove(&path).unwrap();
        assert_eq!(removed.size, 20);
        assert_eq!(tree.root().size, 30);
        assert_eq!(tree.root().files, 1);
        assert_eq!(tree.root().dirs, 0);
        assert!(tree.generation > before);
        assert_eq!(tree.find(Path::new("/root/sub")), None);
        assert_eq!(
            tree.exts.iter().find(|e| &*e.name == ".bin").unwrap().bytes,
            0
        );
        assert!(tree.remove(&[]).is_none());
        assert!(tree.remove(&[5]).is_none());
        // The freed block is reused.
        assert_eq!(tree.sub.arena.free_blocks.len(), 1);
    }

    #[test]
    fn removing_the_last_child_empties_the_directory() {
        let mut tree = sample_tree();
        let a = tree.find(Path::new("/root/sub/a.txt")).unwrap();
        tree.remove(&a).unwrap();
        let b = tree.find(Path::new("/root/sub/b.bin")).unwrap();
        tree.remove(&b).unwrap();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let sub = tree.get(&sub).unwrap();
        assert!(!sub.has_children());
        assert_eq!((sub.size, sub.files), (0, 0));
        assert_eq!(tree.root().size, 30);
    }

    #[test]
    fn insert_adds_new_extension() {
        let mut tree = sample_tree();
        let path = tree.insert(&[], Subtree::file("d.log", 100, 8)).unwrap();
        assert_eq!(tree.name(tree.get(&path).unwrap()), "d.log");
        assert_eq!(tree.root().size, 150);
        assert_eq!(path, vec![0]);
        let log = tree.exts.iter().find(|e| &*e.name == ".log").unwrap();
        assert_eq!(log.bytes, 100);
        assert_eq!(log.files, 1);
    }

    #[test]
    fn insert_into_a_nested_directory() {
        let mut tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let path = tree.insert(&sub, Subtree::file("z.txt", 1, 1)).unwrap();
        assert_eq!(tree.name(tree.get(&path).unwrap()), "z.txt");
        let sub = tree.get(&sub).unwrap();
        assert_eq!(names(&tree, sub), ["b.bin", "a.txt", "z.txt"]);
        assert_eq!(tree.root().size, 51);
        assert_eq!(tree.root().files, 4);
        assert!(tree.find(Path::new("/root/c.txt")).is_some());
    }

    #[test]
    fn insert_replaces_an_item_with_the_same_name() {
        let mut tree = sample_tree();
        let mut arena = Arena::default();
        let mut fresh = DirBuilder::new(0);
        fresh.add_file("new.bin", 7, 7, 0);
        let fresh = fresh.commit(&mut arena);
        let path = tree.insert(&[], Subtree::new(arena, "SUB", fresh)).unwrap();
        assert_eq!(tree.name(tree.get(&path).unwrap()), "SUB");
        assert_eq!(tree.root().size, 37);
        assert_eq!(tree.root().files, 2);
        assert!(tree.find(Path::new("/root/sub/a.txt")).is_none());
        assert!(tree.find(Path::new("/root/sub/new.bin")).is_some());
        let bin = tree.exts.iter().find(|e| &*e.name == ".bin").unwrap();
        assert_eq!((bin.bytes, bin.files), (7, 1));
    }

    #[test]
    fn inserted_subtrees_keep_their_structure() {
        let mut tree = sample_tree();
        let mut arena = Arena::default();
        let mut deep = DirBuilder::new(0);
        deep.add_file("x.txt", 3, 3, 0);
        deep.add_file("y.txt", 4, 4, 0);
        let deep = deep.commit(&mut arena);
        let mut moved = DirBuilder::new(0);
        moved.add_dir("deep", deep);
        moved.add_file("z.txt", 2, 2, 0);
        let moved = moved.commit(&mut arena);
        tree.insert(&[], Subtree::new(arena, "moved", moved))
            .unwrap();
        let deep = tree.find(Path::new("/root/moved/deep")).unwrap();
        let deep = tree.get(&deep).unwrap();
        assert_eq!(deep.size, 7);
        assert_eq!(names(&tree, deep), ["y.txt", "x.txt"]);
        assert_eq!(tree.root().size, 59);
        assert_eq!(tree.root().dirs, 3);
        assert_eq!(tree.exts[0].files, 5); // .txt
    }

    #[test]
    fn rename_moves_the_node_and_keeps_its_contents() {
        let mut tree = sample_tree();
        let path = tree.find(Path::new("/root/sub")).unwrap();
        let new_path = tree.rename(&path, "renamed").unwrap();
        assert_eq!(tree.name(tree.get(&new_path).unwrap()), "renamed");
        assert!(tree.find(Path::new("/root/renamed/a.txt")).is_some());
        assert_eq!(tree.find(Path::new("/root/sub")), None);
        assert_eq!(tree.root().size, 50);
    }

    #[test]
    fn rename_updates_the_extension() {
        let mut tree = sample_tree();
        let path = tree.find(Path::new("/root/c.txt")).unwrap();
        let new_path = tree.rename(&path, "c.log").unwrap();
        let log = tree.exts.iter().position(|e| &*e.name == ".log").unwrap();
        assert_eq!((tree.exts[log].bytes, tree.exts[log].files), (30, 1));
        assert_eq!(tree.exts[0].bytes, 5); // .txt
        assert_eq!(tree.get(&new_path).unwrap().ext as usize, log);
    }

    #[test]
    fn renames_reclaim_name_storage() {
        let mut tree = sample_tree();
        let path = tree.find(Path::new("/root/c.txt")).unwrap();
        let long = "x".repeat(MAX_NAME_LEN);
        for i in 0..3000 {
            let name = if i % 2 == 0 { &long } else { "c.txt" };
            tree.rename(&path, name).unwrap();
        }
        let stored: usize = tree.sub.arena.chunks.iter().map(String::len).sum();
        assert!(stored < 2 * CHUNK_SIZE, "{stored} bytes of names kept");
        assert!(tree.find(Path::new("/root/sub/b.bin")).is_some());
        assert_eq!(tree.name(tree.get(&path).unwrap()), "c.txt");
    }

    #[test]
    fn dominant_ext_descends_to_the_largest_file() {
        let tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!(tree.dominant_ext(tree.get(&sub).unwrap()), 1); // .bin
    }

    #[test]
    fn disk_size_aggregates_separately_from_logical_size() {
        let tree = flat_tree(|r| {
            r.add_file("small.txt", 10, 4096, 1);
            r.add_file("sparse.bin", 100_000, 8192, 2);
        });
        assert_eq!(tree.root().size, 100_010);
        assert_eq!(tree.root().disk_size, 12_288);
        assert_eq!(tree.scanned_disk_bytes(), 12_288);
    }

    #[test]
    fn top_files_keeps_the_largest_in_order() {
        let tree = sample_tree(); // c.txt 30, sub/b.bin 15, sub/a.txt 5
        let top = tree.top_files(2);
        assert_eq!(top.len(), 2);
        assert_eq!(&*top[0].name, "c.txt");
        assert_eq!(&*top[1].name, "b.bin");
        assert_eq!(top[0].size, 30);
        assert_eq!(top[1].path, vec![1, 0]);

        let all = tree.top_files(10);
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|f| &*f.name).collect::<Vec<_>>(),
            ["c.txt", "b.bin", "a.txt"]
        );
        assert!(tree.top_files(0).is_empty());
    }

    #[test]
    fn top_files_skip_hard_links_and_pseudo_items() {
        let mut tree = flat_tree(|r| {
            r.add_file("real.bin", 100, 4096, 1);
            r.add_hard_link("duplicate.bin", 1);
        });
        tree.attach_volume_info(500, 100);
        let top = tree.top_files(10);
        assert_eq!(top.len(), 1);
        assert_eq!(&*top[0].name, "real.bin");
        assert_eq!(top[0].disk_size, 4096);
        assert_eq!(top[0].modified, 1);
    }

    #[test]
    fn hard_link_duplicates_count_as_files_but_not_bytes() {
        let tree = flat_tree(|r| {
            r.add_file("first.bin", 100, 4096, 1);
            r.add_hard_link("second.bin", 1);
        });
        let root = tree.root();
        assert_eq!(root.size, 100);
        assert_eq!(root.disk_size, 4096);
        assert_eq!(root.files, 2);
        let link = &tree.children(root)[1];
        assert_eq!(link.size, 0);
        assert!(link.hard_link());
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
        assert_eq!(tree.root().size, 200);
        // Aggregates ignore pseudo-items.
        assert_eq!(tree.root().files, 3);
        assert_eq!(tree.root().dirs, 1);
        // Extension stats are untouched.
        assert_eq!(tree.exts.len(), 2);
        // Sorted largest-first: free (100) first.
        assert_eq!(tree.name(&tree.children(tree.root())[0]), FREE_SPACE_NAME);

        // Re-attaching replaces the old entries.
        tree.attach_volume_info(300, 100);
        assert_eq!(tree.free_bytes(), Some(100));
        assert_eq!(tree.unknown_bytes(), Some(150));
        assert_eq!(tree.children(tree.root()).len(), 4);
    }

    #[test]
    fn attach_volume_info_skips_zero_entries() {
        let mut tree = sample_tree(); // scanned = 50
        tree.attach_volume_info(50, 0);
        // used = 50, unknown = 0, free = 0 -> no pseudo-items.
        assert!(!tree.is_drive_scan());
        assert_eq!(tree.root().size, 50);

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
        assert_eq!(tree.root().size, 200);
    }

    #[test]
    fn insert_shrinks_free_to_keep_total_constant() {
        let mut tree = sample_tree();
        tree.attach_volume_info(200, 100);
        tree.insert(&[], Subtree::file("d.log", 40, 8)).unwrap();
        assert_eq!(tree.scanned_bytes(), 90);
        assert_eq!(tree.free_bytes(), Some(60));
        assert_eq!(tree.root().size, 200);
    }
}

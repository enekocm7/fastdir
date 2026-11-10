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
//! - Owners (an opt-in scan setting) live per block in a side array of `u16`
//!   indices into a small owner table. A default scan allocates neither, so
//!   `Node` stays 40 bytes.
//!
//! The scanner writes each directory's block straight into the arena when the
//! directory is done, so there is no second copy of the tree at any point.

use crate::categories::{CategoryMap, FileCategory};
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

/// No extension (directories, or files without a dot).
pub const NO_EXT: u16 = u16::MAX;

/// Sentinel owner index: no owner was collected, or it is unknown (for
/// example because the account could not be resolved).
pub const NO_OWNER: u16 = u16::MAX;

/// The owning accounts seen by one scan, as display names: `DOMAIN\user` on
/// Windows, the user name on Unix, or the raw SID / uid when the account
/// could not be resolved.
///
/// Nodes do not carry owner names: each block of children has an optional
/// parallel array of `u16` indices into this table (see [`Arena`]), and the
/// table itself is empty unless the scan opted into owner collection. A
/// default scan therefore pays nothing at all.
#[derive(Clone, Debug, Default)]
pub struct OwnerTable {
    names: Vec<Box<str>>,
    index: HashMap<Box<str>, u16>,
}

impl OwnerTable {
    /// No owners collected yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Number of distinct owners.
    #[inline]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// One owner's display name, by index.
    #[inline]
    pub fn name(&self, index: u16) -> Option<&str> {
        self.names.get(index as usize).map(|n| &**n)
    }

    /// The index of `name`, adding it when new. Returns [`NO_OWNER`] when the
    /// table is full (more than 65534 distinct owners in one scan).
    pub fn intern(&mut self, name: &str) -> u16 {
        if let Some(&index) = self.index.get(name) {
            return index;
        }
        if self.names.len() >= NO_OWNER as usize {
            return NO_OWNER;
        }
        let index = self.names.len() as u16;
        let stored: Box<str> = name.into();
        self.names.push(stored.clone());
        self.index.insert(stored, index);
        index
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Dir = 0,
    File = 1,
    /// Pseudo-item for a drive scan: bytes Windows reports as free.
    FreeSpace = 2,
    /// Pseudo-item for a drive scan: used bytes the scan could not attribute
    /// to a file. Both `<Unknown>` (the unexplained remainder) and the
    /// measured buckets split out of it (`<NTFS metadata>`) have this kind;
    /// their names tell them apart.
    Unknown = 3,
}

/// Display names for the drive pseudo-items (WinDirStat style).
pub const FREE_SPACE_NAME: &str = "<Free space>";
pub const UNKNOWN_NAME: &str = "<Unknown>";
/// Drive pseudo-item (of kind [`NodeKind::Unknown`]) for the NTFS metadata
/// files (`$MFT` and friends) that a directory listing never returns.
pub const NTFS_METADATA_NAME: &str = "<NTFS metadata>";
/// Drive pseudo-item (of kind [`NodeKind::Unknown`]) for the volume shadow
/// copy storage (restore points) in `System Volume Information`.
pub const SHADOW_COPIES_NAME: &str = "<Shadow copies>";

/// Used space measured outside the scan, split out of `<Unknown>` into its
/// own pseudo-items by [`Tree::attach_volume_breakdown`]. `0` means "not
/// measured" and adds no item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnknownBreakdown {
    /// NTFS metadata files (`$MFT`, ...): `<NTFS metadata>`.
    pub ntfs_metadata: u64,
    /// Shadow copy storage on this volume: `<Shadow copies>`.
    pub shadow_copies: u64,
}

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

    /// Identity of this directory's child block, used to cache filtered views.
    /// Only meaningful for directories that have children.
    #[inline]
    pub(crate) fn block_id(&self) -> u32 {
        self.block
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

    /// Stable identity of this node inside its tree: the start of its own
    /// name slot.
    ///
    /// Unlike an index path it does not change when siblings are re-sorted,
    /// and no two nodes in one tree share it. It changes when the node is
    /// renamed or moved, which is exactly when a diff built against the old
    /// tree stops being valid.
    #[inline]
    pub fn id(&self) -> u32 {
        self.name_off
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
    /// Optional owner index for every node in `blocks`, parallel to it. The
    /// outer vector (and each inner array) only exists once a scan collects
    /// owners, so the default scan allocates nothing here.
    block_owners: Option<Vec<Option<Box<[u16]>>>>,
    /// Ids of emptied blocks, reused before growing `blocks`.
    free_blocks: Vec<u32>,
    chunks: Vec<String>,
    /// Distinct owners referenced by `block_owners`.
    owners: OwnerTable,
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

    /// The owners referenced by this tree (empty unless they were collected).
    #[inline]
    pub fn owners(&self) -> &OwnerTable {
        &self.owners
    }

    /// Replace the owner table, e.g. with the scan's shared snapshot.
    pub fn set_owners(&mut self, owners: OwnerTable) {
        self.owners = owners;
    }

    /// The owner index of child `index` of the block `id`, if known.
    #[inline]
    fn owner_in(&self, id: u32, index: usize) -> u16 {
        self.block_owners
            .as_ref()
            .and_then(|blocks| blocks.get(id as usize))
            .and_then(|owners| owners.as_ref())
            .and_then(|owners| owners.get(index).copied())
            .unwrap_or(NO_OWNER)
    }

    /// The owner indices of a whole block, if they were collected.
    #[inline]
    fn owners_of(&self, id: u32) -> Option<&[u16]> {
        self.block_owners
            .as_ref()
            .and_then(|blocks| blocks.get(id as usize))
            .and_then(|owners| owners.as_ref())
            .map(|owners| &**owners)
    }

    /// Store a block of children and return its id ([`NO_BLOCK`] if empty).
    fn push_block(&mut self, nodes: Box<[Node]>, owners: Option<Box<[u16]>>) -> u32 {
        if nodes.is_empty() {
            return NO_BLOCK;
        }
        let id = match self.free_blocks.pop() {
            Some(id) => id,
            None => {
                let id = u32::try_from(self.blocks.len())
                    .ok()
                    .filter(|&id| id != NO_BLOCK)
                    .expect("too many directories");
                self.blocks.push(Box::default());
                id
            }
        };
        self.set_block(id, nodes, owners);
        id
    }

    /// Store one block's children and (optional) owner indices.
    fn set_block(&mut self, id: u32, nodes: Box<[Node]>, owners: Option<Box<[u16]>>) {
        let id = id as usize;
        self.blocks[id] = nodes;
        match owners {
            Some(owners) => {
                debug_assert_eq!(owners.len(), self.blocks[id].len());
                let blocks = self
                    .block_owners
                    .get_or_insert_with(|| vec![None; self.blocks.len()]);
                if blocks.len() <= id {
                    blocks.resize_with(id + 1, || None);
                }
                blocks[id] = Some(owners);
            }
            None => {
                if let Some(blocks) = &mut self.block_owners
                    && let Some(slot) = blocks.get_mut(id)
                {
                    *slot = None;
                }
            }
        }
    }

    /// Replace the children of `dir` with `nodes`, reusing its block id.
    fn set_children(&mut self, dir: &mut Node, nodes: Vec<Node>, owners: Option<Box<[u16]>>) {
        if dir.block == NO_BLOCK {
            dir.block = self.push_block(nodes.into_boxed_slice(), owners);
        } else if nodes.is_empty() {
            let id = dir.block;
            self.blocks[id as usize] = Box::default();
            if let Some(blocks) = &mut self.block_owners
                && let Some(slot) = blocks.get_mut(id as usize)
            {
                *slot = None;
            }
            self.free_blocks.push(id);
            dir.block = NO_BLOCK;
        } else {
            self.set_block(dir.block, nodes.into_boxed_slice(), owners);
        }
    }

    /// Take a block's owner indices out for editing (re-inserted by
    /// [`Self::set_children`]).
    fn take_owners(&mut self, id: u32) -> Option<Vec<u16>> {
        let blocks = self.block_owners.as_mut()?;
        let slot = blocks.get_mut(id as usize)?;
        slot.take().map(|owners| owners.into_vec())
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
            if let Some(blocks) = &mut self.block_owners
                && let Some(slot) = blocks.get_mut(id as usize)
            {
                *slot = None;
            }
            self.free_blocks.push(id);
            for c in block.iter() {
                f(c);
                stack.push(c.block);
            }
        }
    }

    /// Map every owner index of `src` into this arena's table (an identity
    /// map when both tables are the scan's shared snapshot).
    fn owner_map(&mut self, src: &OwnerTable) -> Vec<u16> {
        if self.owners.len() == src.len()
            && (0..src.len()).all(|i| self.owners.name(i as u16) == src.name(i as u16))
        {
            return (0..src.len() as u16).collect();
        }
        src.names
            .iter()
            .map(|name| self.owners.intern(name))
            .collect()
    }

    /// Copy `node` and its subtree from `src` into this arena, re-storing its
    /// names here. Returns the node as it is stored in this arena.
    fn import(&mut self, src: &mut Arena, node: &Node, map: &[u16]) -> Node {
        let mut n = *node;
        self.set_name(&mut n, src.name(node));
        if node.block != NO_BLOCK {
            let block = std::mem::take(&mut src.blocks[node.block as usize]);
            let owners: Option<Vec<u16>> = src
                .block_owners
                .as_ref()
                .and_then(|blocks| blocks[node.block as usize].as_ref())
                .map(|owners| {
                    owners
                        .iter()
                        .map(|&owner| map.get(owner as usize).copied().unwrap_or(NO_OWNER))
                        .collect()
                });
            let children: Vec<Node> = block.iter().map(|c| self.import(src, c, map)).collect();
            n.block = self.push_block(
                children.into_boxed_slice(),
                owners.map(Vec::into_boxed_slice),
            );
        }
        n
    }

    /// Move `src` into this arena, renumbering its block and name references
    /// in place. `root_owner` is remapped too. Unlike [`Self::import`] this
    /// never copies a node or a name, which is what live scanning uses to
    /// adopt a finished top-level subtree without a second copy of it.
    fn adopt_from(&mut self, src: &mut Arena, mut root: Node, root_owner: u16) -> (Node, u16) {
        let chunk_base = u32::try_from(self.chunks.len()).expect("too many name chunks");
        let block_base = u32::try_from(self.blocks.len()).expect("too many directories");
        assert!(
            chunk_base as usize + src.chunks.len() <= 1 << (32 - CHUNK_BITS),
            "name storage exceeds 4 GiB"
        );
        assert!(
            block_base as usize + src.blocks.len() < NO_BLOCK as usize,
            "too many directories"
        );
        let map = self.owner_map(&src.owners);
        let remap_owner = |owner: u16| map.get(owner as usize).copied().unwrap_or(NO_OWNER);
        if let Some(blocks) = &mut src.block_owners {
            for owners in blocks.iter_mut().flatten() {
                for owner in owners.iter_mut() {
                    *owner = remap_owner(*owner);
                }
            }
        }
        let name_add = chunk_base << CHUNK_BITS;
        fn remap(n: &mut Node, name_add: u32, block_base: u32) {
            n.name_off += name_add;
            if n.block != NO_BLOCK {
                n.block += block_base;
            }
        }
        remap(&mut root, name_add, block_base);
        for block in &mut src.blocks {
            for n in block.iter_mut() {
                remap(n, name_add, block_base);
            }
        }
        self.blocks.append(&mut src.blocks);
        if let Some(mut owners) = src.block_owners.take() {
            let blocks = self
                .block_owners
                .get_or_insert_with(|| vec![None; block_base as usize]);
            blocks.resize_with(block_base as usize, || None);
            blocks.append(&mut owners);
        }
        self.chunks.append(&mut src.chunks);
        (root, remap_owner(root_owner))
    }
}

/// A tree of nodes with its storage: either a whole scan (inside [`Tree`])
/// or a detached piece, such as a freshly scanned folder or a single moved
/// file, ready to be spliced in with [`Tree::insert`].
#[derive(Debug)]
pub struct Subtree {
    root: Node,
    arena: Arena,
    /// The owner of the root itself, which lives in `arena`'s table (the
    /// parent's block stores its children's owners, not its own).
    root_owner: u16,
}

impl Subtree {
    /// A tree whose root is `root` (usually from [`DirBuilder::commit`] into
    /// `arena`), named `name`.
    pub fn new(arena: Arena, name: &str, root: Node) -> Self {
        Self::new_owned(arena, name, root, NO_OWNER)
    }

    /// Like [`Self::new`], with the root's owner index in `arena`'s table.
    pub fn new_owned(mut arena: Arena, name: &str, mut root: Node, root_owner: u16) -> Self {
        arena.set_name(&mut root, name);
        Self {
            root,
            arena,
            root_owner,
        }
    }

    /// A file whose on-disk size is unknown; assume it matches the logical size.
    pub fn file(name: &str, size: u64, modified: i64) -> Self {
        Self::file_on_disk(name, size, size, modified)
    }

    /// A file with a known logical and allocated (on-disk) size.
    pub fn file_on_disk(name: &str, size: u64, disk_size: u64, modified: i64) -> Self {
        Self::single(name, Node::new(NodeKind::File, size, disk_size, modified))
    }

    /// Another hard link to a file counted elsewhere: a file that adds no
    /// bytes to any total (see [`Node::hard_link`]).
    pub fn hard_link(name: &str, modified: i64) -> Self {
        let mut node = Node::new(NodeKind::File, 0, 0, modified);
        node.set_flag(HARD_LINK, true);
        Self::single(name, node)
    }

    fn single(name: &str, node: Node) -> Self {
        let mut arena = Arena::default();
        arena.chunks.push(String::with_capacity(name.len()));
        // A one-name chunk: `set_name` only starts a new chunk when full.
        let mut root = node;
        let name = clip_name(name);
        arena.chunks[0].push_str(name);
        root.set_name(0, name.len());
        Self {
            root,
            arena,
            root_owner: NO_OWNER,
        }
    }

    #[inline]
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// The root's own owner (see [`Tree::root_owner`]).
    #[inline]
    pub fn root_owner(&self) -> u16 {
        self.root_owner
    }

    /// The owners referenced by this subtree.
    #[inline]
    pub fn owners(&self) -> &OwnerTable {
        self.arena.owners()
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
    /// Owner index per child, parallel to `children`. Only allocated once a
    /// child actually carries an owner, so a default scan pays nothing.
    owners: Option<Vec<u16>>,
    finished: bool,
}

impl DirBuilder {
    pub fn new(modified: i64) -> Self {
        Self {
            node: Node::new(NodeKind::Dir, 0, 0, modified),
            names: String::new(),
            children: Vec::new(),
            owners: None,
            finished: false,
        }
    }

    fn push(&mut self, name: &str, mut node: Node, owner: u16) {
        let name = clip_name(name);
        node.set_name(self.names.len() as u32, name.len());
        self.names.push_str(name);
        if owner != NO_OWNER && self.owners.is_none() {
            self.owners = Some(vec![NO_OWNER; self.children.len()]);
        }
        if let Some(owners) = &mut self.owners {
            owners.push(owner);
        }
        self.children.push(node);
        self.finished = false;
    }

    /// A file with a known logical and allocated (on-disk) size.
    pub fn add_file(&mut self, name: &str, size: u64, disk_size: u64, modified: i64) {
        self.add_file_owned(name, size, disk_size, modified, NO_OWNER);
    }

    /// A file with a known size whose owner was collected.
    pub fn add_file_owned(
        &mut self,
        name: &str,
        size: u64,
        disk_size: u64,
        modified: i64,
        owner: u16,
    ) {
        self.push(
            name,
            Node::new(NodeKind::File, size, disk_size, modified),
            owner,
        );
    }

    /// A later hard link to a file that was already counted. It still counts
    /// as a file, but adds no bytes to any total.
    pub fn add_hard_link(&mut self, name: &str, modified: i64) {
        self.add_hard_link_owned(name, modified, NO_OWNER);
    }

    /// A hard-link duplicate whose owner was collected.
    pub fn add_hard_link_owned(&mut self, name: &str, modified: i64, owner: u16) {
        let mut node = Node::new(NodeKind::File, 0, 0, modified);
        node.set_flag(HARD_LINK, true);
        self.push(name, node, owner);
    }

    /// A directory that is listed but not descended into (junctions,
    /// symlinked directories).
    pub fn add_empty_dir(&mut self, name: &str, modified: i64) {
        self.add_empty_dir_owned(name, modified, NO_OWNER);
    }

    /// A listed-but-not-entered directory whose owner was collected.
    pub fn add_empty_dir_owned(&mut self, name: &str, modified: i64, owner: u16) {
        self.push(name, Node::new(NodeKind::Dir, 0, 0, modified), owner);
    }

    /// A drive pseudo-item (`<Free space>` or `<Unknown>`) read back from an
    /// export, so its bytes are not mistaken for a real file.
    pub fn add_special(
        &mut self,
        kind: NodeKind,
        name: &str,
        size: u64,
        disk_size: u64,
        modified: i64,
    ) {
        debug_assert!(matches!(kind, NodeKind::FreeSpace | NodeKind::Unknown));
        self.push(name, Node::new(kind, size, disk_size, modified), NO_OWNER);
    }

    /// A sub-directory already committed to the same arena.
    pub fn add_dir(&mut self, name: &str, dir: Node) {
        self.add_dir_owned(name, dir, NO_OWNER);
    }

    /// A sub-directory whose own owner was collected.
    pub fn add_dir_owned(&mut self, name: &str, dir: Node, owner: u16) {
        self.push(name, dir, owner);
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
        let name_of = |n: &Node| &names[n.name_off as usize..n.name_off as usize + n.name_len()];
        match &mut self.owners {
            None => sort_by_size(&mut self.children, name_of),
            Some(owners) => {
                // Keep the parallel owner array in step with the sort.
                let mut pairs: Vec<(Node, u16)> = self
                    .children
                    .iter()
                    .copied()
                    .zip(owners.iter().copied())
                    .collect();
                pairs.sort_unstable_by(|(a, _), (b, _)| {
                    b.size.cmp(&a.size).then_with(|| name_of(a).cmp(name_of(b)))
                });
                for (i, (node, owner)) in pairs.into_iter().enumerate() {
                    self.children[i] = node;
                    owners[i] = owner;
                }
            }
        }
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
        let owners = self.owners.map(Vec::into_boxed_slice);
        debug_assert!(
            owners
                .as_ref()
                .is_none_or(|owners| owners.len() == self.children.len())
        );
        self.node.block = arena.push_block(self.children.into_boxed_slice(), owners);
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
    /// Coarse group this extension belongs to (see [`CategoryMap`]).
    pub category: FileCategory,
}

/// Bytes and file count in one file-type category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CategoryTotal {
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
    /// Owner index in [`Tree::owners`], or [`NO_OWNER`].
    pub owner: u16,
}

/// Bytes and file count in one owning account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnerTotal {
    /// Index in [`Tree::owners`], or [`NO_OWNER`] for files whose owner is
    /// unknown (unreadable, or not collected for that entry).
    pub owner: u16,
    pub size: u64,
    pub disk_size: u64,
    pub files: u64,
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
    /// Built-in extension categories plus the user's `[categories]` overrides.
    categories: CategoryMap,
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
            categories: CategoryMap::new(),
            generation: 1,
            dead_name_bytes: 0,
        };
        tree.build_extensions();
        tree
    }

    /// An empty tree rooted at `root_path`, ready to receive finished
    /// top-level subtrees with [`Self::adopt`] while a scan is running.
    pub fn empty(root_path: PathBuf) -> Self {
        let name = root_path.to_string_lossy();
        let mut sub = Subtree::new(Arena::default(), &name, Node::new(NodeKind::Dir, 0, 0, 0));
        sub.root_mut().set_expanded(true);
        let mut tree = Self::new(sub, root_path);
        // Generation 0 tells the UI that nothing has been inserted yet.
        tree.generation = 0;
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
                category: self.categories.category(&name),
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

    /// Recompute the extension ranking from scratch. Live scanning adds
    /// files incrementally, so extension ranks are only provisional until
    /// this runs once the scan is complete.
    pub fn rebuild_extensions(&mut self) {
        self.build_extensions();
    }

    /// The owning accounts, or an empty table when owners were not collected.
    #[inline]
    pub fn owners(&self) -> &OwnerTable {
        self.sub.arena.owners()
    }

    /// Whether this scan collected file owners.
    #[inline]
    pub fn has_owners(&self) -> bool {
        !self.owners().is_empty()
    }

    /// The display name of an owner index (`DOMAIN\user`, a user name, or a
    /// raw SID / uid).
    #[inline]
    pub fn owner_name(&self, owner: u16) -> Option<&str> {
        self.owners().name(owner)
    }

    /// The owner of the node at `path`, or [`NO_OWNER`] when it is unknown or
    /// owners were not collected.
    pub fn owner_at(&self, path: &[u32]) -> u16 {
        match self.slot(path) {
            Some(Slot::Root) => self.sub.root_owner,
            Some(Slot::In(block, index)) => self.sub.arena.owner_in(block, index),
            None => NO_OWNER,
        }
    }

    /// The owner of the scanned root folder itself.
    #[inline]
    pub fn root_owner(&self) -> u16 {
        self.sub.root_owner
    }

    /// The owner of child `index` of `parent` (both in this tree). Used by
    /// the treemap, which walks nodes without their index paths.
    #[inline]
    pub fn child_owner(&self, parent: &Node, index: usize) -> u16 {
        if parent.block == NO_BLOCK {
            return NO_OWNER;
        }
        self.sub.arena.owner_in(parent.block, index)
    }

    /// Bytes, disk bytes and file count per owner, largest first.
    ///
    /// Only real files carry bytes, so the sizes add up to the scanned total.
    /// Files whose owner is unknown are grouped under [`NO_OWNER`] (so totals
    /// still add up); the result is empty when owners were not collected.
    pub fn owner_totals(&self) -> Vec<OwnerTotal> {
        if self.sub.arena.block_owners.is_none() {
            return Vec::new();
        }
        let mut totals: HashMap<u16, OwnerTotal> = HashMap::new();
        for (id, block) in self.sub.arena.blocks.iter().enumerate() {
            let owners = self.sub.arena.owners_of(id as u32);
            for (i, node) in block.iter().enumerate() {
                if node.kind() != NodeKind::File {
                    continue;
                }
                let owner = owners
                    .and_then(|owners| owners.get(i).copied())
                    .unwrap_or(NO_OWNER);
                let total = totals.entry(owner).or_insert(OwnerTotal {
                    owner,
                    ..OwnerTotal::default()
                });
                total.size += node.size;
                total.disk_size += node.disk_size;
                total.files += 1;
            }
        }
        let mut out: Vec<OwnerTotal> = totals.into_values().collect();
        out.sort_unstable_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| b.files.cmp(&a.files))
                .then_with(|| a.owner.cmp(&b.owner))
        });
        out
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

    /// The built-in category table plus the user's overrides.
    #[inline]
    pub fn categories(&self) -> &CategoryMap {
        &self.categories
    }

    /// Replace the category table (e.g. with `[categories]` overrides) and
    /// re-stamp every extension. Bumps the generation so the treemap redraws.
    pub fn set_category_map(&mut self, categories: CategoryMap) {
        self.categories = categories;
        for ext in &mut self.exts {
            ext.category = self.categories.category(&ext.name);
        }
        self.generation += 1;
    }

    /// The category of a file's extension rank. Unknown ranks (including
    /// [`NO_EXT`]) are [`FileCategory::Other`].
    #[inline]
    pub fn ext_category(&self, ext: u16) -> FileCategory {
        self.exts
            .get(ext as usize)
            .map(|e| e.category)
            .unwrap_or(FileCategory::Other)
    }

    /// Bytes and file count per category, summed from the extension stats.
    /// The totals therefore match the extension view exactly.
    pub fn category_totals(&self) -> [CategoryTotal; FileCategory::COUNT] {
        let mut totals = [CategoryTotal::default(); FileCategory::COUNT];
        for ext in &self.exts {
            let total = &mut totals[ext.category.index()];
            total.bytes += ext.bytes;
            total.files += ext.files;
        }
        totals
    }

    /// The `limit` largest files in the whole tree, largest first.
    ///
    /// Uses a bounded heap, so a drive scan with millions of files only keeps
    /// `limit` candidates in memory. Hard-link duplicates are left out: they
    /// add no bytes and would just repeat a file already in the list.
    pub fn top_files(&self, limit: usize) -> Vec<FileEntry> {
        self.top_files_in(&[], limit)
    }

    /// The `limit` largest files under the node at `root`, largest first
    /// (empty when `root` does not exist or is a file). Paths in the result
    /// are absolute index paths into the whole tree.
    pub fn top_files_in(&self, root: &[u32], limit: usize) -> Vec<FileEntry> {
        self.top_files_in_where(root, limit, |_, _, _| true)
    }

    /// Like [`Tree::top_files_in`], but only files for which `keep` returns
    /// true are ranked. `keep` receives the file's node, name and extension.
    pub fn top_files_in_where<F>(&self, root: &[u32], limit: usize, mut keep: F) -> Vec<FileEntry>
    where
        F: FnMut(&Node, &str, Option<&str>) -> bool,
    {
        let Some(start) = self.get(root) else {
            return Vec::new();
        };
        if limit == 0 || !start.is_dir() {
            return Vec::new();
        }
        // The smallest retained file is at the top of this min-heap.
        let mut heap: BinaryHeap<Reverse<(u64, Box<str>, NodePath)>> =
            BinaryHeap::with_capacity(limit.min(4096));
        let mut stack: Vec<(NodePath, &Node)> = vec![(root.to_vec(), start)];
        while let Some((path, dir)) = stack.pop() {
            for (i, child) in self.children(dir).iter().enumerate() {
                match child.kind() {
                    NodeKind::Dir => {
                        let mut child_path = path.clone();
                        child_path.push(i as u32);
                        stack.push((child_path, child));
                    }
                    NodeKind::File if !child.hard_link() => {
                        if !keep(child, self.name(child), self.ext_name(child)) {
                            continue;
                        }
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
                    path: path.clone(),
                    name,
                    size: node.size,
                    disk_size: node.disk_size,
                    modified: node.modified(),
                    ext: node.ext,
                    owner: self.owner_at(&path),
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
            category: self.categories.category(&name),
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
        let owners = self.sub.arena.take_owners(d.block).map(|mut owners| {
            owners.remove(k);
            owners.into_boxed_slice()
        });
        let exts = &mut self.exts;
        let mut dead = 0usize;
        self.sub.arena.free_subtree(&node, |n| {
            Self::account_ext(exts, n, false);
            dead += n.name_len();
        });
        self.dead_name_bytes += dead;
        self.sub.arena.set_children(&mut d, children, owners);
        *self.at_mut(dir) = d;
        node
    }

    /// Append `node` (already stored in this arena) as the last child of the
    /// directory in `dir`, with `owner` as its owner index. Returns its
    /// position among its siblings.
    fn append(&mut self, dir: Slot, node: Node, owner: u16) -> u32 {
        let mut d = *self.at(dir);
        let mut children = self.children(&d).to_vec();
        children.push(node);
        let k = children.len() as u32 - 1;
        let owners = match self.sub.arena.take_owners(d.block) {
            Some(mut owners) => {
                owners.push(owner);
                Some(owners.into_boxed_slice())
            }
            None if owner != NO_OWNER => {
                let mut owners = vec![NO_OWNER; children.len() - 1];
                owners.push(owner);
                Some(owners.into_boxed_slice())
            }
            None => None,
        };
        self.sub.arena.set_children(&mut d, children, owners);
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
            let Arena {
                blocks,
                chunks,
                block_owners,
                ..
            } = &mut self.sub.arena;
            let chunks: &[String] = chunks;
            let id = dir.block as usize;
            let children = &mut blocks[id];
            match block_owners
                .as_mut()
                .and_then(|blocks| blocks.get_mut(id))
                .and_then(|owners| owners.as_mut())
            {
                None => sort_by_size(children, |n| name_in(chunks, n)),
                Some(owners) => {
                    // Re-sort the parallel owner array along with the nodes.
                    let mut pairs: Vec<(Node, u16)> = children
                        .iter()
                        .copied()
                        .zip(owners.iter().copied())
                        .collect();
                    pairs.sort_unstable_by(|(a, _), (b, _)| {
                        b.size
                            .cmp(&a.size)
                            .then_with(|| name_in(chunks, a).cmp(name_in(chunks, b)))
                    });
                    for (i, (node, owner)) in pairs.into_iter().enumerate() {
                        children[i] = node;
                        owners[i] = owner;
                    }
                }
            }
            aggregate(&mut dir, children);
        }
        *self.at_mut(slot) = dir;
    }

    /// Re-measure the allocated size of every file below `path` with
    /// `measure` and refresh the aggregates and sort order on the way up.
    ///
    /// Used after an operation that changes how the data is stored on disk
    /// (compression) without changing its logical size, so the tree shows the
    /// new size on disk without a rescan.
    pub fn refresh_disk_sizes(
        &mut self,
        path: &[u32],
        mut measure: impl FnMut(&Path) -> Option<u64>,
    ) -> Option<NodePath> {
        let fs = self.fs_path(path)?;
        self.refresh_disk_rec(path, &fs, &mut measure);
        Some(self.refresh_ancestors(path))
    }

    fn refresh_disk_rec(
        &mut self,
        path: &[u32],
        fs: &Path,
        measure: &mut impl FnMut(&Path) -> Option<u64>,
    ) {
        let Some(node) = self.get(path).copied() else {
            return;
        };
        if node.is_dir() {
            let names: Vec<Box<str>> = self
                .children(&node)
                .iter()
                .map(|child| self.name(child).into())
                .collect();
            for (ix, name) in names.iter().enumerate() {
                let mut child = path.to_vec();
                child.push(ix as u32);
                self.refresh_disk_rec(&child, &fs.join(name.as_ref()), measure);
            }
            if let Some(slot) = self.slot(path) {
                self.finalize(slot);
            }
        } else if node.kind() == NodeKind::File
            && !node.hard_link()
            && let Some(disk) = measure(fs)
            && let Some(n) = self.get_mut(path)
        {
            n.disk_size = disk;
        }
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
        self.attach(dir_path, sub, false)
    }

    /// Like [`Self::insert`], but move the subtree's arena into this tree
    /// instead of copying it. Used by live scanning as each finished top-level
    /// folder arrives.
    pub fn adopt(&mut self, dir_path: &[u32], sub: Subtree) -> Option<NodePath> {
        self.attach(dir_path, sub, true)
    }

    fn attach(&mut self, dir_path: &[u32], sub: Subtree, move_arena: bool) -> Option<NodePath> {
        let dir = self.slot(dir_path)?;
        if !self.at(dir).is_dir() {
            return None;
        }
        let Subtree {
            root,
            arena: mut src,
            root_owner,
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
        let (node, owner) = if move_arena {
            self.sub.arena.adopt_from(&mut src, root, root_owner)
        } else {
            let map = self.sub.arena.owner_map(src.owners());
            let owner = map.get(root_owner as usize).copied().unwrap_or(NO_OWNER);
            (self.sub.arena.import(&mut src, &root, &map), owner)
        };
        drop(src);
        // Stamp extensions in this tree's ranking and count them.
        let node = self.stamp_new(node);
        let k = self.append(dir, node, owner);
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
        self.attach_volume_breakdown(total, free, UnknownBreakdown::default());
    }

    /// [`Self::attach_volume_info`], also splitting the measured buckets of
    /// `breakdown` (see [`crate::drives::unknown_breakdown`]) out of
    /// `<Unknown>` into their own items. Buckets are taken in order and each
    /// is capped at what is still unattributed, so the volume total stays
    /// exact and `<Unknown>` keeps only the remainder.
    pub fn attach_volume_breakdown(&mut self, total: u64, free: u64, breakdown: UnknownBreakdown) {
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
        let unattributed = used.saturating_sub(scanned);
        let mut left = unattributed;
        let mut take = |bytes: u64| {
            let taken = bytes.min(left);
            left -= taken;
            taken
        };
        let metadata = take(breakdown.ntfs_metadata);
        let shadow = take(breakdown.shadow_copies);
        for (kind, name, size) in [
            (NodeKind::FreeSpace, FREE_SPACE_NAME, free),
            (NodeKind::Unknown, NTFS_METADATA_NAME, metadata),
            (NodeKind::Unknown, SHADOW_COPIES_NAME, shadow),
            (NodeKind::Unknown, UNKNOWN_NAME, left),
        ] {
            if size > 0 {
                let mut node = Node::new(kind, size, size, 0);
                self.sub.arena.set_name(&mut node, name);
                self.append(Slot::Root, node, NO_OWNER);
                changed = true;
            }
        }
        if changed {
            self.finalize(Slot::Root);
            self.generation += 1;
        }
    }

    /// The pseudo-item of `kind` called `name` under the root, if present.
    fn special_named(&self, kind: NodeKind, name: &str) -> Option<&Node> {
        self.children(self.root())
            .iter()
            .find(|c| c.kind() == kind && self.name(c) == name)
    }

    /// Index paths of every folder that could not be read (access denied,
    /// etc.), in tree order. Their contents are missing from the scan, which
    /// on a drive scan is a large part of `<Unknown>`.
    pub fn unreadable_dirs(&self) -> Vec<NodePath> {
        self.unreadable_dirs_in(&[])
    }

    /// [`Self::unreadable_dirs`] limited to the subtree at `path` (which is
    /// included itself when unreadable). Empty when `path` does not exist.
    pub fn unreadable_dirs_in(&self, path: &[u32]) -> Vec<NodePath> {
        let mut out = Vec::new();
        let Some(start) = self.get(path) else {
            return out;
        };
        let mut stack: Vec<(&Node, NodePath)> = vec![(start, path.to_vec())];
        while let Some((node, p)) = stack.pop() {
            if !node.is_dir() {
                continue;
            }
            if node.error() {
                out.push(p.clone());
            }
            // Push in reverse so children pop in list order.
            for (i, child) in self.children(node).iter().enumerate().rev() {
                if child.is_dir() {
                    let mut cp = p.clone();
                    cp.push(i as u32);
                    stack.push((child, cp));
                }
            }
        }
        out
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

    /// Bytes in `<Unknown>` (the unexplained remainder), if present.
    pub fn unknown_bytes(&self) -> Option<u64> {
        self.special_named(NodeKind::Unknown, UNKNOWN_NAME)
            .map(|c| c.size)
    }

    /// Bytes in `<NTFS metadata>`, if present.
    pub fn ntfs_metadata_bytes(&self) -> Option<u64> {
        self.special_named(NodeKind::Unknown, NTFS_METADATA_NAME)
            .map(|c| c.size)
    }

    /// Bytes in `<Shadow copies>`, if present.
    pub fn shadow_copy_bytes(&self) -> Option<u64> {
        self.special_named(NodeKind::Unknown, SHADOW_COPIES_NAME)
            .map(|c| c.size)
    }

    /// Used bytes the scan could not attribute to a file: `<Unknown>` plus
    /// every bucket split out of it.
    pub fn unattributed_bytes(&self) -> u64 {
        self.children(self.root())
            .iter()
            .filter(|c| c.kind() == NodeKind::Unknown)
            .map(|c| c.size)
            .sum()
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

    /// Used bytes on the volume (scanned + unattributed), or the root size
    /// when this is not a drive scan.
    pub fn used_bytes(&self) -> u64 {
        if self.is_drive_scan() {
            self.scanned_bytes() + self.unattributed_bytes()
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

    /// root (owner alice) / { sub (owner bob) / { a.txt 5 alice, b.bin 15 bob },
    /// c.txt 30 alice }.
    fn owned_tree() -> Tree {
        let mut table = OwnerTable::default();
        let alice = table.intern(r"DOMAIN\alice");
        let bob = table.intern(r"DOMAIN\bob");
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(50);
        sub.add_file_owned("a.txt", 5, 5, 5, alice);
        sub.add_file_owned("b.bin", 15, 15, 6, bob);
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(100);
        root.add_dir_owned("sub", sub, bob);
        root.add_file_owned("c.txt", 30, 30, 7, alice);
        let root = root.commit(&mut arena);
        arena.set_owners(table);
        Tree::new(
            Subtree::new_owned(arena, "root", root, alice),
            PathBuf::from("/root"),
        )
    }

    fn names(tree: &Tree, node: &Node) -> Vec<String> {
        tree.children(node)
            .iter()
            .map(|c| tree.name(c).to_string())
            .collect()
    }

    #[test]
    fn refresh_disk_sizes_updates_files_and_aggregates() {
        let mut tree = sample_tree();
        tree.refresh_disk_sizes(&[], |path| {
            Some(match path.file_name()?.to_str()? {
                "a.txt" => 1,
                "b.bin" => 2,
                "c.txt" => 3,
                _ => return None,
            })
        })
        .unwrap();
        assert_eq!(tree.root().disk_size, 6);
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!(tree.get(&sub).unwrap().disk_size, 3);
        let a = tree.find(Path::new("/root/sub/a.txt")).unwrap();
        assert_eq!(tree.get(&a).unwrap().disk_size, 1);
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
    fn adopt_moves_the_subtree_into_the_arena() {
        let mut tree = sample_tree();
        let mut arena = Arena::default();
        // Force a second name chunk so the adopted root's name offset has to
        // be renumbered.
        arena.chunks.push("x".repeat(CHUNK_SIZE));
        let mut moved = DirBuilder::new(0);
        moved.add_dir("deep", {
            let mut inner = DirBuilder::new(0);
            inner.add_file("y.bin", 7, 7, 0);
            inner.commit(&mut arena)
        });
        let node = moved.commit(&mut arena);
        let sub = Subtree::new(arena, "moved", node);

        let path = tree.adopt(&[], sub).unwrap();
        assert_eq!(tree.name(tree.get(&path).unwrap()), "moved");
        assert_eq!(
            tree.find(Path::new("/root/moved/deep/y.bin")),
            Some(vec![path[0], 0, 0])
        );
        assert_eq!(tree.root().size, 57);
        assert_eq!(tree.root().dirs, 3);
        let bin = tree.exts.iter().find(|e| &*e.name == ".bin").unwrap();
        assert_eq!((bin.bytes, bin.files), (22, 2));
    }

    #[test]
    fn adopt_replaces_an_item_with_the_same_name() {
        let mut tree = sample_tree();
        let mut arena = Arena::default();
        let mut fresh = DirBuilder::new(0);
        fresh.add_file("new.bin", 9, 9, 0);
        let fresh = fresh.commit(&mut arena);
        tree.adopt(&[], Subtree::new(arena, "sub", fresh)).unwrap();
        assert_eq!(tree.root().size, 39);
        assert!(tree.find(Path::new("/root/sub/a.txt")).is_none());
        assert!(tree.find(Path::new("/root/sub/new.bin")).is_some());
    }

    #[test]
    fn empty_tree_starts_at_generation_zero() {
        let tree = Tree::empty(PathBuf::from("/root"));
        assert_eq!(tree.generation, 0);
        assert!(tree.root().is_dir());
        assert!(tree.children(tree.root()).is_empty());
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
    fn top_files_in_limits_to_a_subtree() {
        let tree = sample_tree(); // c.txt 30, sub/b.bin 15, sub/a.txt 5
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let top = tree.top_files_in(&sub, 10);
        assert_eq!(
            top.iter().map(|f| &*f.name).collect::<Vec<_>>(),
            ["b.bin", "a.txt"]
        );
        // Paths stay absolute index paths into the whole tree.
        assert_eq!(top[0].path, vec![1, 0]);
        let file = tree.find(Path::new("/root/c.txt")).unwrap();
        assert!(tree.top_files_in(&file, 10).is_empty());
        assert!(tree.top_files_in(&[9, 9], 10).is_empty());
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
    fn ntfs_metadata_is_split_out_of_unknown() {
        let mut tree = sample_tree(); // scanned = 50
        tree.attach_volume_breakdown(
            200,
            100,
            UnknownBreakdown {
                ntfs_metadata: 30,
                shadow_copies: 0,
            },
        );
        // used = 100, unattributed = 50: 30 metadata + 20 unknown.
        assert_eq!(tree.ntfs_metadata_bytes(), Some(30));
        assert_eq!(tree.unknown_bytes(), Some(20));
        assert_eq!(tree.unattributed_bytes(), 50);
        assert_eq!(tree.used_bytes(), 100);
        assert_eq!(tree.root().size, 200);
        let meta = tree
            .children(tree.root())
            .iter()
            .find(|c| tree.name(c) == NTFS_METADATA_NAME)
            .unwrap();
        assert!(meta.is_special());
        assert_eq!(meta.kind(), NodeKind::Unknown);

        // Metadata never exceeds the unattributed bytes: no `<Unknown>` left.
        tree.attach_volume_breakdown(
            200,
            100,
            UnknownBreakdown {
                ntfs_metadata: 500,
                shadow_copies: 0,
            },
        );
        assert_eq!(tree.ntfs_metadata_bytes(), Some(50));
        assert_eq!(tree.unknown_bytes(), None);
        assert_eq!(tree.root().size, 200);
        assert_eq!(tree.children(tree.root()).len(), 4);

        // Re-attaching without metadata drops the bucket again.
        tree.attach_volume_info(200, 100);
        assert_eq!(tree.ntfs_metadata_bytes(), None);
        assert_eq!(tree.unknown_bytes(), Some(50));
    }

    #[test]
    fn shadow_copies_take_what_metadata_leaves() {
        let mut tree = sample_tree(); // scanned = 50
        tree.attach_volume_breakdown(
            200,
            100,
            UnknownBreakdown {
                ntfs_metadata: 10,
                shadow_copies: 25,
            },
        );
        // unattributed 50 = 10 metadata + 25 shadow + 15 unknown.
        assert_eq!(tree.ntfs_metadata_bytes(), Some(10));
        assert_eq!(tree.shadow_copy_bytes(), Some(25));
        assert_eq!(tree.unknown_bytes(), Some(15));
        assert_eq!(tree.used_bytes(), 100);

        // Shadow storage is capped by what metadata left over.
        tree.attach_volume_breakdown(
            200,
            100,
            UnknownBreakdown {
                ntfs_metadata: 40,
                shadow_copies: 25,
            },
        );
        assert_eq!(tree.ntfs_metadata_bytes(), Some(40));
        assert_eq!(tree.shadow_copy_bytes(), Some(10));
        assert_eq!(tree.unknown_bytes(), None);
        assert_eq!(tree.root().size, 200);
    }

    #[test]
    fn unreadable_dirs_lists_error_folders_in_order() {
        let mut arena = Arena::default();
        let mut denied = DirBuilder::new(0);
        denied.set_error();
        let denied = denied.commit(&mut arena);
        let mut inner_denied = DirBuilder::new(0);
        inner_denied.set_error();
        let inner_denied = inner_denied.commit(&mut arena);
        let mut ok = DirBuilder::new(0);
        ok.add_file("f.txt", 5, 5, 0);
        ok.add_dir("locked", inner_denied);
        let ok = ok.commit(&mut arena);
        let mut root = DirBuilder::new(0);
        root.add_dir("ok", ok);
        root.add_dir("denied", denied);
        let root = root.commit(&mut arena);
        let mut tree = Tree::new(Subtree::new(arena, "r", root), PathBuf::from("/r"));
        tree.attach_volume_info(100, 10);

        let paths: Vec<PathBuf> = tree
            .unreadable_dirs()
            .iter()
            .map(|p| tree.fs_path(p).unwrap())
            .collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("/r/ok/locked"), PathBuf::from("/r/denied")]
        );
        let ok = tree.find(Path::new("/r/ok")).unwrap();
        assert_eq!(tree.unreadable_dirs_in(&ok).len(), 1);
        assert!(tree.unreadable_dirs_in(&[99]).is_empty());
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

    /// Richer fixture: root / { docs(a.txt 100, b.log 50), media(c.mp4 1000,
    /// d.mp4 500), e.txt 200 }. Totals: size 1850, files 5, dirs 2.
    fn rich_tree() -> Tree {
        let mut arena = Arena::default();
        let mut docs = DirBuilder::new(10);
        docs.add_file("a.txt", 100, 100, 10);
        docs.add_file("b.log", 50, 50, 11);
        let docs = docs.commit(&mut arena);
        let mut media = DirBuilder::new(12);
        media.add_file("c.mp4", 1000, 1000, 12);
        media.add_file("d.mp4", 500, 500, 13);
        let media = media.commit(&mut arena);
        let mut root = DirBuilder::new(20);
        root.add_dir("docs", docs);
        root.add_dir("media", media);
        root.add_file("e.txt", 200, 200, 14);
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn ext_totals(tree: &Tree) -> std::collections::HashMap<String, (u64, u64)> {
        let mut map = std::collections::HashMap::new();
        let mut stack = vec![tree.root()];
        while let Some(node) = stack.pop() {
            for child in tree.children(node) {
                if child.kind() == NodeKind::File {
                    if let Some(ext) = extension_of(tree.name(child)) {
                        let e = map.entry(ext).or_insert((0u64, 0u64));
                        e.0 += child.size;
                        e.1 += 1;
                    }
                } else if child.is_dir() {
                    stack.push(child);
                }
            }
        }
        map
    }

    /// Every aggregate, sort order and extension stat matches a fresh walk.
    fn assert_invariants(tree: &Tree) {
        // Aggregates and sort order, depth first.
        let mut stack = vec![tree.root()];
        while let Some(node) = stack.pop() {
            let children = tree.children(node);
            if node.is_dir() {
                let mut size = 0u64;
                let mut disk = 0u64;
                let mut files = 0u32;
                let mut dirs = 0u32;
                let mut max_modified = 0i64;
                for (i, c) in children.iter().enumerate() {
                    size += c.size;
                    disk += c.disk_size;
                    files = files.saturating_add(c.files);
                    dirs = dirs.saturating_add(c.dirs + c.is_dir() as u32);
                    max_modified = max_modified.max(c.modified());
                    if i > 0 {
                        let (a, b) = (&children[i - 1], c);
                        assert!(
                            a.size > b.size || (a.size == b.size && tree.name(a) <= tree.name(b)),
                            "children not sorted: {} ({}) before {} ({})",
                            tree.name(a),
                            a.size,
                            tree.name(b),
                            b.size,
                        );
                    }
                    if c.is_dir() {
                        stack.push(c);
                    }
                }
                // Pseudo-items and hard links follow the same aggregate rule.
                assert_eq!(node.size, size, "size of {}", tree.name(node));
                assert_eq!(node.disk_size, disk, "disk of {}", tree.name(node));
                assert_eq!(node.files, files, "files of {}", tree.name(node));
                assert_eq!(node.dirs, dirs, "dirs of {}", tree.name(node));
                assert!(
                    node.modified() >= max_modified,
                    "modified of {} older than a child",
                    tree.name(node)
                );
            }
        }
        // Extension stats match a fresh walk (order is not stable across
        // incremental mutations, and emptied buckets stay as zero entries, so
        // compare per-extension and allow stale zeros).
        let expected = ext_totals(tree);
        for stat in &tree.exts {
            let (bytes, files) = expected
                .get(&stat.name.to_string())
                .copied()
                .unwrap_or((0, 0));
            assert_eq!(stat.bytes, bytes, "bytes for {}", stat.name);
            assert_eq!(stat.files, files, "files for {}", stat.name);
        }
        for (ext, (bytes, files)) in &expected {
            let found = tree.exts.iter().find(|e| &*e.name == ext.as_str());
            assert!(found.is_some(), "missing ext bucket for {ext}");
            let found = found.unwrap();
            assert_eq!(
                (found.bytes, found.files),
                (*bytes, *files),
                "totals for {ext}"
            );
        }
        // Every file's rank points at its own extension (or NO_EXT).
        let mut stack = vec![tree.root()];
        while let Some(node) = stack.pop() {
            for child in tree.children(node) {
                if child.kind() == NodeKind::File {
                    match extension_of(tree.name(child)) {
                        Some(ext) => {
                            let id = child.ext as usize;
                            assert!(
                                id < tree.exts.len(),
                                "ext id {id} out of range for {}",
                                tree.name(child)
                            );
                            assert_eq!(&*tree.exts[id].name, ext.as_str());
                        }
                        None => assert_eq!(child.ext, NO_EXT),
                    }
                } else if child.is_dir() {
                    stack.push(child);
                }
            }
        }
    }

    fn ext_bytes(tree: &Tree, ext: &str) -> (u64, u64) {
        tree.exts
            .iter()
            .find(|e| &*e.name == ext)
            .map(|e| (e.bytes, e.files))
            .unwrap_or((0, 0))
    }

    #[test]
    fn category_totals_match_the_extension_view() {
        let tree = rich_tree();
        let totals = tree.category_totals();
        let video = totals[FileCategory::Video.index()];
        let documents = totals[FileCategory::Documents.index()];
        assert_eq!((video.bytes, video.files), (1500, 2));
        assert_eq!((documents.bytes, documents.files), (350, 3));
        // `Other` is empty here; every byte is in exactly one category.
        let sum: u64 = totals.iter().map(|t| t.bytes).sum();
        let files: u64 = totals.iter().map(|t| t.files).sum();
        assert_eq!(sum, tree.exts.iter().map(|e| e.bytes).sum::<u64>());
        assert_eq!(files, tree.exts.iter().map(|e| e.files).sum::<u64>());
        assert_eq!(tree.ext_category(0), tree.exts[0].category);
        assert_eq!(tree.ext_category(NO_EXT), FileCategory::Other);
    }

    #[test]
    fn category_overrides_restamp_extensions() {
        let mut tree = rich_tree();
        let map = CategoryMap::with_overrides([
            ("video".to_string(), vec!["txt".to_string()]),
            ("archives".to_string(), vec!["mp4".to_string()]),
        ]);
        tree.set_category_map(map);
        let totals = tree.category_totals();
        assert_eq!(totals[FileCategory::Video.index()].bytes, 300);
        assert_eq!(totals[FileCategory::Archives.index()].bytes, 1500);
        assert_eq!(totals[FileCategory::Documents.index()].bytes, 50);
        assert_eq!(
            tree.ext_category(tree.exts.iter().position(|e| &*e.name == ".txt").unwrap() as u16),
            FileCategory::Video
        );
        // New extensions created after the override keep using it.
        tree.insert(&[], Subtree::file("new.MP4", 10, 0)).unwrap();
        let mp4 = tree.exts.iter().find(|e| &*e.name == ".mp4").unwrap();
        assert_eq!(mp4.category, FileCategory::Archives);
        assert_eq!(mp4.bytes, 1510);
    }

    #[test]
    fn remove_keeps_totals_sorted_and_ext_stats() {
        let mut tree = rich_tree();
        assert_invariants(&tree);
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (1850, 5, 2)
        );

        // Remove one file from a sub-directory.
        let a = tree.find(Path::new("/root/docs/a.txt")).unwrap();
        tree.remove(&a).unwrap();
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (1750, 4, 2)
        );
        assert_eq!(ext_bytes(&tree, ".txt"), (200, 1));
        assert_eq!(ext_bytes(&tree, ".log"), (50, 1));
        assert_invariants(&tree);

        // Remove a whole directory (media: 1500 bytes, 2 files, + itself).
        let media = tree.find(Path::new("/root/media")).unwrap();
        tree.remove(&media).unwrap();
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (250, 2, 1)
        );
        assert_eq!(ext_bytes(&tree, ".mp4"), (0, 0));
        assert_eq!(tree.find(Path::new("/root/media")), None);
        // Largest-first: e.txt (200) before docs (50).
        assert_eq!(names(&tree, tree.root()), ["e.txt", "docs"]);
        assert_invariants(&tree);
    }

    #[test]
    fn insert_keeps_totals_sorted_and_ext_stats() {
        let mut tree = rich_tree();
        // A file larger than everything becomes the first child.
        let path = tree
            .insert(&[], Subtree::file("huge.log", 2000, 0))
            .unwrap();
        assert_eq!(path, vec![0]);
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (3850, 6, 2)
        );
        assert_eq!(ext_bytes(&tree, ".log"), (2050, 2));
        assert_invariants(&tree);

        // A nested directory keeps its structure and re-sorts its parent.
        let mut arena = Arena::default();
        let mut deep = DirBuilder::new(0);
        deep.add_file("x.txt", 300, 300, 0);
        deep.add_file("y.txt", 400, 400, 0);
        let deep = deep.commit(&mut arena);
        let mut moved = DirBuilder::new(0);
        moved.add_dir("deep", deep);
        let moved = moved.commit(&mut arena);
        let path = tree
            .insert(&[], Subtree::new(arena, "extra", moved))
            .unwrap();
        assert_eq!(tree.name(tree.get(&path).unwrap()), "extra");
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (4550, 8, 4)
        );
        assert_eq!(ext_bytes(&tree, ".txt"), (1000, 4));
        assert_invariants(&tree);
        // Replacing an existing name swaps the bytes instead of adding.
        let before_files = tree.root().files;
        tree.insert(&[], Subtree::file("E.TXT", 10, 0)).unwrap();
        assert_eq!(tree.root().files, before_files - 1 + 1);
        assert_eq!(tree.root().size, 4550 - 200 + 10);
        assert_eq!(ext_bytes(&tree, ".txt"), (1000 - 200 + 10, 4));
        assert_invariants(&tree);
    }

    #[test]
    fn rename_keeps_totals_sorted_and_ext_stats() {
        let mut tree = rich_tree();
        // File rename that changes the extension moves bytes between buckets.
        let e = tree.find(Path::new("/root/e.txt")).unwrap();
        let new_path = tree.rename(&e, "e.md").unwrap();
        assert_eq!(tree.name(tree.get(&new_path).unwrap()), "e.md");
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (1850, 5, 2)
        );
        assert_eq!(ext_bytes(&tree, ".txt"), (100, 1));
        assert_eq!(ext_bytes(&tree, ".md"), (200, 1));
        assert_invariants(&tree);

        // Renaming a directory keeps its contents and totals.
        let docs = tree.find(Path::new("/root/docs")).unwrap();
        let new_docs = tree.rename(&docs, "documents").unwrap();
        assert!(tree.find(Path::new("/root/documents/b.log")).is_some());
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (1850, 5, 2)
        );
        assert_invariants(&tree);
        let _ = new_docs;

        // A rename that only changes case keeps the extension bucket.
        let b = tree.find(Path::new("/root/documents/b.log")).unwrap();
        tree.rename(&b, "B.LOG").unwrap();
        assert_eq!(ext_bytes(&tree, ".log"), (50, 1));
        assert_invariants(&tree);
    }

    #[test]
    fn deterministic_mutation_sequence_keeps_invariants() {
        let mut tree = rich_tree();
        assert_invariants(&tree);
        // Fixed sequence (no RNG): sizes stay small so the tree never empties.
        let inserted = tree.insert(&[], Subtree::file("tmp1.dat", 77, 0)).unwrap();
        assert_invariants(&tree);
        let _ = tree.rename(&inserted, "tmp1.log").unwrap();
        assert_eq!(ext_bytes(&tree, ".log"), (50 + 77, 2));
        assert_invariants(&tree);
        let docs = tree.find(Path::new("/root/docs")).unwrap();
        let _ = tree.rename(&docs, "docs2").unwrap();
        assert_invariants(&tree);
        let b = tree.find(Path::new("/root/docs2/b.log")).unwrap();
        tree.remove(&b).unwrap();
        assert_eq!(ext_bytes(&tree, ".log"), (77, 1));
        assert_invariants(&tree);
        tree.insert(&docs, Subtree::file("fresh.txt", 1000, 0))
            .unwrap();
        assert_invariants(&tree);
        // docs2 (1100) is now the second child after media (1500).
        assert_eq!(
            names(&tree, tree.root())[..2],
            ["media".to_string(), "docs2".to_string()]
        );
        let media = tree.find(Path::new("/root/media")).unwrap();
        tree.remove(&media).unwrap();
        assert_invariants(&tree);
        assert_eq!(
            (tree.root().size, tree.root().files, tree.root().dirs),
            (1377, 4, 1)
        );
        assert_eq!(ext_bytes(&tree, ".mp4"), (0, 0));
    }

    #[test]
    fn owners_are_stored_per_node_and_totalled_per_account() {
        let tree = owned_tree();
        assert!(tree.has_owners());
        assert_eq!(tree.owners().len(), 2);
        assert_eq!(tree.owner_name(0), Some(r"DOMAIN\alice"));
        assert_eq!(tree.root_owner(), 0);
        assert_eq!(tree.owner_at(&[]), 0);

        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!(tree.owner_at(&sub), 1, "the folder's own owner");
        let a = tree.find(Path::new("/root/sub/a.txt")).unwrap();
        let b = tree.find(Path::new("/root/sub/b.bin")).unwrap();
        let c = tree.find(Path::new("/root/c.txt")).unwrap();
        assert_eq!(tree.owner_at(&a), 0);
        assert_eq!(tree.owner_at(&b), 1);
        assert_eq!(tree.owner_at(&c), 0);

        // Per-user totals add up to the scan total.
        let totals = tree.owner_totals();
        assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), 50);
        assert_eq!(totals.iter().map(|t| t.files).sum::<u64>(), 3);
        let alice = totals.iter().find(|t| t.owner == 0).unwrap();
        assert_eq!((alice.size, alice.files), (35, 2));
        let bob = totals.iter().find(|t| t.owner == 1).unwrap();
        assert_eq!((bob.size, bob.files), (15, 1));
        // Largest first.
        assert_eq!(totals[0].owner, 0);
    }

    #[test]
    fn owner_arrays_follow_sorts_and_mutations() {
        let mut tree = owned_tree();
        // `c.txt` (30) sorts before `sub` (20): owners follow the sort.
        let root_children = names(&tree, tree.root());
        assert_eq!(root_children, ["c.txt", "sub"]);
        assert_eq!(tree.owner_at(&[0]), 0);
        assert_eq!(tree.owner_at(&[1]), 1);
        assert_eq!(tree.child_owner(tree.root(), 0), 0);

        // Removing a file removes its owner entry too.
        let c = tree.find(Path::new("/root/c.txt")).unwrap();
        tree.remove(&c).unwrap();
        assert_eq!(names(&tree, tree.root()), ["sub"]);
        assert_eq!(tree.owner_at(&[0]), 1);
        let totals = tree.owner_totals();
        assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), 20);

        // Inserting a subtree from another scan remaps its owner indices.
        let mut table = OwnerTable::default();
        let carol = table.intern(r"DOMAIN\carol");
        assert_eq!(carol, 0, "the other scan's table starts at zero");
        let mut arena = Arena::default();
        let mut dir = DirBuilder::new(1);
        dir.add_file_owned("d.txt", 7, 7, 1, carol);
        let node = dir.commit(&mut arena);
        arena.set_owners(table);
        let path = tree
            .insert(&[], Subtree::new_owned(arena, "new", node, carol))
            .unwrap();
        assert_eq!(tree.owner_name(tree.owner_at(&path)), Some(r"DOMAIN\carol"));
        let carol = tree.owner_at(&path);
        let totals = tree.owner_totals();
        assert!(totals.iter().any(|t| t.owner == carol && t.size == 7));
        assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), 27);
    }

    #[test]
    fn owner_totals_are_empty_without_collection() {
        let tree = sample_tree();
        assert!(!tree.has_owners());
        assert!(tree.owner_totals().is_empty());
        assert_eq!(tree.owner_at(&[]), NO_OWNER);
        assert_eq!(tree.owner_name(NO_OWNER), None);
        // A default tree keeps the per-block owner arrays unallocated.
        assert!(tree.sub.arena.block_owners.is_none());
    }
}

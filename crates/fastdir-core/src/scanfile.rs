//! Versioned binary scan files (`.fdscan`).
//!
//! JSON (`fastdir.scan` and the export module's document) is the portable
//! interchange format, but it is large and slow to load for multi-million
//! entry scans. This module defines a compact binary alternative that keeps
//! all the state a saved scan needs to be reopened faithfully: hard-link
//! markers, drive pseudo-items, unreadable-folder flags, owners and
//! timestamps.
//!
//! # Layout
//!
//! Integers are unsigned LEB128 varints unless stated otherwise.
//!
//! ```text
//! header:
//!   magic       6 bytes   "FDSCAN"
//!   version     u16 LE    BINARY_VERSION (1 is still read)
//!   flags       u16 LE    bit 0: an owner table follows
//!   root_path   varint length + UTF-8 bytes
//!   scanned_at  varint    Unix seconds
//!   owners      varint count, then count * (varint length + UTF-8 bytes)
//!
//! records (pre-order, one per node):
//!   flags       u8        bits 0-2 kind, bit 3 error, bit 4 hard link,
//!                         bit 5 owner present (version 1: bits 0-1 kind,
//!                         bits 2, 3, 4 the same flags)
//!   name        varint length + UTF-8 bytes
//!   size        varint    logical bytes
//!   disk_size   varint    allocated bytes
//!   [dir]       files varint, dirs varint
//!   modified    u32 LE    Unix seconds (0 = unknown)
//!   [owner]     varint    index into the header's owner table
//!   [dir]       varint    child count, then that many child records
//! ```
//!
//! Records are written and read in pre-order with an explicit stack, so a
//! very deep tree never recurses. The reader rebuilds the tree through
//! [`DirBuilder`], exactly like the JSON importer: aggregates are recomputed
//! from the exported children, which matters for a scan truncated with
//! `max_depth` or `include_files == false`.
//!
//! [`import_scan`] sniffs the header and reads either this format or JSON, so
//! callers that accept a saved scan do not need to care which one it is.

use crate::export::{ExportOptions, ImportedScan};
use crate::filter::{Filter, FilterView};
use crate::model::{
    Arena, DirBuilder, MAX_NAME_LEN, NO_OWNER, Node, NodeKind, OwnerTable, Subtree, Tree,
};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

/// File magic at the start of every binary scan.
pub const MAGIC: &[u8; 6] = b"FDSCAN";

/// Current format version. Bump only on an incompatible change.
///
/// Version 2 widened the kind field to three bits for the `<Alternate
/// streams>` pseudo-item; version 1 documents are still read.
pub const VERSION: u16 = 2;
/// Oldest format version the reader understands.
const MIN_VERSION: u16 = 1;

/// File extension (without the dot) used for binary scans.
pub const EXTENSION: &str = "fdscan";

/// Header flag: the document carries an owner table.
const FLAG_OWNERS: u16 = 1 << 0;

const FLAG_ERROR: u8 = 1 << 3;
const FLAG_HARD_LINK: u8 = 1 << 4;
const FLAG_OWNER: u8 = 1 << 5;
/// Files only (version 2): an online-only cloud placeholder.
const FLAG_CLOUD: u8 = 1 << 6;
const KIND_MASK: u8 = 0b111;
/// Version-1 layout: two kind bits, then the same flags one bit lower.
const V1_FLAG_ERROR: u8 = 1 << 2;
const V1_FLAG_HARD_LINK: u8 = 1 << 3;
const V1_FLAG_OWNER: u8 = 1 << 4;
const V1_KIND_MASK: u8 = 0b11;

/// Upper bound on a stored root path, to stop a corrupt file from asking for
/// an unbounded allocation.
const MAX_PATH_BYTES: usize = 1 << 20;
/// Upper bound on one owner display name.
const MAX_OWNER_BYTES: usize = 1 << 16;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn truncated() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "truncated binary scan document",
    )
}

fn push_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

fn push_string(buf: &mut Vec<u8>, text: &str) {
    push_varint(buf, text.len() as u64);
    buf.extend_from_slice(text.as_bytes());
}

/// Write the subtree at `root` as a binary scan. Returns the number of nodes
/// written (including the root).
pub fn export_binary(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    scanned_at: i64,
    out: impl Write,
) -> io::Result<usize> {
    export_binary_filtered(tree, root, options, scanned_at, None, out)
}

/// Like [`export_binary`], but keeps only entries that match `filter` plus
/// their ancestors (the same view the CSV export uses).
pub fn export_binary_filtered(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    scanned_at: i64,
    filter: Option<&Filter>,
    mut out: impl Write,
) -> io::Result<usize> {
    let node = *tree
        .get(root)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "export root not found"))?;
    let view = filter.map(|filter| FilterView::new(tree, filter));

    // Header: magic, version, flags, root path, scan time and owner table.
    let mut header = Vec::with_capacity(64);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());
    let owner_count = tree.owners().len();
    let flags = if owner_count > 0 { FLAG_OWNERS } else { 0 };
    header.extend_from_slice(&flags.to_le_bytes());
    let root_path = tree.fs_path(root).unwrap_or_else(|| tree.root_path.clone());
    push_string(&mut header, &root_path.to_string_lossy());
    push_varint(&mut header, scanned_at.max(0) as u64);
    push_varint(&mut header, owner_count as u64);
    for index in 0..owner_count {
        push_string(
            &mut header,
            tree.owners().name(index as u16).unwrap_or_default(),
        );
    }
    out.write_all(&header)?;

    // Pre-order records. The stack holds the node, its owner and its depth
    // below the exported root.
    let mut count = 0usize;
    let mut scratch = Vec::with_capacity(64);
    let mut stack: Vec<(&Node, u16, usize)> = vec![(&node, tree.owner_at(root), 0)];
    while let Some((node, owner, depth)) = stack.pop() {
        count += 1;
        let kind = node.kind();
        // A directory's record carries the number of child records that
        // follow it, so push those children in reverse below.
        let mut child_count = 0usize;
        if kind == NodeKind::Dir && options.max_depth.is_none_or(|max| depth < max) {
            let children = tree.children(node);
            for i in (0..children.len()).rev() {
                let child = &children[i];
                if !child.is_dir() && !options.include_files {
                    continue;
                }
                if view.as_ref().is_some_and(|view| !view.shows(tree, child)) {
                    continue;
                }
                child_count += 1;
                stack.push((child, tree.child_owner(node, i), depth + 1));
            }
        }
        scratch.clear();
        encode_record(&mut scratch, tree, node, owner, child_count);
        out.write_all(&scratch)?;
    }
    out.flush()?;
    Ok(count)
}

fn encode_record(buf: &mut Vec<u8>, tree: &Tree, node: &Node, owner: u16, child_count: usize) {
    let kind = node.kind();
    let mut flags = kind as u8;
    if node.error() {
        flags |= FLAG_ERROR;
    }
    if node.hard_link() {
        flags |= FLAG_HARD_LINK;
    }
    if node.cloud() {
        flags |= FLAG_CLOUD;
    }
    if owner != NO_OWNER {
        flags |= FLAG_OWNER;
    }
    buf.push(flags);
    push_string(buf, tree.name(node));
    push_varint(buf, node.size);
    push_varint(buf, node.disk_size);
    if kind == NodeKind::Dir {
        push_varint(buf, u64::from(node.files));
        push_varint(buf, u64::from(node.dirs));
    }
    // The in-memory node stores the timestamp as a `u32`.
    buf.extend_from_slice(&(node.modified() as u32).to_le_bytes());
    if owner != NO_OWNER {
        push_varint(buf, u64::from(owner));
    }
    if kind == NodeKind::Dir {
        push_varint(buf, child_count as u64);
    }
}

/// Read a binary scan document written by [`export_binary`].
pub fn import_binary(source: impl Read) -> io::Result<ImportedScan> {
    let mut input = Decoder::new(source);
    let mut magic = [0u8; MAGIC.len()];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("not a FastDir binary scan"));
    }
    let version = input.u16_le()?;
    if !(MIN_VERSION..=VERSION).contains(&version) {
        return Err(invalid(format!(
            "unsupported binary scan version {version}"
        )));
    }
    let flags = input.u16_le()?;
    let root_path = input.string(MAX_PATH_BYTES)?;
    let scanned_at = input.varint()? as i64;
    let owner_count = input.varint()?;
    if owner_count > u64::from(u16::MAX) {
        return Err(invalid("too many owners in binary scan"));
    }
    if flags & FLAG_OWNERS == 0 && owner_count > 0 {
        return Err(invalid("owner table without the owners flag"));
    }
    let mut owners = OwnerTable::default();
    let mut owner_map = Vec::with_capacity(owner_count as usize);
    for _ in 0..owner_count {
        let name = input.string(MAX_OWNER_BYTES)?;
        owner_map.push(owners.intern(&name));
    }

    let first = read_record(&mut input, &owner_map, version)?;
    if first.kind != NodeKind::Dir {
        return Err(invalid("binary scan root must be a folder"));
    }
    let mut arena = Arena::default();
    let mut frames = vec![Frame::new(&first)];
    let mut root: Option<(Node, String, u16)> = None;

    while let Some(frame) = frames.last() {
        // A finished folder is committed and handed to its parent. This
        // cascades through any parents that are now complete too.
        if frame.remaining == 0 {
            let frame = frames.pop().expect("the frame was just observed");
            let node = frame.builder.commit(&mut arena);
            match frames.last_mut() {
                Some(parent) => {
                    parent.builder.add_dir_owned(&frame.name, node, frame.owner);
                    parent.remaining -= 1;
                }
                None => {
                    root = Some((node, frame.name, frame.owner));
                }
            }
            continue;
        }
        let record = read_record(&mut input, &owner_map, version)?;
        if record.kind == NodeKind::Dir {
            frames.push(Frame::new(&record));
        } else {
            let parent = frames.last_mut().expect("a frame is open");
            match record.kind {
                NodeKind::File if record.hard_link => {
                    parent
                        .builder
                        .add_hard_link_owned(&record.name, record.modified, record.owner);
                }
                NodeKind::File if record.cloud => parent.builder.add_cloud_file_owned(
                    &record.name,
                    record.size,
                    record.disk_size,
                    record.modified,
                    record.owner,
                ),
                NodeKind::File => parent.builder.add_file_owned(
                    &record.name,
                    record.size,
                    record.disk_size,
                    record.modified,
                    record.owner,
                ),
                kind => parent.builder.add_special(
                    kind,
                    &record.name,
                    record.size,
                    record.disk_size,
                    record.modified,
                ),
            }
            parent.remaining -= 1;
        }
    }

    let (root_node, root_name, root_owner) =
        root.ok_or_else(|| invalid("binary scan without a root"))?;
    if !input.at_end()? {
        return Err(invalid("trailing data after the binary scan root"));
    }
    arena.set_owners(owners);
    Ok(ImportedScan {
        tree: Tree::new(
            Subtree::new_owned(arena, &root_name, root_node, root_owner),
            PathBuf::from(root_path),
        ),
        scanned_at,
    })
}

/// Read a saved scan in either the binary format or JSON, detected by the
/// file's first bytes.
pub fn import_scan(mut source: impl Read) -> io::Result<ImportedScan> {
    let mut prefix = [0u8; MAGIC.len()];
    let read = read_prefix(&mut source, &mut prefix)?;
    let combined = io::Cursor::new(prefix[..read].to_vec()).chain(source);
    if read == MAGIC.len() && &prefix == MAGIC {
        import_binary(combined)
    } else {
        crate::export::import_json_with_meta(combined)
    }
}

/// Fill `buf` unless the reader ends first, returning the number of bytes
/// read. Unlike `read_exact`, a short document is not an error here: the
/// JSON reader reports it instead.
fn read_prefix(source: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

struct Frame {
    builder: DirBuilder,
    name: String,
    owner: u16,
    remaining: u32,
}

impl Frame {
    fn new(record: &Record) -> Self {
        let mut builder = DirBuilder::new(record.modified);
        if record.error {
            builder.set_error();
        }
        Self {
            builder,
            name: record.name.clone(),
            owner: record.owner,
            remaining: record.child_count,
        }
    }
}

struct Record {
    kind: NodeKind,
    name: String,
    size: u64,
    disk_size: u64,
    modified: i64,
    error: bool,
    hard_link: bool,
    cloud: bool,
    owner: u16,
    child_count: u32,
}

fn read_record<R: Read>(
    input: &mut Decoder<R>,
    owner_map: &[u16],
    version: u16,
) -> io::Result<Record> {
    let flags = input.u8()?;
    // Version 1 packed the kind into two bits; version 2 needs three for the
    // `<Alternate streams>` item, so the other flags moved up one bit.
    let (kind, error_bit, hard_link_bit, owner_bit) = if version == 1 {
        let kind = match flags & V1_KIND_MASK {
            0 => NodeKind::Dir,
            1 => NodeKind::File,
            2 => NodeKind::FreeSpace,
            _ => NodeKind::Unknown,
        };
        (kind, V1_FLAG_ERROR, V1_FLAG_HARD_LINK, V1_FLAG_OWNER)
    } else {
        let kind = match flags & KIND_MASK {
            0 => NodeKind::Dir,
            1 => NodeKind::File,
            2 => NodeKind::FreeSpace,
            3 => NodeKind::Unknown,
            4 => NodeKind::Streams,
            _ => return Err(invalid("unknown node kind in binary scan")),
        };
        (kind, FLAG_ERROR, FLAG_HARD_LINK, FLAG_OWNER)
    };
    let error = flags & error_bit != 0;
    let hard_link = flags & hard_link_bit != 0;
    let cloud = version >= 2 && flags & FLAG_CLOUD != 0;
    let has_owner = flags & owner_bit != 0;
    let name = input.string(MAX_NAME_LEN)?;
    if name.is_empty() {
        return Err(invalid("node without a name"));
    }
    let size = input.varint()?;
    let disk_size = input.varint()?;
    if kind == NodeKind::Dir {
        // Read and discard the stored aggregates: the builder recomputes
        // them from the exported children.
        input.varint()?;
        input.varint()?;
    }
    let modified = i64::from(input.u32_le()?);
    let owner = if has_owner {
        let index = input.varint()?;
        let index = usize::try_from(index).map_err(|_| invalid("owner index out of range"))?;
        *owner_map
            .get(index)
            .ok_or_else(|| invalid("owner index out of range"))?
    } else {
        NO_OWNER
    };
    let child_count = if kind == NodeKind::Dir {
        u32::try_from(input.varint()?).map_err(|_| invalid("child count out of range"))?
    } else {
        0
    };
    Ok(Record {
        kind,
        name,
        size,
        disk_size,
        modified,
        error,
        hard_link,
        cloud,
        owner,
        child_count,
    })
}

/// A buffered reader with the handful of primitives the format needs.
struct Decoder<R: Read> {
    inner: BufReader<R>,
}

impl<R: Read> Decoder<R> {
    fn new(source: R) -> Self {
        Self {
            inner: BufReader::with_capacity(1 << 20, source),
        }
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buf)
    }

    fn u8(&mut self) -> io::Result<u8> {
        let buf = self.inner.fill_buf()?;
        let Some(&byte) = buf.first() else {
            return Err(truncated());
        };
        self.inner.consume(1);
        Ok(byte)
    }

    fn u16_le(&mut self) -> io::Result<u16> {
        let mut bytes = [0u8; 2];
        self.read_exact(&mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32_le(&mut self) -> io::Result<u32> {
        let mut bytes = [0u8; 4];
        self.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut value = 0u64;
        for index in 0..10 {
            let byte = self.u8()?;
            if index == 9 && byte > 1 {
                return Err(invalid("varint overflow"));
            }
            value |= u64::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("varint too long"))
    }

    fn string(&mut self, max: usize) -> io::Result<String> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| invalid("string length out of range"))?;
        if len > max {
            return Err(invalid("string too long"));
        }
        let mut bytes = vec![0u8; len];
        self.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| invalid("string is not valid UTF-8"))
    }

    fn at_end(&mut self) -> io::Result<bool> {
        Ok(self.inner.fill_buf()?.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// root / { sub / { a.txt 5, link.bin (hard link), b.bin 15 }, c.txt 30 }
    /// with a read error on `sub` and an owner table.
    fn sample_tree() -> Tree {
        let mut owners = OwnerTable::default();
        let alice = owners.intern(r"DOMAIN\alice");
        let bob = owners.intern(r"DOMAIN\bob");
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(1_700_000_050);
        sub.add_file_owned("a.txt", 5, 4096, 1_700_000_100, alice);
        sub.add_hard_link_owned("link.bin", 1_700_000_110, bob);
        sub.add_file_owned("b.bin", 15, 15, 1_700_000_200, bob);
        sub.set_error();
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(1_700_000_300);
        root.add_dir_owned("sub", sub, bob);
        root.add_file_owned("c.txt", 30, 30, 1_700_000_400, alice);
        let root = root.commit(&mut arena);
        arena.set_owners(owners);
        Tree::new(
            Subtree::new_owned(arena, "root", root, alice),
            PathBuf::from("/root"),
        )
    }

    fn round_trip(tree: &Tree) -> (Tree, i64) {
        let mut buf = Vec::new();
        export_binary(
            tree,
            &[],
            &ExportOptions::default(),
            1_700_000_500,
            &mut buf,
        )
        .unwrap();
        let imported = import_binary(buf.as_slice()).unwrap();
        (imported.tree, imported.scanned_at)
    }

    #[test]
    fn round_trips_structure_sizes_and_timestamps() {
        let tree = sample_tree();
        let (back, scanned_at) = round_trip(&tree);
        assert_eq!(scanned_at, 1_700_000_500);
        assert_eq!(back.root_path, PathBuf::from("/root"));
        assert_eq!(back.root().size, tree.root().size);
        assert_eq!(back.root().disk_size, tree.root().disk_size);
        assert_eq!(back.root().files, tree.root().files);
        assert_eq!(back.root().dirs, tree.root().dirs);
        let a = back.find(Path::new("/root/sub/a.txt")).unwrap();
        let node = back.get(&a).unwrap();
        assert_eq!((node.size, node.disk_size), (5, 4096));
        assert_eq!(node.modified(), 1_700_000_100);
        assert_eq!(back.name(node), "a.txt");
        // Extension stats are rebuilt from the imported names.
        assert!(back.exts.iter().any(|e| &*e.name == ".txt"));
    }

    #[test]
    fn keeps_cloud_placeholder_markers() {
        let mut arena = Arena::default();
        let mut dir = DirBuilder::new(1_700_000_000);
        dir.add_cloud_file_owned("remote.docx", 5000, 0, 1_700_000_000, NO_OWNER);
        dir.add_file("local.docx", 10, 4096, 1_700_000_000);
        let dir = dir.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", dir), PathBuf::from("/root"));
        let (back, _) = round_trip(&tree);
        let remote = back.find(Path::new("/root/remote.docx")).unwrap();
        let node = back.get(&remote).unwrap();
        assert!(node.cloud());
        assert!(!node.error());
        assert_eq!((node.size, node.disk_size), (5000, 0));
        let local = back.find(Path::new("/root/local.docx")).unwrap();
        assert!(!back.get(&local).unwrap().cloud());
    }

    #[test]
    fn keeps_hard_link_markers() {
        let tree = sample_tree();
        let (back, _) = round_trip(&tree);
        let link = back.find(Path::new("/root/sub/link.bin")).unwrap();
        let node = back.get(&link).unwrap();
        assert!(node.hard_link());
        assert_eq!((node.size, node.disk_size), (0, 0));
        assert_eq!(node.files, 1);
        // The marker did not change the aggregates.
        assert_eq!(back.root().size, tree.root().size);
        assert_eq!(back.root().files, tree.root().files);
    }

    #[test]
    fn round_trips_alternate_streams() {
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(0);
        sub.add_file("a.txt", 5, 5, 0);
        sub.add_streams(7, 8);
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(0);
        root.add_dir("sub", sub);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));

        let (back, _) = round_trip(&tree);
        let path = back
            .find(Path::new("/root/sub/<Alternate streams>"))
            .unwrap();
        let node = back.get(&path).unwrap();
        assert_eq!(node.kind(), NodeKind::Streams);
        assert_eq!((node.size, node.disk_size), (7, 8));
        assert_eq!(back.root().size, 12);
        assert_eq!(back.root().files, 1);
    }

    /// Version 1 packed kinds into two bits; a document written then still
    /// opens after the three-bit layout of version 2.
    #[test]
    fn reads_version_1_documents() {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // no owner table
        push_string(&mut buf, "/root");
        push_varint(&mut buf, 1_700_000_000); // scanned at
        push_varint(&mut buf, 0); // owner count
        // Root directory with one file child.
        buf.push(0); // kind: dir, no flags
        push_string(&mut buf, "root");
        push_varint(&mut buf, 15);
        push_varint(&mut buf, 15);
        push_varint(&mut buf, 1); // files
        push_varint(&mut buf, 0); // dirs
        buf.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        push_varint(&mut buf, 1); // child count
        // File "a.txt", 15 bytes.
        buf.push(1); // kind: file
        push_string(&mut buf, "a.txt");
        push_varint(&mut buf, 15);
        push_varint(&mut buf, 15);
        buf.extend_from_slice(&1_700_000_000u32.to_le_bytes());

        let back = import_binary(buf.as_slice()).unwrap().tree;
        assert_eq!(back.root().files, 1);
        assert_eq!(back.root().size, 15);
        let a = back.find(Path::new("/root/a.txt")).unwrap();
        assert_eq!(back.get(&a).unwrap().size, 15);
    }

    #[test]
    fn keeps_error_flags_and_owners() {
        let tree = sample_tree();
        let (back, _) = round_trip(&tree);
        let sub = back.find(Path::new("/root/sub")).unwrap();
        assert!(back.get(&sub).unwrap().error());
        assert!(back.has_owners());
        let a = back.find(Path::new("/root/sub/a.txt")).unwrap();
        assert_eq!(back.owner_name(back.owner_at(&a)), Some(r"DOMAIN\alice"));
        assert_eq!(back.owner_name(back.root_owner()), Some(r"DOMAIN\alice"));
        let link = back.find(Path::new("/root/sub/link.bin")).unwrap();
        assert_eq!(back.owner_name(back.owner_at(&link)), Some(r"DOMAIN\bob"));
    }

    #[test]
    fn round_trips_drive_pseudo_items() {
        let mut tree = sample_tree();
        tree.attach_volume_info(1_000, 400);
        let (back, _) = round_trip(&tree);
        assert!(back.is_drive_scan());
        assert_eq!(back.free_bytes(), Some(400));
        assert_eq!(back.scanned_bytes(), tree.scanned_bytes());
        // The special entries stay distinguishable from real files.
        assert_eq!(
            back.children(back.root())
                .iter()
                .filter(|n| n.is_special())
                .count(),
            2
        );
    }

    #[test]
    fn honours_export_options() {
        let tree = sample_tree();
        let mut buf = Vec::new();
        let options = ExportOptions {
            max_depth: Some(1),
            include_files: true,
        };
        let nodes = export_binary(&tree, &[], &options, 0, &mut buf).unwrap();
        assert_eq!(nodes, 3); // root, sub, c.txt
        let back = import_binary(buf.as_slice()).unwrap().tree;
        assert!(back.find(Path::new("/root/sub/a.txt")).is_none());

        let mut buf = Vec::new();
        let options = ExportOptions {
            max_depth: None,
            include_files: false,
        };
        export_binary(&tree, &[], &options, 0, &mut buf).unwrap();
        let back = import_binary(buf.as_slice()).unwrap().tree;
        // Only the folders were exported, so the rebuilt root has no files.
        assert_eq!(back.root().files, 0);
        assert!(back.find(Path::new("/root/c.txt")).is_none());
    }

    #[test]
    fn exports_a_subtree() {
        let tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let mut buf = Vec::new();
        export_binary(&tree, &sub, &ExportOptions::default(), 0, &mut buf).unwrap();
        let back = import_binary(buf.as_slice()).unwrap().tree;
        assert_eq!(back.root_path, PathBuf::from("/root/sub"));
        assert_eq!(back.name(back.root()), "sub");
        assert_eq!(back.root().files, 3);
    }

    #[test]
    fn filters_keep_matches_and_ancestors() {
        let tree = sample_tree();
        let filter = Filter::parse("*.bin", "", "", "", "").unwrap();
        let mut buf = Vec::new();
        export_binary_filtered(
            &tree,
            &[],
            &ExportOptions::default(),
            0,
            Some(&filter),
            &mut buf,
        )
        .unwrap();
        let back = import_binary(buf.as_slice()).unwrap().tree;
        assert!(back.find(Path::new("/root/sub/a.txt")).is_none());
        assert!(back.find(Path::new("/root/sub/b.bin")).is_some());
        assert!(back.find(Path::new("/root/c.txt")).is_none());
    }

    #[test]
    fn auto_detection_reads_both_formats() {
        let tree = sample_tree();
        let mut binary = Vec::new();
        export_binary(
            &tree,
            &[],
            &ExportOptions::default(),
            1_700_000_123,
            &mut binary,
        )
        .unwrap();
        let back = import_scan(binary.as_slice()).unwrap();
        assert_eq!(back.scanned_at, 1_700_000_123);
        assert_eq!(back.tree.root().size, tree.root().size);

        let mut json = Vec::new();
        crate::export::export_json(&tree, &[], &ExportOptions::default(), &mut json).unwrap();
        let back = import_scan(json.as_slice()).unwrap();
        assert_eq!(back.tree.root().size, tree.root().size);
    }

    #[test]
    fn rejects_garbage_and_versions() {
        assert!(import_binary(b"not a scan".as_slice()).is_err());
        let mut buf = Vec::new();
        export_binary(&sample_tree(), &[], &ExportOptions::default(), 0, &mut buf).unwrap();
        buf[6] = 99; // version
        assert!(import_binary(buf.as_slice()).is_err());

        let mut buf = Vec::new();
        export_binary(&sample_tree(), &[], &ExportOptions::default(), 0, &mut buf).unwrap();
        buf.push(0); // trailing byte after a complete root
        assert!(import_binary(buf.as_slice()).is_err());
    }

    #[test]
    fn rejects_truncated_documents() {
        let mut buf = Vec::new();
        export_binary(&sample_tree(), &[], &ExportOptions::default(), 0, &mut buf).unwrap();
        for cut in [MAGIC.len(), 12, buf.len() / 2, buf.len() - 1] {
            assert!(
                import_binary(&buf[..cut]).is_err(),
                "a document cut at {cut} must be rejected"
            );
        }
    }

    #[test]
    fn accepts_a_long_name() {
        let long = "é".repeat(MAX_NAME_LEN);
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file(&long, 7, 7, 0);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));
        // The tree clips names to `MAX_NAME_LEN` bytes at a character
        // boundary; the document must carry exactly that clipped name.
        let expected = tree.name(&tree.children(tree.root())[0]).to_string();
        assert!(expected.len() <= MAX_NAME_LEN && expected.len() >= MAX_NAME_LEN - 1);
        let (back, _) = round_trip(&tree);
        let node = &back.children(back.root())[0];
        assert_eq!(back.name(node), expected);
    }
}

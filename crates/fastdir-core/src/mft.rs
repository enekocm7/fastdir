//! Fast NTFS scanning by reading the Master File Table directly.
//!
//! Walking directories costs one kernel round trip per folder (plus the
//! index lookups behind it). Every file on an NTFS volume, however, already
//! has a fixed-size record in the MFT that holds its names (with the parent
//! folder's record number), its timestamps, its attributes and the sizes of
//! its data stream. Reading the MFT sequentially from the raw volume and
//! linking records to their parents rebuilds the same tree in a fraction of
//! the time, because the reads are large and sequential instead of one
//! small random read per folder.
//!
//! This path is opt-in ([`crate::scanner::ScanOptions::use_mft`]) and only
//! works when the process can open the raw volume, which on Windows means
//! running as administrator. Whenever it cannot be used (another file
//! system, no privileges, a network share, an option the MFT reader does not
//! support, a corrupt MFT) [`scan`] returns `None` and the caller falls back
//! to the regular directory walk, so the results never depend on which path
//! ran.
//!
//! The record parsing and the tree building are plain Rust over byte
//! buffers, so they build and are tested on every platform; only reading
//! the volume is Windows-specific.

use crate::model::{Arena, DirBuilder, Node, Subtree};
use crate::scanner::{ExcludedDir, LinkTracker, Progress, ScanOptions, excluded_dirs, round_up};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::Ordering;

/// Record number of the volume's root folder.
pub(crate) const ROOT_RECORD: u64 = 5;
/// Records below this number are NTFS metadata files (`$MFT`, `$LogFile`,
/// `$Bitmap`, `$Extend`, ...). A directory listing never shows them, so the
/// tree leaves them out too; their bytes end up in `<Unknown>` either way.
const FIRST_USER_RECORD: u64 = 16;
/// The low 48 bits of a file reference are the record number, the high 16
/// bits the record's sequence number.
const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// Folders nested deeper than this are not followed. A healthy volume never
/// gets close (paths are limited to 32 767 characters); the cap only stops a
/// corrupt MFT whose parent links form a cycle.
const MAX_DEPTH: usize = 16 * 1024;

// Record header flags.
const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIR: u16 = 0x0002;

// Attribute type codes.
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

// Attribute header flags.
const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;

// `$FILE_NAME` namespaces. A DOS (8.3) name is an alias of a Win32 name in
// the same record, so it is never listed.
const NAMESPACE_DOS: u8 = 2;

// Windows file attributes, spelled out so this module builds everywhere.
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0002;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0004;
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0200;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0800;

/// The named stream that holds the data of a WOF-compressed (CompactOS)
/// file. Its unnamed stream is sparse and allocates nothing.
const WOF_STREAM: &str = "WofCompressedData";

/// Scan `root` by reading the MFT of the NTFS volume it lives on.
///
/// Returns `None` when the MFT cannot be used for this scan; the caller then
/// walks the directories instead. A cancelled scan still returns `Some` (an
/// empty or partial tree), so cancelling never triggers the slow fallback.
pub fn scan(
    root: &Path,
    name: &str,
    progress: &Progress,
    options: &ScanOptions,
) -> Option<Subtree> {
    if !supports(options) {
        return None;
    }
    let volume = read_volume(root, progress)?;
    if progress.cancelled() {
        let mut arena = Arena::default();
        let node = DirBuilder::new(0).commit(&mut arena);
        return Some(Subtree::new(arena, name, node));
    }
    let index = volume.index;
    if !index.is_dir(volume.root) {
        return None;
    }
    Some(build_subtree(
        &index,
        volume.root,
        root,
        name,
        volume.cluster,
        progress,
        options,
    ))
}

/// `true` when the MFT reader honours every option the scan asked for.
/// Following junctions needs the target's path resolution, and owners need
/// a security query per file: both are left to the directory walk.
pub fn supports(options: &ScanOptions) -> bool {
    options.use_mft && !options.follow_junctions && !options.collect_owners
}

/// One name of a file: its parent folder (full file reference, with the
/// sequence number) and the name itself.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Name {
    pub parent: u64,
    pub name: Box<str>,
}

/// What the tree needs from one MFT record, after its extension records
/// have been merged in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Record {
    /// Logical size of the unnamed data stream.
    size: u64,
    /// Allocated size as a directory listing reports it: the compressed
    /// size for compressed or sparse streams, the allocation otherwise.
    alloc: u64,
    /// Allocated size of the `WofCompressedData` stream (CompactOS).
    wof: u64,
    /// Last write time, Unix seconds.
    modified: i64,
    /// Windows file attributes from `$STANDARD_INFORMATION`.
    attrs: u32,
    /// The record's sequence number, checked against parent references so
    /// a stale name (pointing at a reused record) is never attached.
    seq: u16,
    /// [`RECORD_IN_USE`] / [`RECORD_IS_DIR`].
    flags: u16,
    names: Box<[Name]>,
}

impl Record {
    fn in_use(&self) -> bool {
        self.flags & RECORD_IN_USE != 0
    }

    fn is_dir(&self) -> bool {
        self.flags & RECORD_IS_DIR != 0
    }

    fn hidden(&self) -> bool {
        self.attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }

    fn reparse(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    /// The size on disk, with the same rules the directory walk applies to
    /// `FileIdExtdDirectoryInfo`: the allocation is exact for compressed,
    /// sparse and reparse-point files, ordinary files round their logical
    /// size up to the cluster, and WOF files report the size of their
    /// compressed stream.
    fn disk_size(&self, cluster: u64) -> u64 {
        let special = self.attrs
            & (FILE_ATTRIBUTE_COMPRESSED
                | FILE_ATTRIBUTE_SPARSE_FILE
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
            || (self.alloc == 0 && self.size > 0);
        if !special {
            round_up(self.size, cluster)
        } else if self.alloc > 0 {
            self.alloc
        } else if self.size == 0 {
            0
        } else if self.wof > 0 {
            self.wof
        } else {
            round_up(self.size, cluster)
        }
    }
}

/// The parsed contents of one MFT record, before extension records are
/// merged into their base record.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Parsed {
    /// Base record (masked to the record number) when this is an extension
    /// record, 0 for a base record.
    base: u64,
    seq: u16,
    flags: u16,
    /// `(modified, attributes)` from `$STANDARD_INFORMATION`.
    info: Option<(i64, u32)>,
    names: Vec<Name>,
    /// `(size, alloc)` of the unnamed `$DATA` stream (its first segment).
    data: Option<(u64, u64)>,
    /// Allocated size of the `WofCompressedData` stream.
    wof: Option<u64>,
}

/// Every record of one volume, indexed by record number, and the children
/// of every folder.
pub(crate) struct MftIndex {
    records: Vec<Record>,
    /// `children[child_start[d]..child_start[d + 1]]` are the entries of the
    /// folder with record number `d`: the child's record and which of its
    /// names lives in `d`.
    child_start: Vec<u32>,
    children: Vec<(u32, u16)>,
}

impl MftIndex {
    /// Link parsed records into an index. `records` holds the base records
    /// by record number; `extensions` the extension records, which are
    /// merged into their base.
    pub(crate) fn new(mut records: Vec<Record>, extensions: Vec<Parsed>) -> Self {
        merge_extensions(&mut records, extensions);
        let n = records.len();
        let mut counts = vec![0u32; n + 1];
        let parent_of = |records: &[Record], child: usize, name: &Name| -> Option<usize> {
            let parent = (name.parent & RECORD_MASK) as usize;
            let seq = (name.parent >> 48) as u16;
            let p = records.get(parent)?;
            let attachable = (child as u64) >= FIRST_USER_RECORD
                && parent != child
                && p.in_use()
                && p.is_dir()
                && (seq == 0 || p.seq == 0 || seq == p.seq);
            attachable.then_some(parent)
        };
        for (i, r) in records.iter().enumerate() {
            if !r.in_use() {
                continue;
            }
            for name in r.names.iter() {
                if let Some(p) = parent_of(&records, i, name) {
                    counts[p + 1] += 1;
                }
            }
        }
        for i in 1..=n {
            counts[i] += counts[i - 1];
        }
        let child_start = counts.clone();
        let mut fill = counts;
        let mut children = vec![(0u32, 0u16); child_start[n] as usize];
        for (i, r) in records.iter().enumerate() {
            if !r.in_use() {
                continue;
            }
            for (k, name) in r.names.iter().enumerate() {
                if let Some(p) = parent_of(&records, i, name) {
                    children[fill[p] as usize] = (i as u32, k as u16);
                    fill[p] += 1;
                }
            }
        }
        Self {
            records,
            child_start,
            children,
        }
    }

    fn is_dir(&self, record: u64) -> bool {
        self.records
            .get(record as usize)
            .is_some_and(|r| r.in_use() && r.is_dir())
    }

    fn children(&self, dir: u64) -> &[(u32, u16)] {
        let d = dir as usize;
        if d + 1 >= self.child_start.len() {
            return &[];
        }
        &self.children[self.child_start[d] as usize..self.child_start[d + 1] as usize]
    }
}

/// Fold every extension record into its base record: extra names, the
/// unnamed data stream when it moved out of the base, and so on.
fn merge_extensions(records: &mut [Record], extensions: Vec<Parsed>) {
    for ext in extensions {
        let Some(base) = records.get_mut(ext.base as usize) else {
            continue;
        };
        if !base.in_use() {
            continue;
        }
        if !ext.names.is_empty() {
            let mut names = std::mem::take(&mut base.names).into_vec();
            names.extend(ext.names);
            base.names = names.into_boxed_slice();
        }
        if let Some((size, alloc)) = ext.data {
            base.size = size;
            base.alloc = alloc;
        }
        if let Some(wof) = ext.wof {
            base.wof = wof;
        }
        if let Some((modified, attrs)) = ext.info {
            base.modified = modified;
            base.attrs = attrs;
        }
    }
}

/// Split a parsed record into its base-record form, or hand it back as an
/// extension record to merge later.
pub(crate) fn into_record(parsed: Parsed) -> Result<Record, Parsed> {
    if parsed.base != 0 {
        return Err(parsed);
    }
    let (modified, attrs) = parsed.info.unwrap_or((0, 0));
    let (size, alloc) = parsed.data.unwrap_or((0, 0));
    Ok(Record {
        size,
        alloc,
        wof: parsed.wof.unwrap_or(0),
        modified,
        attrs,
        seq: parsed.seq,
        flags: parsed.flags,
        names: parsed.names.into_boxed_slice(),
    })
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// Undo the update-sequence protection of a multi-sector record: the last
/// two bytes of every 512-byte stride were replaced with the sequence
/// number when the record was written, and the originals are kept in the
/// update sequence array. Returns `false` for a torn or invalid record.
pub(crate) fn apply_fixup(rec: &mut [u8]) -> bool {
    const STRIDE: usize = 512;
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (off, count) = (off as usize, count as usize);
    if count == 0 || (count - 1) * STRIDE > rec.len() || off + count * 2 > rec.len() {
        return false;
    }
    let usn = [rec[off], rec[off + 1]];
    for i in 1..count {
        let end = i * STRIDE - 2;
        if rec[end..end + 2] != usn {
            return false;
        }
        let saved = off + i * 2;
        rec[end] = rec[saved];
        rec[end + 1] = rec[saved + 1];
    }
    true
}

/// Convert a Windows 100ns-since-1601 timestamp to Unix seconds, exactly as
/// the directory walk does.
fn filetime_to_unix(t: i64) -> i64 {
    t / 10_000_000 - 11_644_473_600
}

/// One attribute inside a record.
struct Attr<'a> {
    kind: u32,
    flags: u16,
    name: &'a [u8],
    /// The whole attribute, header included.
    raw: &'a [u8],
    resident: bool,
}

impl<'a> Attr<'a> {
    /// The value of a resident attribute.
    fn value(&self) -> Option<&'a [u8]> {
        if !self.resident {
            return None;
        }
        let len = u32_at(self.raw, 0x10)? as usize;
        let off = u16_at(self.raw, 0x14)? as usize;
        self.raw.get(off..off.checked_add(len)?)
    }

    fn name_is(&self, expected: &str) -> bool {
        self.name.len() == expected.len() * 2
            && self
                .name
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .eq(expected.encode_utf16())
    }

    /// `(size, as-listed allocation)` of a data stream, or `None` for a
    /// non-resident segment that does not start the stream (its sizes are
    /// not valid).
    fn stream_sizes(&self) -> Option<(u64, u64)> {
        if self.resident {
            let size = self.value()?.len() as u64;
            // A directory listing reports a resident stream's allocation
            // rounded to the 8-byte attribute alignment.
            return Some((size, size.next_multiple_of(8)));
        }
        if u64_at(self.raw, 0x10)? != 0 {
            return None;
        }
        let allocated = u64_at(self.raw, 0x28)?;
        let size = u64_at(self.raw, 0x30)?;
        let alloc = if self.flags & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0 {
            u64_at(self.raw, 0x40)?
        } else {
            allocated
        };
        Some((size, alloc))
    }

    /// Mapping pairs (data runs) of a non-resident attribute.
    fn runs(&self) -> Option<&'a [u8]> {
        if self.resident {
            return None;
        }
        let off = u16_at(self.raw, 0x20)? as usize;
        self.raw.get(off..)
    }

    /// First VCN of a non-resident segment.
    fn start_vcn(&self) -> Option<u64> {
        if self.resident {
            None
        } else {
            u64_at(self.raw, 0x10)
        }
    }
}

/// Iterate over the attributes of a record whose fixup was applied.
fn attributes(rec: &[u8]) -> impl Iterator<Item = Attr<'_>> {
    let used = u32_at(rec, 0x18).map_or(0, |u| (u as usize).min(rec.len()));
    let mut off = u16_at(rec, 0x14).map_or(usize::MAX, usize::from);
    std::iter::from_fn(move || {
        if off.checked_add(16)? > used {
            return None;
        }
        let kind = u32_at(rec, off)?;
        if kind == ATTR_END {
            return None;
        }
        let len = u32_at(rec, off + 4)? as usize;
        if len < 16 || off + len > used {
            return None;
        }
        let raw = &rec[off..off + len];
        let name_len = raw[9] as usize * 2;
        let name_off = u16_at(raw, 0x0A)? as usize;
        let name = raw.get(name_off..name_off + name_len).unwrap_or(&[]);
        let attr = Attr {
            kind,
            flags: u16_at(raw, 0x0C)?,
            name,
            raw,
            resident: raw[8] == 0,
        };
        off += len;
        Some(attr)
    })
}

/// `true` when `rec` starts with the `FILE` signature.
fn is_file_record(rec: &[u8]) -> bool {
    rec.get(..4) == Some(b"FILE")
}

/// Parse one MFT record (in place: the fixup is applied to `rec`). Returns
/// `None` for an unused, empty or damaged record.
pub(crate) fn parse_record(rec: &mut [u8]) -> Option<Parsed> {
    if !is_file_record(rec) || !apply_fixup(rec) {
        return None;
    }
    let rec: &[u8] = rec;
    let flags = u16_at(rec, 0x16)?;
    if flags & RECORD_IN_USE == 0 {
        return None;
    }
    let mut parsed = Parsed {
        base: u64_at(rec, 0x20)? & RECORD_MASK,
        seq: u16_at(rec, 0x10)?,
        flags: flags & (RECORD_IN_USE | RECORD_IS_DIR),
        ..Parsed::default()
    };
    for attr in attributes(rec) {
        match attr.kind {
            ATTR_STANDARD_INFORMATION => {
                if let Some(v) = attr.value()
                    && let (Some(modified), Some(attrs)) = (u64_at(v, 0x08), u32_at(v, 0x20))
                {
                    parsed.info = Some((filetime_to_unix(modified as i64), attrs));
                }
            }
            ATTR_FILE_NAME => {
                let Some(v) = attr.value() else { continue };
                let (Some(parent), Some(&len), Some(&namespace)) =
                    (u64_at(v, 0), v.get(0x40), v.get(0x41))
                else {
                    continue;
                };
                if namespace == NAMESPACE_DOS {
                    continue;
                }
                let Some(raw) = v.get(0x42..0x42 + len as usize * 2) else {
                    continue;
                };
                let wide: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                parsed.names.push(Name {
                    parent,
                    name: String::from_utf16_lossy(&wide).into_boxed_str(),
                });
            }
            ATTR_DATA => {
                if attr.name.is_empty() {
                    if let Some(sizes) = attr.stream_sizes() {
                        parsed.data = Some(sizes);
                    }
                } else if attr.name_is(WOF_STREAM)
                    && let Some((size, alloc)) = attr.stream_sizes()
                {
                    // A resident WOF stream lives inside the record: it
                    // costs no clusters of its own.
                    parsed.wof = Some(if attr.resident { size } else { alloc });
                }
            }
            _ => {}
        }
    }
    Some(parsed)
}

/// A contiguous range of clusters, or a sparse hole (`lcn: None`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Decode NTFS mapping pairs into cluster runs. `None` on malformed input.
pub(crate) fn decode_runs(mut bytes: &[u8]) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    while let Some((&header, rest)) = bytes.split_first() {
        if header == 0 {
            return Some(runs);
        }
        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || len_size > 8 || off_size > 8 || rest.len() < len_size + off_size {
            return None;
        }
        let mut clusters = 0u64;
        for (i, b) in rest[..len_size].iter().enumerate() {
            clusters |= (*b as u64) << (8 * i);
        }
        let run_lcn = if off_size == 0 {
            None
        } else {
            let raw = &rest[len_size..len_size + off_size];
            let mut delta = 0i64;
            for (i, b) in raw.iter().enumerate() {
                delta |= (*b as i64) << (8 * i);
            }
            // Sign-extend the delta from its encoded width.
            let shift = 64 - 8 * off_size as u32;
            delta = (delta << shift) >> shift;
            lcn = lcn.checked_add(delta)?;
            Some(u64::try_from(lcn).ok()?)
        };
        runs.push(Run {
            lcn: run_lcn,
            clusters,
        });
        bytes = &rest[len_size + off_size..];
    }
    Some(runs)
}

/// Where the `$MFT` file's own data lives, from its first record.
#[derive(Debug, Default)]
pub(crate) struct MftLayout {
    /// `(first VCN, mapping pairs)` of every unnamed `$DATA` segment found.
    pub segments: Vec<(u64, Vec<Run>)>,
    /// Logical size of `$MFT`.
    pub size: u64,
    /// The `$ATTRIBUTE_LIST` value when it is resident, or its runs and
    /// size when it is not.
    pub list: Option<AttrList>,
}

#[derive(Debug)]
pub(crate) enum AttrList {
    Resident(Vec<u8>),
    NonResident { runs: Vec<Run>, size: u64 },
}

/// Collect the `$DATA` segments of a `$MFT` record (record 0 or one of its
/// extension records).
pub(crate) fn mft_layout(rec: &[u8], layout: &mut MftLayout) {
    for attr in attributes(rec) {
        match attr.kind {
            ATTR_DATA if attr.name.is_empty() => {
                if let (Some(vcn), Some(runs)) = (attr.start_vcn(), attr.runs()) {
                    if vcn == 0
                        && let Some(size) = u64_at(attr.raw, 0x30)
                    {
                        layout.size = size;
                    }
                    if let Some(runs) = decode_runs(runs) {
                        layout.segments.push((vcn, runs));
                    }
                }
            }
            ATTR_ATTRIBUTE_LIST if layout.list.is_none() => {
                layout.list = match attr.value() {
                    Some(v) => Some(AttrList::Resident(v.to_vec())),
                    None => attr
                        .runs()
                        .and_then(decode_runs)
                        .zip(u64_at(attr.raw, 0x30))
                        .map(|(runs, size)| AttrList::NonResident { runs, size }),
                };
            }
            _ => {}
        }
    }
}

/// The records an `$ATTRIBUTE_LIST` names as holding unnamed `$DATA`
/// segments, other than the base record `base` itself.
pub(crate) fn data_records_in_list(list: &[u8], base: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 0x1A <= list.len() {
        let (Some(kind), Some(len)) = (u32_at(list, off), u16_at(list, off + 4)) else {
            break;
        };
        let len = len as usize;
        if len < 0x1A || off + len > list.len() {
            break;
        }
        let name_len = list[off + 6];
        let record = u64_at(list, off + 0x10).unwrap_or(0) & RECORD_MASK;
        if kind == ATTR_DATA && name_len == 0 && record != base && !out.contains(&record) {
            out.push(record);
        }
        off += len;
    }
    out
}

/// The `$MFT` runs in VCN order, merged from all segments.
pub(crate) fn merged_runs(segments: &mut [(u64, Vec<Run>)]) -> Vec<Run> {
    segments.sort_by_key(|(vcn, _)| *vcn);
    segments
        .iter()
        .flat_map(|(_, runs)| runs.iter().copied())
        .collect()
}

/// Build the tree below `root_record` from an index.
pub(crate) fn build_subtree(
    index: &MftIndex,
    root_record: u64,
    root_path: &Path,
    name: &str,
    cluster: u64,
    progress: &Progress,
    options: &ScanOptions,
) -> Subtree {
    let ctx = Build {
        index,
        cluster,
        progress,
        options,
        links: LinkTracker::default(),
        excluded_dirs: excluded_dirs(options),
    };
    let arena = Mutex::new(Arena::default());
    let root = build_dir(&ctx, &arena, root_record, root_path, 0, 0);
    Subtree::new(arena.into_inner(), name, root)
}

struct Build<'a> {
    index: &'a MftIndex,
    cluster: u64,
    progress: &'a Progress,
    options: &'a ScanOptions,
    links: LinkTracker,
    excluded_dirs: Vec<ExcludedDir>,
}

/// One folder (and, in parallel, everything below it), mirroring the
/// directory walk's `list_one` + `scan_dir`: the same filters, the same
/// hard-link rule and the same handling of reparse points.
fn build_dir(
    ctx: &Build,
    arena: &Mutex<Arena>,
    record: u64,
    path: &Path,
    modified: i64,
    depth: usize,
) -> Node {
    let mut node = DirBuilder::new(modified);
    if ctx.progress.cancelled() {
        return node.commit(&mut arena.lock());
    }
    if let Some(mut cur) = ctx.progress.current.try_lock() {
        cur.clear();
        cur.push_str(&path.to_string_lossy());
    }
    let options = ctx.options;
    let records = &ctx.index.records;
    let mut subdirs: Vec<(&str, u64, i64)> = Vec::new();
    let (mut files, mut bytes) = (0u64, 0u64);
    for &(child, which) in ctx.index.children(record) {
        let r = &records[child as usize];
        let name = &*r.names[which as usize].name;
        if r.hidden() && options.skip_hidden_system {
            continue;
        }
        if options.is_excluded(name) {
            continue;
        }
        if r.is_dir() {
            if !ctx.excluded_dirs.is_empty()
                && ctx.excluded_dirs.iter().any(|d| d.matches(path, name))
            {
                continue;
            }
            // Junctions, mount points and symlinked folders are listed but
            // not entered, exactly like the walk does by default.
            if r.reparse() || depth + 1 >= MAX_DEPTH {
                node.add_empty_dir(name, r.modified);
            } else {
                subdirs.push((name, child as u64, r.modified));
            }
            continue;
        }
        files += 1;
        bytes += r.size;
        // Only files with several names can be hard links; a zero-length
        // file shares no blocks, so it is never tracked.
        if r.size > 0 && r.names.len() > 1 && ctx.links.is_duplicate(child as u128 + 1) {
            node.add_hard_link(name, r.modified);
        } else {
            node.add_file(name, r.size, r.disk_size(ctx.cluster), r.modified);
        }
    }
    let progress = ctx.progress;
    progress.files.fetch_add(files, Ordering::Relaxed);
    progress.bytes.fetch_add(bytes, Ordering::Relaxed);
    progress
        .dirs
        .fetch_add(subdirs.len() as u64, Ordering::Relaxed);

    if !subdirs.is_empty() {
        let children: Vec<(&str, Node)> = subdirs
            .into_par_iter()
            .map(|(name, child, modified)| {
                let child_path = path.join(name);
                let dir = build_dir(ctx, arena, child, &child_path, modified, depth + 1);
                (name, dir)
            })
            .collect();
        for (name, dir) in children {
            node.add_dir(name, dir);
        }
    }
    node.finish();
    node.commit(&mut arena.lock())
}

/// A volume's MFT, read and indexed.
struct VolumeMft {
    index: MftIndex,
    /// Record number of the scan root.
    root: u64,
    cluster: u64,
}

#[cfg(not(windows))]
fn read_volume(_root: &Path, _progress: &Progress) -> Option<VolumeMft> {
    None
}

/// Read the MFT of the NTFS volume `root` lives on. `None` when the volume
/// is not NTFS or cannot be opened raw (not elevated).
#[cfg(windows)]
fn read_volume(root: &Path, progress: &Progress) -> Option<VolumeMft> {
    win::read_volume(root, progress)
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
        OPEN_EXISTING, ReadFile,
    };
    use windows_sys::Win32::System::IO::{DeviceIoControl, OVERLAPPED};
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

    const GENERIC_READ: u32 = 0x8000_0000;
    /// Bytes read from the volume per request (and parsed per batch).
    const CHUNK_BYTES: usize = 8 * 1024 * 1024;

    /// A handle closed on drop.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: a file handle may be used from any thread; reads pass their
    // own offset, so nothing depends on a shared file pointer.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    fn wide_z(s: &std::ffi::OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    /// A 4 KiB-aligned buffer, so raw volume reads meet any device's
    /// alignment requirement.
    #[repr(C, align(4096))]
    #[derive(Clone, Copy)]
    struct Page([u8; 4096]);

    struct AlignedBuf {
        pages: Vec<Page>,
        len: usize,
    }

    impl AlignedBuf {
        fn new(len: usize) -> Self {
            Self {
                pages: vec![Page([0; 4096]); len.div_ceil(4096)],
                len,
            }
        }

        fn bytes_mut(&mut self) -> &mut [u8] {
            // SAFETY: `pages` is `len.div_ceil(4096)` contiguous plain-byte
            // pages, so the first `len` bytes are in bounds and initialised.
            unsafe { std::slice::from_raw_parts_mut(self.pages.as_mut_ptr().cast(), self.len) }
        }
    }

    /// Read exactly `buf.len()` bytes at `offset`.
    fn read_at(handle: &Handle, offset: u64, buf: &mut [u8]) -> bool {
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.Anonymous.Anonymous.Offset = at as u32;
            ov.Anonymous.Anonymous.OffsetHigh = (at >> 32) as u32;
            let want = (buf.len() - done).min(u32::MAX as usize & !0xFFFF) as u32;
            let mut got = 0u32;
            let ok =
                unsafe { ReadFile(handle.0, buf[done..].as_mut_ptr(), want, &mut got, &mut ov) };
            if ok == 0 || got == 0 {
                return false;
            }
            done += got as usize;
        }
        true
    }

    /// The device path (`\\?\Volume{GUID}`) and root (`...\`) of the volume
    /// `path` lives on, when it is a local NTFS volume.
    fn ntfs_volume(path: &Path) -> Option<Vec<u16>> {
        let abs = std::path::absolute(path).ok()?;
        let mut mount = vec![0u16; 1024];
        if unsafe { GetVolumePathNameW(wide_z(abs.as_os_str()).as_ptr(), mount.as_mut_ptr(), 1024) }
            == 0
        {
            return None;
        }
        let mount = wide_z(std::ffi::OsStr::new(&from_wide(&mount)));
        let mut guid = vec![0u16; 64];
        if unsafe { GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), 64) } == 0
        {
            return None;
        }
        let root = from_wide(&guid);
        let mut fs = vec![0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                wide_z(std::ffi::OsStr::new(&root)).as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                64,
            )
        };
        if ok == 0 || from_wide(&fs) != "NTFS" {
            return None;
        }
        // The volume device is the GUID path without its trailing slash.
        let device = root.trim_end_matches('\\');
        Some(wide_z(std::ffi::OsStr::new(device)))
    }

    /// The MFT record number of the folder at `path`.
    fn record_of(path: &Path) -> Option<u64> {
        let abs = std::path::absolute(path).ok()?;
        let wide = wide_z(abs.as_os_str());
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let handle = Handle(handle);
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
            return None;
        }
        let id = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some(id & RECORD_MASK)
    }

    pub(super) fn read_volume(root: &Path, progress: &Progress) -> Option<VolumeMft> {
        let device = ntfs_volume(root)?;
        let root_record = record_of(root)?;
        let handle = unsafe {
            CreateFileW(
                device.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // Not elevated: opening a raw volume needs administrator rights.
            return None;
        }
        let handle = Handle(handle);

        let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                (&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast(),
                std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let cluster = data.BytesPerCluster as u64;
        let rec_size = data.BytesPerFileRecordSegment as usize;
        if cluster == 0 || rec_size < 512 || rec_size % 512 != 0 {
            return None;
        }
        let mft_offset = (data.MftStartLcn as u64).checked_mul(cluster)?;

        // Record 0 describes `$MFT` itself: where its clusters are.
        let read_record = |offset: u64| -> Option<Vec<u8>> {
            // Reads must cover whole sectors: read whole clusters.
            let start = offset - offset % cluster;
            let span = (offset - start + rec_size as u64).next_multiple_of(cluster);
            let mut buf = AlignedBuf::new(span as usize);
            if !read_at(&handle, start, buf.bytes_mut()) {
                return None;
            }
            let at = (offset - start) as usize;
            let mut rec = buf.bytes_mut()[at..at + rec_size].to_vec();
            (is_file_record(&rec) && apply_fixup(&mut rec)).then_some(rec)
        };
        let first = read_record(mft_offset)?;
        let mut layout = MftLayout::default();
        mft_layout(&first, &mut layout);
        if layout.segments.is_empty() || layout.size == 0 {
            return None;
        }
        // A fragmented `$MFT` keeps more of its runs in extension records.
        if let Some(list) = layout.list.take() {
            let list = match list {
                AttrList::Resident(v) => Some(v),
                AttrList::NonResident { runs, size } => read_stream(&handle, &runs, cluster, size),
            };
            let runs = merged_runs(&mut layout.segments);
            for record in list
                .map(|l| data_records_in_list(&l, 0))
                .unwrap_or_default()
            {
                let offset = record_offset(&runs, cluster, record * rec_size as u64)?;
                let rec = read_record(offset)?;
                mft_layout(&rec, &mut layout);
            }
        }
        let runs = merged_runs(&mut layout.segments);
        let total_bytes = layout.size.min(data.MftValidDataLength.max(0) as u64);
        let total = (total_bytes / rec_size as u64) as usize;
        if total <= ROOT_RECORD as usize {
            return None;
        }

        let mut records = vec![Record::default(); total];
        let mut extensions = Vec::new();
        let ok = read_records(
            &handle,
            &runs,
            cluster,
            rec_size,
            &mut records,
            &mut extensions,
            progress,
        );
        if !ok {
            return None;
        }
        progress.files.store(0, Ordering::Relaxed);
        progress.dirs.store(0, Ordering::Relaxed);
        Some(VolumeMft {
            index: MftIndex::new(records, extensions),
            root: root_record,
            cluster,
        })
    }

    /// The volume byte offset of `$MFT` byte `pos`.
    fn record_offset(runs: &[Run], cluster: u64, pos: u64) -> Option<u64> {
        let mut vcn_bytes = 0u64;
        for run in runs {
            let len = run.clusters * cluster;
            if pos < vcn_bytes + len {
                return Some(run.lcn? * cluster + (pos - vcn_bytes));
            }
            vcn_bytes += len;
        }
        None
    }

    /// Read a whole non-resident stream (an attribute list).
    fn read_stream(handle: &Handle, runs: &[Run], cluster: u64, size: u64) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(size as usize);
        for run in runs {
            let len = (run.clusters * cluster) as usize;
            match run.lcn {
                Some(lcn) => {
                    let mut buf = AlignedBuf::new(len);
                    if !read_at(handle, lcn * cluster, buf.bytes_mut()) {
                        return None;
                    }
                    out.extend_from_slice(buf.bytes_mut());
                }
                None => out.resize(out.len() + len, 0),
            }
            if out.len() as u64 >= size {
                break;
            }
        }
        out.truncate(size as usize);
        Some(out)
    }

    /// Stream the MFT from disk on one thread while the scan pool parses the
    /// previous chunk.
    fn read_records(
        handle: &Handle,
        runs: &[Run],
        cluster: u64,
        rec_size: usize,
        records: &mut [Record],
        extensions: &mut Vec<Parsed>,
        progress: &Progress,
    ) -> bool {
        let chunk_bytes = (CHUNK_BYTES / rec_size).max(1) * rec_size;
        let total = records.len();
        let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, AlignedBuf)>(2);
        std::thread::scope(|scope| {
            let reader = scope.spawn(move || {
                let mut first_record = 0usize;
                let mut buf = AlignedBuf::new(chunk_bytes);
                let mut fill = 0usize;
                for run in runs {
                    let mut left = run.clusters * cluster;
                    let mut pos = run.lcn.map(|l| l * cluster);
                    while left > 0 {
                        if progress.cancelled() || first_record >= total {
                            return true;
                        }
                        let take = (left as usize).min(chunk_bytes - fill);
                        let dest = &mut buf.bytes_mut()[fill..fill + take];
                        match pos {
                            Some(p) => {
                                if !read_at(handle, p, dest) {
                                    return false;
                                }
                                pos = Some(p + take as u64);
                            }
                            None => dest.fill(0),
                        }
                        fill += take;
                        left -= take as u64;
                        if fill == chunk_bytes {
                            let full = std::mem::replace(&mut buf, AlignedBuf::new(chunk_bytes));
                            if tx.send((first_record, full)).is_err() {
                                return true;
                            }
                            first_record += chunk_bytes / rec_size;
                            fill = 0;
                        }
                    }
                }
                if fill > 0 {
                    buf.len = fill - fill % rec_size;
                    let _ = tx.send((first_record, buf));
                }
                true
            });
            for (first, mut buf) in rx {
                if first >= total {
                    continue;
                }
                let bytes = buf.bytes_mut();
                let count = (bytes.len() / rec_size).min(total - first);
                let found = parse_chunk(
                    &mut bytes[..count * rec_size],
                    rec_size,
                    &mut records[first..first + count],
                );
                let used = records[first..first + count]
                    .iter()
                    .filter(|r| r.in_use())
                    .count();
                progress.files.fetch_add(used as u64, Ordering::Relaxed);
                extensions.extend(found);
            }
            reader.join().unwrap_or(false)
        })
    }
}

/// Parse a batch of consecutive records in parallel into `out` (one slot per
/// record), returning the extension records found.
pub(crate) fn parse_chunk(bytes: &mut [u8], rec_size: usize, out: &mut [Record]) -> Vec<Parsed> {
    bytes
        .par_chunks_mut(rec_size)
        .zip(out.par_iter_mut())
        .filter_map(|(rec, slot)| match into_record(parse_record(rec)?) {
            Ok(record) => {
                *slot = record;
                None
            }
            Err(ext) => Some(ext),
        })
        .collect()
}

/// Synthetic MFT records, laid out the way NTFS writes them. Used by the
/// tests and the benchmarks; not part of the public API.
#[doc(hidden)]
pub mod synthetic {
    use super::*;

    pub const REC: usize = 1024;

    /// A file reference with a sequence number.
    pub fn fref(record: u64, seq: u16) -> u64 {
        ((seq as u64) << 48) | record
    }

    /// Builds synthetic MFT records, laid out the way NTFS writes them
    /// (header, attributes, end marker, update sequence protection).
    pub struct RecordBuilder {
        seq: u16,
        flags: u16,
        base: u64,
        attrs: Vec<Vec<u8>>,
    }

    fn attr_header(kind: u32, name: &str, resident: bool, flags: u16, body: &[u8]) -> Vec<u8> {
        let name_w: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let header = if resident { 0x18 } else { 0x48 };
        let name_off = header;
        let value_off = (name_off + name_w.len()).next_multiple_of(8);
        let len = (value_off + body.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = u8::from(!resident);
        a[9] = name.encode_utf16().count() as u8;
        a[0x0A..0x0C].copy_from_slice(&(name_off as u16).to_le_bytes());
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[name_off..name_off + name_w.len()].copy_from_slice(&name_w);
        if resident {
            a[0x10..0x14].copy_from_slice(&(body.len() as u32).to_le_bytes());
            a[0x14..0x16].copy_from_slice(&(value_off as u16).to_le_bytes());
        } else {
            a[0x20..0x22].copy_from_slice(&(value_off as u16).to_le_bytes());
        }
        a[value_off..value_off + body.len()].copy_from_slice(body);
        a
    }

    impl RecordBuilder {
        pub fn new(dir: bool) -> Self {
            Self {
                seq: 1,
                flags: RECORD_IN_USE | if dir { RECORD_IS_DIR } else { 0 },
                base: 0,
                attrs: Vec::new(),
            }
        }

        pub fn seq(mut self, seq: u16) -> Self {
            self.seq = seq;
            self
        }

        pub fn unused(mut self) -> Self {
            self.flags &= !RECORD_IN_USE;
            self
        }

        pub fn extension_of(mut self, base: u64) -> Self {
            self.base = base;
            self
        }

        /// `$STANDARD_INFORMATION` with a Unix modification time.
        pub fn info(mut self, modified: i64, attrs: u32) -> Self {
            let mut v = vec![0u8; 0x48];
            let ft = (modified + 11_644_473_600) * 10_000_000;
            v[0x08..0x10].copy_from_slice(&ft.to_le_bytes());
            v[0x20..0x24].copy_from_slice(&attrs.to_le_bytes());
            self.attrs
                .push(attr_header(ATTR_STANDARD_INFORMATION, "", true, 0, &v));
            self
        }

        pub fn name(mut self, parent: u64, name: &str, namespace: u8) -> Self {
            let wide: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let mut v = vec![0u8; 0x42 + wide.len()];
            v[0..8].copy_from_slice(&parent.to_le_bytes());
            v[0x40] = name.encode_utf16().count() as u8;
            v[0x41] = namespace;
            v[0x42..].copy_from_slice(&wide);
            self.attrs
                .push(attr_header(ATTR_FILE_NAME, "", true, 0, &v));
            self
        }

        /// A resident unnamed data stream of `len` bytes.
        pub fn resident_data(mut self, len: usize) -> Self {
            self.attrs
                .push(attr_header(ATTR_DATA, "", true, 0, &vec![0xAB; len]));
            self
        }

        /// A non-resident data stream.
        #[allow(clippy::too_many_arguments)]
        pub fn data(
            mut self,
            stream: &str,
            size: u64,
            allocated: u64,
            compressed: u64,
            flags: u16,
            start_vcn: u64,
            runs: &[u8],
        ) -> Self {
            let mut a = attr_header(ATTR_DATA, stream, false, flags, runs);
            a[0x10..0x18].copy_from_slice(&start_vcn.to_le_bytes());
            a[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
            a[0x30..0x38].copy_from_slice(&size.to_le_bytes());
            a[0x38..0x40].copy_from_slice(&size.to_le_bytes());
            a[0x40..0x48].copy_from_slice(&compressed.to_le_bytes());
            self.attrs.push(a);
            self
        }

        pub fn attribute_list(mut self, list: &[u8]) -> Self {
            self.attrs
                .push(attr_header(ATTR_ATTRIBUTE_LIST, "", true, 0, list));
            self
        }

        pub fn build(self) -> Vec<u8> {
            let mut rec = vec![0u8; REC];
            rec[0..4].copy_from_slice(b"FILE");
            let usa_off = 0x30usize;
            let usa_count = REC / 512 + 1;
            rec[4..6].copy_from_slice(&(usa_off as u16).to_le_bytes());
            rec[6..8].copy_from_slice(&(usa_count as u16).to_le_bytes());
            rec[0x10..0x12].copy_from_slice(&self.seq.to_le_bytes());
            let first = (usa_off + usa_count * 2).next_multiple_of(8);
            rec[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
            rec[0x16..0x18].copy_from_slice(&self.flags.to_le_bytes());
            rec[0x20..0x28].copy_from_slice(&self.base.to_le_bytes());
            let mut off = first;
            for a in &self.attrs {
                rec[off..off + a.len()].copy_from_slice(a);
                off += a.len();
            }
            rec[off..off + 4].copy_from_slice(&ATTR_END.to_le_bytes());
            off += 8;
            rec[0x18..0x1C].copy_from_slice(&(off as u32).to_le_bytes());
            rec[0x1C..0x20].copy_from_slice(&(REC as u32).to_le_bytes());
            // Update sequence protection.
            let usn = [0x34u8, 0x12];
            rec[usa_off..usa_off + 2].copy_from_slice(&usn);
            for i in 1..usa_count {
                let end = i * 512 - 2;
                let saved = usa_off + i * 2;
                rec[saved] = rec[end];
                rec[saved + 1] = rec[end + 1];
                rec[end..end + 2].copy_from_slice(&usn);
            }
            rec
        }
    }

    /// A synthetic MFT image: `records[i]` is record `i` (missing records
    /// are left zeroed, like unused space).
    pub fn image(records: Vec<(u64, Vec<u8>)>) -> Vec<u8> {
        let n = records.iter().map(|(i, _)| *i).max().unwrap_or(0) as usize + 1;
        let mut img = vec![0u8; n.max(FIRST_USER_RECORD as usize) * REC];
        for (i, rec) in records {
            let i = i as usize;
            img[i * REC..(i + 1) * REC].copy_from_slice(&rec);
        }
        img
    }

    #[cfg(test)]
    pub(crate) fn index_of(mut img: Vec<u8>) -> MftIndex {
        let mut records = vec![Record::default(); img.len() / REC];
        let ext = parse_chunk(&mut img, REC, &mut records);
        MftIndex::new(records, ext)
    }

    /// The root folder (record 5, its own parent).
    pub fn root_record() -> Vec<u8> {
        RecordBuilder::new(true)
            .seq(5)
            .info(0, 0x16)
            .name(fref(ROOT_RECORD, 5), ".", 3)
            .build()
    }

    /// A volume-like MFT with `dirs` folders under the root, each holding
    /// `files` files, ready for [`build`].
    pub fn volume(dirs: usize, files: usize) -> Vec<u8> {
        let root = fref(ROOT_RECORD, 5);
        let mut records = vec![(ROOT_RECORD, root_record())];
        let mut next = FIRST_USER_RECORD;
        for d in 0..dirs {
            let dir = next;
            next += 1;
            records.push((
                dir,
                RecordBuilder::new(true)
                    .info(1_600_000_000, 0)
                    .name(root, &format!("folder {d}"), 3)
                    .build(),
            ));
            for f in 0..files {
                let size = ((d * 7919 + f * 104_729) % 900_000) as u64 + 1;
                let rec = RecordBuilder::new(false)
                    .info(1_600_000_000 + f as i64, 0)
                    .name(fref(dir, 1), &format!("file {f}.dat"), 3);
                let rec = if size < 600 {
                    rec.resident_data(size as usize)
                } else {
                    let alloc = size.next_multiple_of(4096);
                    rec.data("", size, alloc, 0, 0, 0, &[0x21, 0x10, 0x00, 0x01, 0x00])
                };
                records.push((next, rec.build()));
                next += 1;
            }
        }
        image(records)
    }

    /// Parse a whole image and build the tree below the root, as a scan of
    /// the volume root would.
    pub fn build(mut img: Vec<u8>, options: &ScanOptions) -> Subtree {
        let mut records = vec![Record::default(); img.len() / REC];
        let ext = parse_chunk(&mut img, REC, &mut records);
        let index = MftIndex::new(records, ext);
        build_subtree(
            &index,
            ROOT_RECORD,
            Path::new("C:\\"),
            "C:\\",
            4096,
            &Progress::default(),
            options,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::synthetic::*;
    use super::*;
    use std::path::PathBuf;

    fn build(index: &MftIndex, options: &ScanOptions) -> crate::model::Tree {
        let root = PathBuf::from(r"C:\");
        let sub = build_subtree(
            index,
            ROOT_RECORD,
            &root,
            r"C:\",
            4096,
            &Progress::default(),
            options,
        );
        crate::model::Tree::new(sub, root)
    }

    fn child<'a>(tree: &'a crate::model::Tree, path: &[&str]) -> Option<&'a Node> {
        let mut node = tree.root();
        for part in path {
            node = tree.children(node).iter().find(|c| tree.name(c) == *part)?;
        }
        Some(node)
    }

    #[test]
    fn fixup_restores_the_sector_tails_and_rejects_torn_records() {
        let mut rec = RecordBuilder::new(false).resident_data(600).build();
        let mut copy = rec.clone();
        // The last two bytes of each sector hold the update sequence number.
        assert_eq!(rec[510..512], [0x34, 0x12]);
        assert!(apply_fixup(&mut rec));
        // The data bytes under the first sector's tail came back.
        assert_eq!(rec[510..512], [0xAB, 0xAB]);
        // A torn write leaves a sector tail that does not match.
        copy[1022] ^= 0xFF;
        assert!(!apply_fixup(&mut copy));
    }

    #[test]
    fn decodes_data_runs_with_signed_offsets_and_holes() {
        // 0x21: 1-byte length, 2-byte offset. Then a negative delta, then a
        // sparse run (no offset), then the end marker.
        let runs = [
            0x21, 0x10, 0x00, 0x01, // 16 clusters at LCN 256
            0x11, 0x08, 0xF0, // 8 clusters at 256 - 16 = 240
            0x01, 0x04, // 4 sparse clusters
            0x00,
        ];
        let decoded = decode_runs(&runs).unwrap();
        assert_eq!(
            decoded,
            vec![
                Run {
                    lcn: Some(256),
                    clusters: 16
                },
                Run {
                    lcn: Some(240),
                    clusters: 8
                },
                Run {
                    lcn: None,
                    clusters: 4
                },
            ]
        );
        assert!(decode_runs(&[0x21, 0x10]).is_none());
    }

    #[test]
    fn parses_names_info_and_data_of_a_record() {
        let mut rec = RecordBuilder::new(false)
            .seq(7)
            .info(1_700_000_000, FILE_ATTRIBUTE_HIDDEN)
            .name(fref(ROOT_RECORD, 5), "LONGFI~1.TXT", NAMESPACE_DOS)
            .name(fref(ROOT_RECORD, 5), "long file name.txt", 1)
            .data("", 10_000, 12_288, 0, 0, 0, &[0x11, 0x03, 0x20, 0x00])
            .build();
        let p = parse_record(&mut rec).unwrap();
        assert_eq!(p.seq, 7);
        assert_eq!(p.base, 0);
        assert_eq!(p.info, Some((1_700_000_000, FILE_ATTRIBUTE_HIDDEN)));
        assert_eq!(
            p.names,
            vec![Name {
                parent: fref(ROOT_RECORD, 5),
                name: "long file name.txt".into()
            }]
        );
        assert_eq!(p.data, Some((10_000, 12_288)));
        assert!(parse_record(&mut RecordBuilder::new(false).unused().build()).is_none());
        assert!(parse_record(&mut vec![0u8; REC]).is_none());
    }

    #[test]
    fn builds_the_same_tree_a_directory_walk_would() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (0, RecordBuilder::new(false).name(root, "$MFT", 3).build()),
            (ROOT_RECORD, root_record()),
            (
                11,
                RecordBuilder::new(true)
                    .info(0, 0x6)
                    .name(root, "$Extend", 3)
                    .build(),
            ),
            (
                20,
                RecordBuilder::new(true)
                    .seq(2)
                    .info(100, 0)
                    .name(root, "Users", 3)
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(false)
                    .info(200, 0)
                    .name(fref(20, 2), "a.txt", 3)
                    .resident_data(5)
                    .build(),
            ),
            (
                22,
                RecordBuilder::new(false)
                    .info(300, 0)
                    .name(root, "big.bin", 3)
                    .data("", 10_000, 12_288, 0, 0, 0, &[0x11, 0x03, 0x20, 0x00])
                    .build(),
            ),
            // A junction: listed, never entered.
            (
                23,
                RecordBuilder::new(true)
                    .info(400, FILE_ATTRIBUTE_REPARSE_POINT)
                    .name(root, "Link", 3)
                    .build(),
            ),
            (
                24,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(23, 1), "inside.bin", 3)
                    .resident_data(9)
                    .build(),
            ),
            // A stale name: its parent record was reused (sequence 3, not 2).
            (
                25,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(20, 3), "ghost.txt", 3)
                    .resident_data(50)
                    .build(),
            ),
            // A deleted file.
            (
                26,
                RecordBuilder::new(false)
                    .unused()
                    .name(root, "deleted.txt", 3)
                    .resident_data(70)
                    .build(),
            ),
            (
                27,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(root, "empty.dat", 3)
                    .build(),
            ),
        ]);
        let index = index_of(img);
        let tree = build(&index, &ScanOptions::default());
        let names: Vec<&str> = tree
            .children(tree.root())
            .iter()
            .map(|c| tree.name(c))
            .collect();
        assert_eq!(names, vec!["big.bin", "Users", "Link", "empty.dat"]);
        assert_eq!(tree.root().files, 3);
        assert_eq!(tree.root().dirs, 2);
        assert_eq!(tree.root().size, 10_005);

        let big = child(&tree, &["big.bin"]).unwrap();
        assert_eq!(
            (big.size, big.disk_size, big.modified()),
            (10_000, 12_288, 300)
        );
        let a = child(&tree, &["Users", "a.txt"]).unwrap();
        // A resident file still rounds up to one cluster, like the walk.
        assert_eq!((a.size, a.disk_size, a.modified()), (5, 4096, 200));
        // A folder shows its newest entry's date, as with the walk.
        assert_eq!(child(&tree, &["Users"]).unwrap().modified(), 200);
        let link = child(&tree, &["Link"]).unwrap();
        assert!(tree.children(link).is_empty());
        assert_eq!(child(&tree, &["empty.dat"]).unwrap().disk_size, 0);
    }

    #[test]
    fn hard_links_are_counted_once() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(true)
                    .info(0, 0)
                    .name(root, "one", 3)
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(true)
                    .info(0, 0)
                    .name(root, "two", 3)
                    .build(),
            ),
            (
                22,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(20, 1), "original.bin", 3)
                    .name(fref(21, 1), "link.bin", 0)
                    .data("", 4096, 4096, 0, 0, 0, &[0x11, 0x01, 0x20, 0x00])
                    .build(),
            ),
        ]);
        let tree = build(&index_of(img), &ScanOptions::default());
        assert_eq!(tree.root().files, 2);
        assert_eq!(tree.root().size, 4096);
        let links = tree
            .children(tree.root())
            .iter()
            .flat_map(|d| tree.children(d))
            .filter(|n| n.hard_link())
            .count();
        assert_eq!(links, 1);
    }

    #[test]
    fn extension_records_are_merged_into_their_base() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(false)
                    .info(500, 0)
                    .name(root, "fragmented.vhdx", 3)
                    .build(),
            ),
            // The data stream (and a second name) moved to an extension
            // record; a later segment of the stream must not override it.
            (
                30,
                RecordBuilder::new(false)
                    .extension_of(fref(20, 1))
                    .data("", 1 << 30, 1 << 30, 0, 0, 0, &[0x11, 0x01, 0x20, 0x00])
                    .data("", 0, 0, 0, 0, 100, &[0x11, 0x01, 0x20, 0x00])
                    .name(root, "second name.vhdx", 1)
                    .build(),
            ),
        ]);
        let tree = build(&index_of(img), &ScanOptions::default());
        let f = child(&tree, &["fragmented.vhdx"]).unwrap();
        let g = child(&tree, &["second name.vhdx"]).unwrap();
        assert_eq!(tree.root().size, 1 << 30);
        assert_eq!(f.size + g.size, 1 << 30);
        assert_eq!(f.modified(), 500);
    }

    #[test]
    fn compressed_sparse_and_wof_files_report_their_real_allocation() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(false)
                    .info(0, FILE_ATTRIBUTE_COMPRESSED)
                    .name(root, "compressed.log", 3)
                    .data(
                        "",
                        100_000,
                        131_072,
                        16_384,
                        ATTR_FLAG_COMPRESSED,
                        0,
                        &[0x11, 0x04, 0x20, 0x00],
                    )
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(false)
                    .info(0, FILE_ATTRIBUTE_REPARSE_POINT)
                    .name(root, "compact.exe", 3)
                    .data(
                        "",
                        50_000,
                        53_248,
                        0,
                        ATTR_FLAG_SPARSE,
                        0,
                        &[0x01, 0x0D, 0x00],
                    )
                    .data(
                        WOF_STREAM,
                        20_000,
                        20_480,
                        20_480,
                        0,
                        0,
                        &[0x11, 0x05, 0x20, 0x00],
                    )
                    .build(),
            ),
        ]);
        let tree = build(&index_of(img), &ScanOptions::default());
        assert_eq!(child(&tree, &["compressed.log"]).unwrap().disk_size, 16_384);
        let wof = child(&tree, &["compact.exe"]).unwrap();
        assert_eq!((wof.size, wof.disk_size), (50_000, 20_480));
    }

    #[test]
    fn scan_options_filter_the_tree() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(true)
                    .info(0, 0)
                    .name(root, "node_modules", 3)
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(20, 1), "dep.js", 3)
                    .resident_data(10)
                    .build(),
            ),
            (
                22,
                RecordBuilder::new(false)
                    .info(0, FILE_ATTRIBUTE_HIDDEN)
                    .name(root, "secret.txt", 3)
                    .resident_data(3)
                    .build(),
            ),
            (
                23,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(root, "keep.txt", 3)
                    .resident_data(4)
                    .build(),
            ),
        ]);
        let index = index_of(img);
        let options = ScanOptions {
            excluded_globs: vec!["node_modules".into()],
            skip_hidden_system: true,
            ..ScanOptions::default()
        };
        let tree = build(&index, &options);
        assert_eq!(tree.root().files, 1);
        assert_eq!(tree.root().size, 4);
        assert!(child(&tree, &["keep.txt"]).is_some());

        let all = build(&index, &ScanOptions::default());
        assert_eq!(all.root().files, 3);
    }

    #[test]
    fn excluded_dirs_are_skipped_by_full_path() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(true)
                    .info(0, 0)
                    .name(root, "Windows", 3)
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(20, 1), "big.dll", 3)
                    .resident_data(64)
                    .build(),
            ),
        ]);
        let index = index_of(img);
        let ctx_root = PathBuf::from(r"C:\");
        let mut ctx = Build {
            index: &index,
            cluster: 4096,
            progress: &Progress::default(),
            options: &ScanOptions::default(),
            links: LinkTracker::default(),
            excluded_dirs: Vec::new(),
        };
        ctx.excluded_dirs = ExcludedDir::new(&ctx_root.join("Windows"))
            .into_iter()
            .collect();
        let arena = Mutex::new(Arena::default());
        let node = build_dir(&ctx, &arena, ROOT_RECORD, &ctx_root, 0, 0);
        assert_eq!(node.files, 0);
        assert_eq!(node.dirs, 0);
    }

    #[test]
    fn a_subfolder_can_be_the_scan_root() {
        let root = fref(ROOT_RECORD, 5);
        let img = image(vec![
            (ROOT_RECORD, root_record()),
            (
                20,
                RecordBuilder::new(true)
                    .info(0, 0)
                    .name(root, "Users", 3)
                    .build(),
            ),
            (
                21,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(fref(20, 1), "a.txt", 3)
                    .resident_data(5)
                    .build(),
            ),
            (
                22,
                RecordBuilder::new(false)
                    .info(0, 0)
                    .name(root, "outside.txt", 3)
                    .resident_data(50)
                    .build(),
            ),
        ]);
        let index = index_of(img);
        assert!(index.is_dir(20));
        assert!(!index.is_dir(21));
        let sub = build_subtree(
            &index,
            20,
            Path::new(r"C:\Users"),
            r"C:\Users",
            4096,
            &Progress::default(),
            &ScanOptions::default(),
        );
        assert_eq!(sub.root().files, 1);
        assert_eq!(sub.root().size, 5);
    }

    #[test]
    fn mft_layout_reads_runs_and_the_attribute_list() {
        // Attribute list entry: $DATA (unnamed) held in record 40.
        let mut entry = vec![0u8; 0x20];
        entry[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        entry[4..6].copy_from_slice(&0x20u16.to_le_bytes());
        entry[0x10..0x18].copy_from_slice(&fref(40, 1).to_le_bytes());
        let mut base_entry = entry.clone();
        base_entry[0x10..0x18].copy_from_slice(&fref(0, 1).to_le_bytes());
        let list: Vec<u8> = [base_entry, entry].concat();

        let mut rec0 = RecordBuilder::new(false)
            .attribute_list(&list)
            .data(
                "",
                64 * REC as u64,
                0,
                0,
                0,
                0,
                &[0x21, 0x08, 0x00, 0x01, 0x00],
            )
            .build();
        assert!(apply_fixup(&mut rec0));
        let mut layout = MftLayout::default();
        mft_layout(&rec0, &mut layout);
        assert_eq!(layout.size, 64 * REC as u64);
        let Some(AttrList::Resident(list)) = &layout.list else {
            panic!("resident attribute list");
        };
        assert_eq!(data_records_in_list(list, 0), vec![40]);

        let mut rec40 = RecordBuilder::new(false)
            .extension_of(0)
            .data("", 0, 0, 0, 0, 8, &[0x21, 0x08, 0x40, 0x01, 0x00])
            .build();
        assert!(apply_fixup(&mut rec40));
        mft_layout(&rec40, &mut layout);
        let runs = merged_runs(&mut layout.segments);
        assert_eq!(
            runs,
            vec![
                Run {
                    lcn: Some(256),
                    clusters: 8
                },
                Run {
                    lcn: Some(320),
                    clusters: 8
                },
            ]
        );
    }
}

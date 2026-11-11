//! Export a scan (or a subtree of it) to CSV or JSON, for reports and for
//! comparing two scans.
//!
//! This is deliberately GUI-free: it walks [`Tree`] through its public API,
//! so it can be tested and reused without a toolkit.
//!
//! - CSV is a flat table with one row per node:
//!   `path,size,disk_size,items,files,folders,type,modified,modified_iso`, plus
//!   an `owner` column when the scan collected owners.
//! - JSON is a hierarchical snapshot of the subtree that
//!   [`import_json`] can read back into a [`Tree`] (sizes, structure,
//!   timestamps, hard-link markers and drive pseudo-item kinds round-trip;
//!   the UI-only expanded state does not).
//! - The versioned binary form lives in [`crate::scanfile`]: smaller and
//!   faster to load for very large scans, and it also keeps hard-link
//!   markers and owners.
//!
//! Every format honours [`ExportOptions`]: `max_depth` limits how far below
//! the exported root rows are emitted (the root itself is depth 0), and
//! `include_files == false` exports folders only.

use crate::filter::{Filter, FilterView};
use crate::model::{
    Arena, DirBuilder, NO_OWNER, Node, NodeKind, NodePath, OwnerTable, Subtree, Tree,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// What to export and how much of it.
#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// Deepest depth below the exported root to include (`None` = no limit).
    /// The exported root itself is depth 0.
    pub max_depth: Option<usize>,
    /// Emit file rows. When false, only folders are exported.
    pub include_files: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            include_files: true,
        }
    }
}

/// Export file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    /// Versioned binary scan (see [`crate::scanfile`]).
    Binary,
}

impl ExportFormat {
    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Binary => crate::scanfile::EXTENSION,
        }
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Dir => "dir",
        NodeKind::File => "file",
        NodeKind::FreeSpace => "free_space",
        NodeKind::Unknown => "unknown",
    }
}

fn parse_kind(raw: &str) -> Option<NodeKind> {
    Some(match raw {
        "dir" => NodeKind::Dir,
        "file" => NodeKind::File,
        "free_space" => NodeKind::FreeSpace,
        "unknown" => NodeKind::Unknown,
        _ => return None,
    })
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// `modified` as an ISO-8601 UTC timestamp (`""` when unknown).
fn iso_time(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default()
}

/// Quote one CSV field (RFC 4180).
fn push_field(out: &mut String, field: &str) {
    if field.contains(['"', ',', '\n', '\r']) {
        out.push('"');
        out.push_str(&field.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(field);
    }
}

/// One exported row: the node's absolute index path and its file-system path.
struct Row {
    path: NodePath,
    fs_path: PathBuf,
}

/// Pre-order walk of the subtree at `root`, honouring `options` and, when
/// given, `view` (which keeps matches plus their ancestors).
/// Children keep the tree's order (largest first).
fn collect_rows(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    view: Option<&FilterView<'_>>,
) -> io::Result<Vec<Row>> {
    if tree.get(root).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "export root not found",
        ));
    }
    let base: PathBuf = tree.fs_path(root).unwrap_or_else(|| tree.root_path.clone());
    let mut rows = Vec::new();
    // Stack of (index path, depth, fs path); children are pushed in reverse
    // so they pop largest-first.
    let mut stack = vec![(root.to_vec(), 0usize, base)];
    while let Some((path, depth, fs_path)) = stack.pop() {
        let Some(node) = tree.get(&path) else {
            continue;
        };
        let node = *node;
        let is_dir = node.is_dir();
        if is_dir || options.include_files {
            rows.push(Row {
                path: path.clone(),
                fs_path: fs_path.clone(),
            });
        }
        if is_dir && options.max_depth.is_none_or(|max| depth < max) {
            let children = tree.children(tree.get(&path).expect("path still valid"));
            for i in (0..children.len()).rev() {
                let child = &children[i];
                if view.is_some_and(|view| !view.shows(tree, child)) {
                    continue;
                }
                let mut child_path = path.clone();
                child_path.push(i as u32);
                let mut child_fs = fs_path.clone();
                child_fs.push(tree.name(child));
                stack.push((child_path, depth + 1, child_fs));
            }
        }
    }
    Ok(rows)
}

/// Write the subtree at `root` as CSV. Returns the number of data rows
/// (excluding the header).
pub fn export_csv(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    out: impl Write,
) -> io::Result<usize> {
    export_csv_filtered(tree, root, options, None, out)
}

/// Like [`export_csv`], but keeps only entries that match `filter` plus their
/// ancestors. The exported root row is always written.
pub fn export_csv_filtered(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    filter: Option<&Filter>,
    mut out: impl Write,
) -> io::Result<usize> {
    let view = filter.map(|filter| FilterView::new(tree, filter));
    let rows = collect_rows(tree, root, options, view.as_ref())?;
    // The owner column only exists for scans that collected owners, so the
    // default export keeps its existing shape.
    let with_owner = tree.has_owners();
    let mut buf =
        String::from("path,size,disk_size,items,files,folders,type,modified,modified_iso");
    if with_owner {
        buf.push_str(",owner");
    }
    buf.push('\n');
    for row in &rows {
        let node = *tree
            .get(&row.path)
            .ok_or_else(|| invalid("tree changed during export"))?;
        push_field(&mut buf, &row.fs_path.to_string_lossy());
        buf.push(',');
        buf.push_str(&format!(
            "{},{},{},{},{},{},{},{}",
            node.size,
            node.disk_size,
            node.items(),
            node.files,
            node.dirs,
            kind_label(node.kind()),
            node.modified(),
            iso_time(node.modified()),
        ));
        if with_owner {
            buf.push(',');
            push_field(
                &mut buf,
                tree.owner_name(tree.owner_at(&row.path)).unwrap_or(""),
            );
        }
        buf.push('\n');
    }
    out.write_all(buf.as_bytes())?;
    Ok(rows.len())
}

/// A JSON snapshot of one node and everything below it.
///
/// The field aliases let [`import_json_with_meta`] read both this crate's
/// export (`kind`, `dirs`, numeric `modified`) and the `fastdir.scan`
/// documents the CLI writes (`type`, `folders`, ISO-8601 `modified`).
#[derive(Serialize, Deserialize)]
struct JsonNode {
    name: String,
    #[serde(alias = "type")]
    kind: String,
    size: u64,
    disk_size: u64,
    #[serde(default)]
    files: u32,
    #[serde(default, alias = "folders")]
    dirs: u32,
    #[serde(default, deserialize_with = "de_modified")]
    modified: i64,
    /// Another hard link to a file counted elsewhere. Omitted when false so
    /// ordinary exports stay unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    hard_link: bool,
    /// Display name of the owning account, when the scan collected owners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    children: Vec<JsonNode>,
}

/// A JSON snapshot of an exported subtree.
#[derive(Serialize, Deserialize)]
struct JsonDoc {
    version: u32,
    root_path: String,
    exported_at: i64,
    root: JsonNode,
}

/// `root` is an object in this crate's export and the scanned path in the
/// CLI's `fastdir.scan` document.
#[derive(Deserialize)]
#[serde(untagged)]
enum DocRoot {
    Path(String),
    Node(JsonNode),
}

/// Everything the reader accepts: either [`JsonDoc`] or a CLI `fastdir.scan`
/// document (which carries the tree under `tree`).
#[derive(Deserialize)]
struct ImportDoc {
    version: u32,
    #[serde(default)]
    root_path: Option<String>,
    #[serde(default)]
    exported_at: Option<i64>,
    #[serde(default)]
    scanned_at: Option<String>,
    #[serde(default)]
    root: Option<DocRoot>,
    #[serde(default)]
    tree: Option<JsonNode>,
}

/// Read a timestamp that is either Unix seconds (this crate's export) or an
/// ISO-8601 UTC string (the CLI's documents, or `null`).
fn de_modified<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    use serde::Deserialize as _;
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match value {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => parse_iso_secs(&s).unwrap_or(0),
        _ => 0,
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn parse_iso_secs(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.timestamp())
}

const JSON_VERSION: u32 = 1;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn snapshot(
    tree: &Tree,
    path: &[u32],
    options: &ExportOptions,
    depth: usize,
) -> io::Result<JsonNode> {
    let node = *tree
        .get(path)
        .ok_or_else(|| invalid("tree changed during export"))?;
    let name = tree
        .get(path)
        .map(|n| tree.name(n).to_owned())
        .unwrap_or_default();
    let mut children = Vec::new();
    if node.is_dir() && options.max_depth.is_none_or(|max| depth < max) {
        let len = tree.get(path).map(|n| tree.children(n).len()).unwrap_or(0);
        for i in 0..len {
            let Some(child) = tree.get(path).and_then(|n| tree.children(n).get(i)) else {
                continue;
            };
            let child = *child;
            if !child.is_dir() && !options.include_files {
                continue;
            }
            let mut child_path = path.to_vec();
            child_path.push(i as u32);
            children.push(snapshot(tree, &child_path, options, depth + 1)?);
        }
    }
    Ok(JsonNode {
        name,
        kind: kind_label(node.kind()).to_string(),
        size: node.size,
        disk_size: node.disk_size,
        files: node.files,
        dirs: node.dirs,
        modified: node.modified(),
        hard_link: node.hard_link(),
        owner: tree.owner_name(tree.owner_at(path)).map(str::to_owned),
        children,
    })
}

/// Write the subtree at `root` as pretty-printed JSON. Returns the number of
/// nodes in the snapshot (including the root).
pub fn export_json(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    out: impl Write,
) -> io::Result<usize> {
    write_json(tree, root, options, now_unix(), true, out)
}

/// Like [`export_json`], but compact and stamped with `exported_at` (Unix
/// seconds) instead of the current time. Scan snapshots use this so the
/// document records when the scan was taken.
pub fn export_json_compact(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    exported_at: i64,
    out: impl Write,
) -> io::Result<usize> {
    write_json(tree, root, options, exported_at, false, out)
}

fn write_json(
    tree: &Tree,
    root: &[u32],
    options: &ExportOptions,
    exported_at: i64,
    pretty: bool,
    mut out: impl Write,
) -> io::Result<usize> {
    if tree.get(root).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "export root not found",
        ));
    }
    let snapshot = snapshot(tree, root, options, 0)?;
    let count = 1 + count_descendants(&snapshot);
    let doc = JsonDoc {
        version: JSON_VERSION,
        root_path: tree
            .fs_path(root)
            .unwrap_or_else(|| tree.root_path.clone())
            .to_string_lossy()
            .into_owned(),
        exported_at,
        root: snapshot,
    };
    if pretty {
        serde_json::to_writer_pretty(&mut out, &doc)
    } else {
        serde_json::to_writer(&mut out, &doc)
    }
    .map_err(|e| invalid(e.to_string()))?;
    out.flush()?;
    Ok(count)
}

fn count_descendants(node: &JsonNode) -> usize {
    node.children.iter().map(|c| 1 + count_descendants(c)).sum()
}

/// Write the subtree at `root` to `dest` in `format`. Returns the number of
/// exported rows (CSV) or nodes (JSON / binary).
pub fn export_to_file(
    tree: &Tree,
    root: &[u32],
    format: ExportFormat,
    options: &ExportOptions,
    dest: &Path,
) -> io::Result<usize> {
    let file = std::fs::File::create(dest)?;
    let out = io::BufWriter::new(file);
    match format {
        ExportFormat::Csv => export_csv(tree, root, options, out),
        ExportFormat::Json => export_json(tree, root, options, out),
        ExportFormat::Binary => {
            crate::scanfile::export_binary(tree, root, options, now_unix(), out)
        }
    }
}

/// Build one directory (and its children) from a JSON node, interning the
/// optional owner names into `owners`. Returns the node and its own owner.
fn build_dir(
    node: &JsonNode,
    arena: &mut Arena,
    owners: &mut OwnerTable,
) -> io::Result<(Node, u16)> {
    if node.name.is_empty() {
        return Err(invalid("node without a name"));
    }
    let mut builder = DirBuilder::new(node.modified);
    for child in &node.children {
        if child.name.is_empty() {
            return Err(invalid("node without a name"));
        }
        let owner = child
            .owner
            .as_deref()
            .map(|name| owners.intern(name))
            .unwrap_or(NO_OWNER);
        match parse_kind(&child.kind).ok_or_else(|| invalid("unknown node kind"))? {
            NodeKind::Dir => {
                let (dir, dir_owner) = build_dir(child, arena, owners)?;
                builder.add_dir_owned(&child.name, dir, dir_owner);
            }
            NodeKind::File if child.hard_link => {
                builder.add_hard_link_owned(&child.name, child.modified, owner)
            }
            NodeKind::File => builder.add_file_owned(
                &child.name,
                child.size,
                child.disk_size,
                child.modified,
                owner,
            ),
            kind @ (NodeKind::FreeSpace | NodeKind::Unknown) => {
                builder.add_special(
                    kind,
                    &child.name,
                    child.size,
                    child.disk_size,
                    child.modified,
                );
            }
        }
    }
    let own = node
        .owner
        .as_deref()
        .map(|name| owners.intern(name))
        .unwrap_or(NO_OWNER);
    Ok((builder.commit(arena), own))
}

/// A scan read back from JSON, plus when it was taken.
pub struct ImportedScan {
    pub tree: Tree,
    /// Unix seconds. The GUI's export stores the export time; a CLI
    /// `fastdir.scan` document stores the scan time. `0` when unknown.
    pub scanned_at: i64,
}

/// Read back a [`export_json`] snapshot or a CLI `fastdir.scan` document into
/// a [`Tree`], with the scan/export timestamp.
///
/// Structure, sizes, timestamps, hard-link markers and drive pseudo-item
/// kinds round trip; extension stats are rebuilt from the imported names.
/// A `hard_link` marker adds no bytes, exactly as in the scanned tree, and
/// older documents without the field keep importing as plain files.
pub fn import_json_with_meta(source: impl Read) -> io::Result<ImportedScan> {
    let doc: ImportDoc =
        serde_json::from_reader(source).map_err(|e| invalid(format!("invalid export: {e}")))?;
    if doc.version != JSON_VERSION {
        return Err(invalid(format!(
            "unsupported export version {}",
            doc.version
        )));
    }
    // This crate's export: `root_path` + a `root` object. The CLI's
    // `fastdir.scan`: a `root` path string + the tree under `tree`.
    let (root, root_path, scanned_at) = match (doc.root, doc.tree) {
        (Some(DocRoot::Node(root)), _) => {
            let path = doc
                .root_path
                .ok_or_else(|| invalid("export without a root path"))?;
            (root, path, doc.exported_at.unwrap_or(0))
        }
        (Some(DocRoot::Path(path)), Some(root)) => (
            root,
            path,
            doc.scanned_at
                .as_deref()
                .and_then(parse_iso_secs)
                .unwrap_or(0),
        ),
        _ => return Err(invalid("export without a tree")),
    };
    if root.name.is_empty() {
        return Err(invalid("export without a root name"));
    }
    let root_name = root.name.clone();
    let root_path = PathBuf::from(root_path);
    let mut arena = Arena::default();
    let kind = parse_kind(&root.kind).ok_or_else(|| invalid("unknown node kind"))?;
    if kind != NodeKind::Dir {
        return Err(invalid("export root must be a folder"));
    }
    let mut owners = OwnerTable::default();
    let (root, root_owner) = build_dir(&root, &mut arena, &mut owners)?;
    arena.set_owners(owners);
    Ok(ImportedScan {
        tree: Tree::new(
            Subtree::new_owned(arena, &root_name, root, root_owner),
            root_path,
        ),
        scanned_at,
    })
}

/// Read back a [`export_json`] snapshot into a [`Tree`].
///
/// Structure, sizes and timestamps round trip; extension stats are rebuilt
/// from the imported names.
pub fn import_json(source: impl Read) -> io::Result<Tree> {
    import_json_with_meta(source).map(|scan| scan.tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// root / { sub / { a.txt 5, b.bin 15 }, c.txt 30 }
    fn sample_tree() -> Tree {
        let mut arena = Arena::default();
        let mut sub = DirBuilder::new(1_700_000_000);
        sub.add_file("a.txt", 5, 4096, 1_700_000_100);
        sub.add_file("b.bin", 15, 15, 1_700_000_200);
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(1_700_000_300);
        root.add_dir("sub", sub);
        root.add_file("c.txt", 30, 30, 1_700_000_400);
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn header() -> &'static str {
        "path,size,disk_size,items,files,folders,type,modified,modified_iso"
    }

    #[test]
    fn csv_has_header_and_one_row_per_node() {
        let tree = sample_tree();
        let mut out = Vec::new();
        let rows = export_csv(&tree, &[], &ExportOptions::default(), &mut out).unwrap();
        assert_eq!(rows, 5);
        let text = String::from_utf8(out).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next().unwrap(), header());
        assert_eq!(lines.count(), 5);
        // Spot-check a file row: sizes, counts, kind and both timestamps.
        let file_line = text.lines().find(|l| l.contains("a.txt")).unwrap();
        let cells: Vec<&str> = file_line.split(',').collect();
        assert_eq!(cells[1], "5"); // size
        assert_eq!(cells[2], "4096"); // disk_size
        assert_eq!(cells[3], "1"); // items
        assert_eq!(cells[6], "file"); // type
        assert_eq!(cells[7], "1700000100"); // modified
        assert_eq!(cells[8], "2023-11-14T22:15:00Z"); // modified_iso
        // A folder row aggregates its children.
        let sub_line = text
            .lines()
            .find(|l| l.contains("sub") && l.contains("dir"))
            .unwrap();
        let cells: Vec<&str> = sub_line.split(',').collect();
        assert_eq!(cells[1], "20");
        assert_eq!(cells[4], "2");
    }

    #[test]
    fn csv_can_filter_matches_and_ancestors() {
        let tree = sample_tree();
        let filter = Filter::parse("*.bin", "", "", "", "").unwrap();
        let mut out = Vec::new();
        let rows = export_csv_filtered(
            &tree,
            &[],
            &ExportOptions::default(),
            Some(&filter),
            &mut out,
        )
        .unwrap();
        assert_eq!(rows, 3); // root, sub and b.bin
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("b.bin"));
        assert!(text.contains("sub"));
        assert!(!text.contains("a.txt"));
        assert!(!text.contains("c.txt"));
    }

    #[test]
    fn csv_quotes_paths_with_commas() {
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file("weird,name.txt", 1, 1, 0);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));
        let mut out = Vec::new();
        export_csv(&tree, &[], &ExportOptions::default(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let line = text.lines().find(|l| l.contains("weird,name.txt")).unwrap();
        // The comma forces the whole path field to be quoted.
        assert!(line.starts_with('"') && line.contains("weird,name.txt\""));
    }

    #[test]
    fn csv_respects_depth_limit() {
        let tree = sample_tree();
        let options = ExportOptions {
            max_depth: Some(1),
            include_files: true,
        };
        let mut out = Vec::new();
        let rows = export_csv(&tree, &[], &options, &mut out).unwrap();
        // root + sub + c.txt; a.txt and b.bin are one level too deep.
        assert_eq!(rows, 3);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("c.txt"));
        assert!(!text.contains("a.txt"));
        assert!(!text.contains("b.bin"));
    }

    #[test]
    fn csv_depth_zero_exports_only_the_root() {
        let tree = sample_tree();
        let options = ExportOptions {
            max_depth: Some(0),
            include_files: true,
        };
        let mut out = Vec::new();
        let rows = export_csv(&tree, &[], &options, &mut out).unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn csv_can_exclude_files() {
        let tree = sample_tree();
        let options = ExportOptions {
            max_depth: None,
            include_files: false,
        };
        let mut out = Vec::new();
        let rows = export_csv(&tree, &[], &options, &mut out).unwrap();
        assert_eq!(rows, 2); // root + sub
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains(".txt"));
        assert!(!text.contains(".bin"));
    }

    #[test]
    fn csv_can_export_a_subtree() {
        let tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let mut out = Vec::new();
        let rows = export_csv(&tree, &sub, &ExportOptions::default(), &mut out).unwrap();
        assert_eq!(rows, 3);
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("c.txt"));
    }

    #[test]
    fn export_rejects_an_unknown_root() {
        let tree = sample_tree();
        let mut out = Vec::new();
        let csv = export_csv(&tree, &[99], &ExportOptions::default(), &mut out);
        assert_eq!(csv.unwrap_err().kind(), io::ErrorKind::NotFound);
        let json = export_json(&tree, &[99], &ExportOptions::default(), &mut out);
        assert_eq!(json.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn json_round_trips_the_subtree() {
        let tree = sample_tree();
        let mut buf = Vec::new();
        let nodes = export_json(&tree, &[], &ExportOptions::default(), &mut buf).unwrap();
        assert_eq!(nodes, 5);
        // The snapshot parses as JSON and carries the root path.
        let doc: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["root_path"], "/root");
        assert_eq!(doc["root"]["children"].as_array().unwrap().len(), 2);

        let back = import_json(buf.as_slice()).unwrap();
        assert_eq!(back.root_path, PathBuf::from("/root"));
        assert_eq!(back.root().size, tree.root().size);
        assert_eq!(back.root().files, tree.root().files);
        assert_eq!(back.root().dirs, tree.root().dirs);
        let a = back.find(Path::new("/root/sub/a.txt")).unwrap();
        let node = back.get(&a).unwrap();
        assert_eq!((node.size, node.disk_size), (5, 4096));
        assert_eq!(node.modified(), 1_700_000_100);
        assert_eq!(back.name(node), "a.txt");
        // Extension stats are rebuilt on import.
        assert!(back.exts.iter().any(|e| &*e.name == ".txt"));
    }

    #[test]
    fn json_round_trips_drive_pseudo_items() {
        let mut tree = sample_tree();
        tree.attach_volume_info(100, 40);
        assert!(tree.is_drive_scan());
        let mut buf = Vec::new();
        export_json(&tree, &[], &ExportOptions::default(), &mut buf).unwrap();
        let back = import_json(buf.as_slice()).unwrap();
        assert!(back.is_drive_scan(), "free space and unknown kept their kind");
        assert_eq!(back.free_bytes(), Some(40));
        assert_eq!(back.unknown_bytes(), Some(10));
        assert_eq!(back.scanned_bytes(), 50);
        // They are still not real files for the item count.
        assert_eq!(back.children(back.root()).len(), 4); // free, unknown, c.txt, sub
    }

    #[test]
    fn json_round_trips_ntfs_metadata() {
        let mut tree = sample_tree(); // scanned 50
        tree.attach_volume_breakdown(
            100,
            40,
            crate::model::UnknownBreakdown {
                ntfs_metadata: 4,
                shadow_copies: 3,
            },
        );
        let mut buf = Vec::new();
        export_json(&tree, &[], &ExportOptions::default(), &mut buf).unwrap();
        let back = import_json(buf.as_slice()).unwrap();
        assert_eq!(back.ntfs_metadata_bytes(), Some(4));
        assert_eq!(back.shadow_copy_bytes(), Some(3));
        assert_eq!(back.unknown_bytes(), Some(3));
        assert_eq!(back.used_bytes(), 60);
        assert_eq!(back.scanned_bytes(), 50);
    }

    #[test]
    fn json_round_trips_a_subtree_with_options() {
        let tree = sample_tree();
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        let options = ExportOptions {
            max_depth: None,
            include_files: false,
        };
        let mut buf = Vec::new();
        export_json(&tree, &sub, &options, &mut buf).unwrap();
        let back = import_json(buf.as_slice()).unwrap();
        // Files were excluded, so the imported folder is empty.
        assert_eq!(back.root().files, 0);
        assert_eq!(back.root().size, 0);
    }

    #[test]
    fn json_round_trips_hard_link_markers() {
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file("a.bin", 10, 10, 1);
        root.add_hard_link("b.bin", 2);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));

        let mut buf = Vec::new();
        export_json(&tree, &[], &ExportOptions::default(), &mut buf).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(doc["root"]["children"][1]["hard_link"], true);
        // The field is omitted on ordinary nodes.
        assert!(doc["root"].get("hard_link").is_none());

        let back = import_json(buf.as_slice()).unwrap();
        let b = back.find(Path::new("/root/b.bin")).unwrap();
        let node = back.get(&b).unwrap();
        assert!(node.hard_link());
        assert_eq!((node.size, node.disk_size), (0, 0));
        assert_eq!(back.root().files, 2);
        assert_eq!(back.root().size, 10);
    }

    #[test]
    fn import_rejects_garbage() {
        assert!(import_json(b"not json".as_slice()).is_err());
        let wrong_version =
            br#"{"version":99,"root_path":"/x","exported_at":0,"root":{"name":"x","kind":"dir","size":0,"disk_size":0,"files":0,"dirs":0,"modified":0}}"#;
        assert!(import_json(wrong_version.as_slice()).is_err());
    }

    #[test]
    fn import_reads_a_cli_scan_document() {
        // The shape `fastdir-cli scan` writes: `root` is a path, the tree is
        // under `tree`, kinds are `type`, folders are `folders`, and
        // timestamps are ISO-8601 (or null).
        let doc = br#"{
            "schema": "fastdir.scan",
            "version": 1,
            "fastdir_version": "0.2.0",
            "root": "/root",
            "scanned_at": "2023-11-14T22:15:00Z",
            "elapsed_ms": 12,
            "errors": 0,
            "tree": {
                "name": "root", "path": "/root", "type": "dir",
                "size": 50, "disk_size": 50, "items": 3,
                "files": 3, "folders": 1,
                "modified": "2023-11-14T22:20:00Z",
                "error": false, "hard_link": false, "truncated": false,
                "children": [
                    {"name": "sub", "path": "/root/sub", "type": "dir",
                     "size": 20, "disk_size": 20, "items": 2,
                     "files": 2, "folders": 0,
                     "modified": null, "error": false, "hard_link": false,
                     "truncated": false,
                     "children": [
                        {"name": "a.txt", "path": "/root/sub/a.txt", "type": "file",
                         "size": 5, "disk_size": 4096, "items": 1,
                         "files": 1, "folders": 0,
                         "modified": "2023-11-14T22:15:00Z",
                         "error": false, "hard_link": false},
                        {"name": "b.bin", "path": "/root/sub/b.bin", "type": "file",
                         "size": 15, "disk_size": 15, "items": 1,
                         "files": 1, "folders": 0,
                         "modified": "2023-11-14T22:16:00Z",
                         "error": false, "hard_link": false}
                     ]},
                    {"name": "c.txt", "path": "/root/c.txt", "type": "file",
                     "size": 30, "disk_size": 30, "items": 1,
                     "files": 1, "folders": 0,
                     "modified": "2023-11-14T22:17:00Z",
                     "error": false, "hard_link": false}
                ]
            }
        }"#;
        let loaded = import_json_with_meta(doc.as_slice()).unwrap();
        assert_eq!(loaded.scanned_at, 1_700_000_100);
        let tree = loaded.tree;
        assert_eq!(tree.root_path, PathBuf::from("/root"));
        assert_eq!(tree.root().size, 50);
        assert_eq!(tree.root().files, 3);
        assert_eq!(tree.root().dirs, 1);
        let a = tree.find(Path::new("/root/sub/a.txt")).unwrap();
        assert_eq!(tree.get(&a).unwrap().size, 5);
        assert_eq!(tree.get(&a).unwrap().modified(), 1_700_000_100);
        // A null CLI timestamp becomes "unknown".
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!(tree.get(&sub).unwrap().modified(), 1_700_000_160);
        assert!(tree.exts.iter().any(|e| &*e.name == ".txt"));
    }

    #[test]
    fn exports_include_the_owner_when_collected() {
        let mut table = crate::model::OwnerTable::default();
        let alice = table.intern(r"DOMAIN\alice");
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file_owned("a.txt", 5, 5, 0, alice);
        let root = root.commit(&mut arena);
        arena.set_owners(table);
        let tree = Tree::new(
            Subtree::new_owned(arena, "root", root, alice),
            PathBuf::from("/root"),
        );

        let mut csv = Vec::new();
        export_csv(&tree, &[], &ExportOptions::default(), &mut csv).unwrap();
        let text = String::from_utf8(csv).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next().unwrap(), format!("{},owner", header()));
        let row = lines.find(|line| line.contains("a.txt")).unwrap();
        assert!(row.ends_with(r#"DOMAIN\alice"#), "{row}");

        let mut json = Vec::new();
        export_json(&tree, &[], &ExportOptions::default(), &mut json).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(doc["root"]["owner"], r"DOMAIN\alice");
        assert_eq!(doc["root"]["children"][0]["owner"], r"DOMAIN\alice");

        // The owner round-trips through the JSON document.
        let back = import_json(json.as_slice()).unwrap();
        assert!(back.has_owners());
        let a = back.find(Path::new("/root/a.txt")).unwrap();
        assert_eq!(back.owner_name(back.owner_at(&a)), Some(r"DOMAIN\alice"));
        assert_eq!(back.owner_name(back.root_owner()), Some(r"DOMAIN\alice"));
    }

    #[test]
    fn import_rejects_a_document_without_a_tree() {
        let no_tree = br#"{"schema":"fastdir.scan","version":1,"root":"/x"}"#;
        assert!(import_json(no_tree.as_slice()).is_err());
        let no_path =
            br#"{"version":1,"root":{"name":"x","kind":"dir","size":0,"disk_size":0,"files":0,"dirs":0,"modified":0}}"#;
        assert!(import_json(no_path.as_slice()).is_err());
    }
}

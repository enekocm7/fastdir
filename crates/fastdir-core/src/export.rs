//! Export a scan (or a subtree of it) to CSV or JSON, for reports and for
//! comparing two scans.
//!
//! This is deliberately GUI-free: it walks [`Tree`] through its public API,
//! so it can be tested and reused without a toolkit.
//!
//! - CSV is a flat table with one row per node:
//!   `path,size,disk_size,items,files,folders,type,modified,modified_iso`.
//! - JSON is a hierarchical snapshot of the subtree that
//!   [`import_json`] can read back into a [`Tree`] (sizes, structure and
//!   timestamps round-trip; UI-only flags such as expanded state, hard-link
//!   markers and drive pseudo-item kinds do not).
//!
//! Both formats honour [`ExportOptions`]: `max_depth` limits how far below
//! the exported root rows are emitted (the root itself is depth 0), and
//! `include_files == false` exports folders only.

use crate::model::{Arena, DirBuilder, Node, NodeKind, NodePath, Subtree, Tree};
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
}

impl ExportFormat {
    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
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

/// Pre-order walk of the subtree at `root`, honouring `options`.
/// Children keep the tree's order (largest first).
fn collect_rows(tree: &Tree, root: &[u32], options: &ExportOptions) -> io::Result<Vec<Row>> {
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
    mut out: impl Write,
) -> io::Result<usize> {
    let rows = collect_rows(tree, root, options)?;
    let mut buf =
        String::from("path,size,disk_size,items,files,folders,type,modified,modified_iso\n");
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
        buf.push('\n');
    }
    out.write_all(buf.as_bytes())?;
    Ok(rows.len())
}

/// A JSON snapshot of one node and everything below it.
#[derive(Serialize, Deserialize)]
struct JsonNode {
    name: String,
    kind: String,
    size: u64,
    disk_size: u64,
    files: u32,
    dirs: u32,
    modified: i64,
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
        exported_at: now_unix(),
        root: snapshot,
    };
    serde_json::to_writer_pretty(out, &doc).map_err(|e| invalid(e.to_string()))?;
    Ok(count)
}

fn count_descendants(node: &JsonNode) -> usize {
    node.children.iter().map(|c| 1 + count_descendants(c)).sum()
}

/// Write the subtree at `root` to `dest` in `format`. Returns the number of
/// exported rows (CSV) or nodes (JSON).
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
    }
}

fn build_dir(node: &JsonNode, arena: &mut Arena) -> io::Result<Node> {
    if node.name.is_empty() {
        return Err(invalid("node without a name"));
    }
    let mut builder = DirBuilder::new(node.modified);
    for child in &node.children {
        if child.name.is_empty() {
            return Err(invalid("node without a name"));
        }
        match parse_kind(&child.kind).ok_or_else(|| invalid("unknown node kind"))? {
            NodeKind::Dir => {
                let dir = build_dir(child, arena)?;
                builder.add_dir(&child.name, dir);
            }
            // Drive pseudo-items and hard-link markers have no builder of
            // their own; keep their bytes as plain files so totals survive.
            _ => builder.add_file(&child.name, child.size, child.disk_size, child.modified),
        }
    }
    Ok(builder.commit(arena))
}

/// Read back a [`export_json`] snapshot into a [`Tree`].
///
/// Structure, sizes and timestamps round trip; extension stats are rebuilt
/// from the imported names.
pub fn import_json(source: impl Read) -> io::Result<Tree> {
    let doc: JsonDoc =
        serde_json::from_reader(source).map_err(|e| invalid(format!("invalid export: {e}")))?;
    if doc.version != JSON_VERSION {
        return Err(invalid(format!(
            "unsupported export version {}",
            doc.version
        )));
    }
    if doc.root.name.is_empty() {
        return Err(invalid("export without a root name"));
    }
    let root_path = PathBuf::from(doc.root_path);
    let mut arena = Arena::default();
    let kind = parse_kind(&doc.root.kind).ok_or_else(|| invalid("unknown node kind"))?;
    let root = if kind == NodeKind::Dir {
        build_dir(&doc.root, &mut arena)?
    } else {
        return Err(invalid("export root must be a folder"));
    };
    Ok(Tree::new(
        Subtree::new(arena, &doc.root.name, root),
        root_path,
    ))
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
    fn import_rejects_garbage() {
        assert!(import_json(b"not json".as_slice()).is_err());
        let wrong_version =
            br#"{"version":99,"root_path":"/x","exported_at":0,"root":{"name":"x","kind":"dir","size":0,"disk_size":0,"files":0,"dirs":0,"modified":0}}"#;
        assert!(import_json(wrong_version.as_slice()).is_err());
    }
}

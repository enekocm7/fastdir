//! `fastdir diff` documents: the path-based view of
//! [`fastdir_core::diff`] used by the CLI and the MCP `compare_scans` tool.
//!
//! The core diff stores changes keyed by new-tree node id and keeps removed
//! subtrees as single entries. This module turns that into a flat, sorted
//! list of rows with the same shape in JSON and CSV, so scripts and agents
//! can read one schema (`fastdir.diff`) regardless of the front end.

use fastdir_core::diff::{ChangeStatus, DiffTotals, ScanDiff};
use fastdir_core::model::{Node, NodePath, Tree};
use serde::Serialize;
use std::path::Path;

/// Versioned document names.
pub const DIFF_SCHEMA: &str = "fastdir.diff";
pub const SCHEMA_VERSION: u32 = 1;

/// One side of the comparison.
#[derive(Clone, Debug, Serialize)]
pub struct DiffSide {
    /// Root path, lossily converted so a non-Unicode path still serializes.
    pub root: String,
    /// ISO 8601 UTC, or `null` when the document did not record it.
    pub scanned_at: Option<String>,
}

impl DiffSide {
    pub fn new(root: impl Into<String>, scanned_at: Option<String>) -> Self {
        Self {
            root: root.into(),
            scanned_at,
        }
    }
}

/// The overall change, from [`ScanDiff::totals`].
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DiffTotal {
    pub old_size: u64,
    pub new_size: u64,
    /// `new_size - old_size` for the scanned bytes.
    pub delta: i64,
    /// Bytes in top-level added entries.
    pub added: u64,
    /// Bytes in top-level removed entries.
    pub removed: u64,
    pub added_items: u64,
    pub removed_items: u64,
    /// Change in free space when both scans carry drive volume data.
    pub free_delta: Option<i64>,
}

impl From<&DiffTotals> for DiffTotal {
    fn from(t: &DiffTotals) -> Self {
        Self {
            old_size: t.old_size,
            new_size: t.new_size,
            delta: t.net(),
            added: t.added,
            removed: t.removed,
            added_items: t.added_items,
            removed_items: t.removed_items,
            free_delta: t.free_delta,
        }
    }
}

/// One changed path.
#[derive(Clone, Debug, Serialize)]
pub struct DiffEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// `added`, `removed` or `changed`.
    pub status: &'static str,
    pub old_size: u64,
    pub new_size: u64,
    pub delta: i64,
    pub old_disk_size: u64,
    pub new_disk_size: u64,
}

/// `fastdir.diff` v1.
#[derive(Debug, Serialize)]
pub struct DiffDoc<'a> {
    pub schema: &'static str,
    pub version: u32,
    pub fastdir_version: &'static str,
    pub old: &'a DiffSide,
    pub new: &'a DiffSide,
    pub total: DiffTotal,
    /// Changes smaller than this were omitted.
    pub min_change: u64,
    /// Cap on `entries`, when one was given.
    pub limit: Option<usize>,
    pub entries: Vec<DiffEntry>,
}

impl<'a> DiffDoc<'a> {
    fn new(
        diff: &ScanDiff,
        old: &'a DiffSide,
        new: &'a DiffSide,
        min_change: u64,
        limit: Option<usize>,
    ) -> Self {
        Self {
            schema: DIFF_SCHEMA,
            version: SCHEMA_VERSION,
            fastdir_version: env!("CARGO_PKG_VERSION"),
            old,
            new,
            total: DiffTotal::from(diff.totals()),
            min_change,
            limit,
            entries: Vec::new(),
        }
    }
}

/// Build a document for `diff`, with `new` supplying paths and node kinds.
pub fn document<'a>(
    diff: &ScanDiff,
    new_tree: &Tree,
    old: &'a DiffSide,
    new: &'a DiffSide,
    min_change: u64,
    max_depth: Option<usize>,
    limit: Option<usize>,
) -> DiffDoc<'a> {
    let mut doc = DiffDoc::new(diff, old, new, min_change, limit);
    doc.entries = collect_entries(diff, new_tree, min_change, max_depth, limit);
    doc
}

/// Every changed path, largest absolute change first.
///
/// Added and removed subtrees contribute one row (their top-most node), so
/// their bytes are not repeated for every file below them. Changed folders
/// are listed alongside their changed children, matching the GUI's list.
pub fn collect_entries(
    diff: &ScanDiff,
    new_tree: &Tree,
    min_change: u64,
    max_depth: Option<usize>,
    limit: Option<usize>,
) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    let mut path = NodePath::new();
    visit(
        diff,
        new_tree,
        new_tree.root(),
        &mut path,
        0,
        min_change,
        max_depth,
        &mut entries,
    );
    for removed in diff.removed() {
        let depth = Path::new(&removed.path).components().count();
        if max_depth.is_some_and(|max| depth > max) || removed.size < min_change {
            continue;
        }
        entries.push(DiffEntry {
            path: diff
                .old_root()
                .join(&removed.path)
                .to_string_lossy()
                .into_owned(),
            kind: if removed.is_dir { "dir" } else { "file" },
            status: "removed",
            old_size: removed.size,
            new_size: 0,
            delta: -(removed.size as i64),
            old_disk_size: removed.disk_size,
            new_disk_size: 0,
        });
    }
    entries.sort_unstable_by(|a, b| {
        b.delta
            .unsigned_abs()
            .cmp(&a.delta.unsigned_abs())
            .then_with(|| {
                a.path
                    .to_lowercase()
                    .cmp(&b.path.to_lowercase())
                    .then_with(|| a.path.cmp(&b.path))
            })
    });
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    entries
}

#[allow(clippy::too_many_arguments)]
fn visit(
    diff: &ScanDiff,
    tree: &Tree,
    node: &Node,
    path: &mut NodePath,
    depth: usize,
    min_change: u64,
    max_depth: Option<usize>,
    out: &mut Vec<DiffEntry>,
) {
    // Drive pseudo-items are bookkeeping, not comparable data.
    if node.is_special() {
        return;
    }
    let change = diff.change(node);
    let added_here = change.is_some_and(|c| c.status == ChangeStatus::Added);
    if let Some(change) = change
        && change.delta.unsigned_abs() >= min_change
        && max_depth.is_none_or(|max| depth <= max)
    {
        out.push(DiffEntry {
            path: tree
                .fs_path(path)
                .unwrap_or_else(|| tree.root_path.clone())
                .to_string_lossy()
                .into_owned(),
            kind: if node.is_dir() { "dir" } else { "file" },
            status: if added_here { "added" } else { "changed" },
            old_size: change.old_size,
            new_size: change.new_size,
            delta: change.delta,
            old_disk_size: change.old_disk_size,
            new_disk_size: change.new_disk_size,
        });
    }
    // An added subtree is represented by its top-most row; do not repeat its
    // children. Stop at `max_depth` instead of walking further.
    if added_here || !node.is_dir() || max_depth.is_some_and(|max| depth >= max) {
        return;
    }
    for (ix, child) in tree.children(node).iter().enumerate() {
        path.push(ix as u32);
        visit(
            diff,
            tree,
            child,
            path,
            depth + 1,
            min_change,
            max_depth,
            out,
        );
        path.pop();
    }
}

/// `fastdir diff --format csv` output.
pub fn csv(entries: &[DiffEntry]) -> String {
    let mut out =
        String::from("path,type,status,old_size,new_size,delta,old_disk_size,new_disk_size\n");
    for e in entries {
        out.push_str(&crate::schema::csv_field(&e.path));
        out.push_str(&format!(
            ",{},{},{},{},{},{},{}\n",
            e.kind, e.status, e.old_size, e.new_size, e.delta, e.old_disk_size, e.new_disk_size,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastdir_core::model::{Arena, DirBuilder, Subtree};
    use std::path::PathBuf;

    enum Item {
        File(&'static str, u64),
        Dir(&'static str, Vec<Item>),
    }

    fn build_dir(arena: &mut Arena, items: &[Item]) -> Node {
        let mut builder = DirBuilder::new(1_700_000_000);
        for item in items {
            match item {
                Item::File(name, size) => builder.add_file(name, *size, *size, 1_700_000_000),
                Item::Dir(name, children) => {
                    let child = build_dir(arena, children);
                    builder.add_dir(name, child);
                }
            }
        }
        builder.commit(arena)
    }

    fn tree(root: &str, items: &[Item]) -> Tree {
        let mut arena = Arena::default();
        let root_node = build_dir(&mut arena, items);
        Tree::new(Subtree::new(arena, root, root_node), PathBuf::from(root))
    }

    fn sides() -> (DiffSide, DiffSide) {
        (
            DiffSide::new("old", Some("2026-01-01T00:00:00Z".into())),
            DiffSide::new("new", Some("2026-01-02T00:00:00Z".into())),
        )
    }

    #[test]
    fn entries_are_sorted_by_absolute_delta() {
        let old = tree(
            "old",
            &[
                Item::File("shrink.bin", 100),
                Item::File("grow.bin", 10),
                Item::File("same.bin", 5),
            ],
        );
        let new = tree(
            "new",
            &[
                Item::File("shrink.bin", 20),
                Item::File("grow.bin", 80),
                Item::File("same.bin", 5),
                Item::File("added.bin", 40),
            ],
        );
        let diff = fastdir_core::diff::build(&old, &new);
        let (old_side, new_side) = sides();
        let doc = document(&diff, &new, &old_side, &new_side, 0, None, None);
        assert_eq!(doc.schema, "fastdir.diff");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.total.delta, 40 + 70 - 80);
        let rows: Vec<(&str, i64)> = doc.entries.iter().map(|e| (e.status, e.delta)).collect();
        assert_eq!(
            rows,
            [
                ("changed", -80),
                ("changed", 70),
                ("added", 40),
                ("changed", 30),
            ],
            "the root row is included and ties fall back to the path"
        );
        let grow = doc
            .entries
            .iter()
            .find(|e| e.path.ends_with("grow.bin"))
            .unwrap();
        assert_eq!(grow.kind, "file");
        assert_eq!((grow.old_size, grow.new_size), (10, 80));
    }

    #[test]
    fn added_subtree_is_one_row_and_respects_depth() {
        let old = tree("old", &[Item::File("keep.bin", 1)]);
        let new = tree(
            "new",
            &[
                Item::File("keep.bin", 1),
                Item::Dir(
                    "newdir",
                    vec![Item::File("a.bin", 10), Item::File("b.bin", 20)],
                ),
            ],
        );
        let diff = fastdir_core::diff::build(&old, &new);
        let (old_side, new_side) = sides();
        let doc = document(&diff, &new, &old_side, &new_side, 0, None, None);
        let added: Vec<&str> = doc
            .entries
            .iter()
            .filter(|e| e.status == "added")
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(added.len(), 1);
        assert!(added[0].ends_with("newdir"));
        assert_eq!(doc.total.added, 30);

        let shallow = document(&diff, &new, &old_side, &new_side, 0, Some(0), None);
        assert_eq!(shallow.entries.len(), 1, "just the root");
        assert_eq!(shallow.entries[0].path, "new");
    }

    #[test]
    fn removed_rows_use_the_old_root_and_min_change() {
        let old = tree(
            "old",
            &[
                Item::File("keep.bin", 10),
                Item::Dir("gone", vec![Item::File("a.bin", 100)]),
                Item::File("tiny.bin", 5),
            ],
        );
        let new = tree("new", &[Item::File("keep.bin", 10)]);
        let diff = fastdir_core::diff::build(&old, &new);
        let (old_side, new_side) = sides();
        let doc = document(&diff, &new, &old_side, &new_side, 0, None, None);
        let removed: Vec<&DiffEntry> = doc
            .entries
            .iter()
            .filter(|e| e.status == "removed")
            .collect();
        assert_eq!(removed.len(), 2);
        let gone = removed.iter().find(|e| e.kind == "dir").unwrap();
        assert!(gone.path.ends_with("gone"));
        assert_eq!((gone.old_size, gone.new_size, gone.delta), (100, 0, -100));

        let filtered = document(&diff, &new, &old_side, &new_side, 6, None, None);
        assert!(filtered.entries.iter().all(|e| e.delta.unsigned_abs() >= 6));
        assert_eq!(filtered.entries.len(), 2, "the root and the removed folder");
    }

    #[test]
    fn limit_caps_and_csv_has_one_row_each() {
        let old = tree("old", &[Item::File("a", 1), Item::File("b", 2)]);
        let new = tree("new", &[Item::File("a", 5), Item::File("b", 4)]);
        let diff = fastdir_core::diff::build(&old, &new);
        let (old_side, new_side) = sides();
        let doc = document(&diff, &new, &old_side, &new_side, 0, None, Some(2));
        assert_eq!(doc.entries.len(), 2);
        let csv = csv(&doc.entries);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            "path,type,status,old_size,new_size,delta,old_disk_size,new_disk_size"
        );
        assert!(lines[1].starts_with("new"));
    }
}

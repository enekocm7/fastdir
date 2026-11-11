//! The versioned JSON documents the CLI and the MCP server emit.
//!
//! This is a slimmer, agent-oriented shape than `fastdir_core::export` (which
//! is designed to round-trip back into the UI). Every document carries
//! `schema` and `version`; fields are only ever added within a version, never
//! renamed or removed. The README documents the shape and the tests below pin
//! it down.
//!
//! The tree is serialized straight from [`Tree`] without building an
//! intermediate copy, so exporting a multi-million-entry drive scan does not
//! double its memory.

use fastdir_core::duplicates::{DuplicateOptions, DuplicateReport};
use fastdir_core::filter::FilterView;
use fastdir_core::model::{FileEntry, Node, NodeKind, Tree};
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct, Serializer};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Version of every document below. Bump only on incompatible changes.
pub const SCHEMA_VERSION: u32 = 1;
pub const SCAN_SCHEMA: &str = "fastdir.scan";
pub const TOP_SCHEMA: &str = "fastdir.top";
pub const DUPLICATES_SCHEMA: &str = "fastdir.duplicates";

/// `modified` as ISO-8601 UTC, or `None` when unknown.
pub fn iso_time(unix: i64) -> Option<String> {
    if unix <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(unix, 0).map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Inverse of [`iso_time`] / [`now_iso`]: Unix seconds, or `None` when the
/// text is not a timestamp this crate writes.
pub fn parse_iso(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.timestamp())
}

pub fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Dir => "dir",
        NodeKind::File => "file",
        NodeKind::FreeSpace => "free_space",
        NodeKind::Unknown => "unknown",
    }
}

/// How much of the tree to write.
#[derive(Clone, Copy, Debug)]
pub struct TreeShape {
    /// Deepest level to list children for (`None` = everything). The root is
    /// depth 0, so `Some(0)` writes the root alone.
    pub max_depth: Option<usize>,
    pub include_files: bool,
}

/// One entry and (lazily) everything below it.
pub struct EntryView<'a> {
    pub tree: &'a Tree,
    pub node: &'a Node,
    pub path: PathBuf,
    pub depth: usize,
    pub shape: TreeShape,
    /// Owner index in `tree.owners()` ([`fastdir_core::model::NO_OWNER`] when
    /// unknown or not collected).
    pub owner: u16,
    /// When set, only matches and their ancestors are written.
    pub filter: Option<&'a FilterView<'a>>,
}

impl EntryView<'_> {
    fn lists_children(&self) -> bool {
        self.shape.max_depth.is_none_or(|max| self.depth < max)
    }

    /// Whether this folder has any child that would be written.
    fn has_visible_children(&self) -> bool {
        match self.filter {
            Some(view) => self
                .tree
                .children(self.node)
                .iter()
                .any(|child| view.shows(self.tree, child)),
            None => self.node.has_children(),
        }
    }
}

impl Serialize for EntryView<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let node = self.node;
        let is_dir = node.is_dir();
        let mut st = s.serialize_struct("Entry", if is_dir { 13 } else { 11 })?;
        st.serialize_field("name", self.tree.name(node))?;
        st.serialize_field("path", &self.path.to_string_lossy())?;
        st.serialize_field("type", kind_label(node.kind()))?;
        st.serialize_field("size", &node.size)?;
        st.serialize_field("disk_size", &node.disk_size)?;
        st.serialize_field("items", &node.items())?;
        st.serialize_field("files", &node.files)?;
        st.serialize_field("folders", &node.dirs)?;
        st.serialize_field("modified", &iso_time(node.modified()))?;
        st.serialize_field("error", &node.error())?;
        st.serialize_field("hard_link", &node.hard_link())?;
        if let Some(owner) = self.tree.owner_name(self.owner) {
            st.serialize_field("owner", owner)?;
        }
        if is_dir {
            let lists = self.lists_children();
            // Truncated: the folder has entries that were cut off by
            // `max_depth` (not by `--no-files`, which is an explicit filter).
            st.serialize_field("truncated", &(!lists && self.has_visible_children()))?;
            st.serialize_field(
                "children",
                &Children {
                    parent: self,
                    lists,
                },
            )?;
        }
        st.end()
    }
}

struct Children<'a, 'b> {
    parent: &'a EntryView<'b>,
    lists: bool,
}

impl Serialize for Children<'_, '_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let p = self.parent;
        if !self.lists {
            return s.serialize_seq(Some(0))?.end();
        }
        let mut seq = s.serialize_seq(None)?;
        for (i, child) in p.tree.children(p.node).iter().enumerate() {
            if !child.is_dir() && !p.shape.include_files {
                continue;
            }
            if p.filter.is_some_and(|view| !view.shows(p.tree, child)) {
                continue;
            }
            seq.serialize_element(&EntryView {
                tree: p.tree,
                node: child,
                path: p.path.join(p.tree.name(child)),
                depth: p.depth + 1,
                shape: p.shape,
                owner: p.tree.child_owner(p.node, i),
                filter: p.filter,
            })?;
        }
        seq.end()
    }
}

/// Facts about the scan itself, shared by every document.
#[derive(Clone, Debug)]
pub struct ScanMeta {
    pub root: PathBuf,
    pub scanned_at: String,
    pub elapsed_ms: u64,
    pub errors: u64,
}

/// `fastdir scan` output (also written by the MCP `export_scan` tool).
pub struct ScanDoc<'a> {
    pub meta: &'a ScanMeta,
    pub tree: &'a Tree,
    /// Index path of the folder to write (`&[]` for the whole scan). `root`
    /// in the document is that folder's path.
    pub subtree: &'a [u32],
    pub shape: TreeShape,
    /// When set, only matches and their ancestors are written. Folder totals
    /// still describe the whole subtree.
    pub filter: Option<&'a FilterView<'a>>,
}

impl Serialize for ScanDoc<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let node = self
            .tree
            .get(self.subtree)
            .ok_or_else(|| serde::ser::Error::custom("export root not found"))?;
        let path = self
            .tree
            .fs_path(self.subtree)
            .unwrap_or_else(|| self.tree.root_path.clone());
        // Folders that could not be read, so their contents are missing
        // from the totals (run elevated to include them). Listed even when
        // `--max-depth` / `--no-files` / a filter hide them in `tree`.
        let unreadable: Vec<String> = self
            .tree
            .unreadable_dirs_in(self.subtree)
            .iter()
            .filter_map(|p| self.tree.fs_path(p))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut st = s.serialize_struct("ScanDoc", 9)?;
        st.serialize_field("schema", SCAN_SCHEMA)?;
        st.serialize_field("version", &SCHEMA_VERSION)?;
        st.serialize_field("fastdir_version", env!("CARGO_PKG_VERSION"))?;
        st.serialize_field("root", &path.to_string_lossy())?;
        st.serialize_field("scanned_at", &self.meta.scanned_at)?;
        st.serialize_field("elapsed_ms", &self.meta.elapsed_ms)?;
        st.serialize_field("errors", &self.meta.errors)?;
        st.serialize_field("unreadable", &unreadable)?;
        st.serialize_field(
            "tree",
            &EntryView {
                tree: self.tree,
                node,
                path,
                depth: 0,
                shape: self.shape,
                owner: self.tree.owner_at(self.subtree),
                filter: self.filter,
            },
        )?;
        st.end()
    }
}

/// A node without its children, for MCP replies and `top` totals.
pub fn entry_summary(tree: &Tree, node: &Node, path: &Path, owner: u16) -> Value {
    let mut v = json!({
        "name": tree.name(node),
        "path": path.to_string_lossy(),
        "type": kind_label(node.kind()),
        "size": node.size,
        "disk_size": node.disk_size,
        "items": node.items(),
        "files": node.files,
        "folders": node.dirs,
        "modified": iso_time(node.modified()),
        "error": node.error(),
        "hard_link": node.hard_link(),
    });
    if let Some(ext) = tree
        .ext_name(node)
        .filter(|_| node.kind() == NodeKind::File)
    {
        v["extension"] = json!(ext);
    }
    if let Some(owner) = tree.owner_name(owner) {
        v["owner"] = json!(owner);
    }
    v
}

/// One file of a `top` list.
pub fn file_entry(tree: &Tree, f: &FileEntry) -> Value {
    let path = tree.fs_path(&f.path).unwrap_or_default();
    let ext = tree.exts.get(f.ext as usize).map(|e| e.name.to_string());
    let mut v = json!({
        "name": &*f.name,
        "path": path.to_string_lossy(),
        "extension": ext,
        "size": f.size,
        "disk_size": f.disk_size,
        "modified": iso_time(f.modified),
    });
    if let Some(owner) = tree.owner_name(f.owner) {
        v["owner"] = json!(owner);
    }
    v
}

/// `fastdir top` output.
pub fn top_doc(meta: &ScanMeta, tree: &Tree, files: &[FileEntry], limit: usize) -> Value {
    let root = tree.root();
    json!({
        "schema": TOP_SCHEMA,
        "version": SCHEMA_VERSION,
        "fastdir_version": env!("CARGO_PKG_VERSION"),
        "root": meta.root.to_string_lossy(),
        "scanned_at": meta.scanned_at,
        "elapsed_ms": meta.elapsed_ms,
        "errors": meta.errors,
        "total": {
            "size": root.size,
            "disk_size": root.disk_size,
            "files": root.files,
            "folders": root.dirs,
        },
        "limit": limit,
        "files": files.iter().map(|f| file_entry(tree, f)).collect::<Vec<_>>(),
    })
}

/// `fastdir duplicates` output.
pub fn duplicates_doc(
    meta: &ScanMeta,
    report: &DuplicateReport,
    options: &DuplicateOptions,
) -> Value {
    json!({
        "schema": DUPLICATES_SCHEMA,
        "version": SCHEMA_VERSION,
        "fastdir_version": env!("CARGO_PKG_VERSION"),
        "root": meta.root.to_string_lossy(),
        "scanned_at": meta.scanned_at,
        "elapsed_ms": meta.elapsed_ms,
        "errors": meta.errors,
        "min_size": options.min_size,
        "extensions": options.extensions,
        "limit": options.limit,
        "candidates": report.candidates,
        "total_groups": report.total_groups,
        "total_wasted": report.total_wasted,
        "hashed_bytes": report.bytes_hashed,
        "hash_errors": report.hash_errors,
        "groups": report.groups.iter().map(|group| json!({
            "size": group.size,
            "hash": group.hash,
            "wasted": group.wasted(),
            "files": group.files.iter().map(|file| json!({
                "path": file.path.to_string_lossy(),
                "modified": iso_time(file.modified),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// `fastdir duplicates --format csv` output: one row per file.
pub fn duplicates_csv(report: &DuplicateReport) -> String {
    let mut out = String::from("group,wasted,path,size,hash,modified_iso\n");
    for (index, group) in report.groups.iter().enumerate() {
        for file in &group.files {
            out.push_str(&format!(
                "{},{},{},{},{},{}\n",
                index + 1,
                group.wasted(),
                csv_field(&file.path.to_string_lossy()),
                group.size,
                group.hash,
                iso_time(file.modified).unwrap_or_default()
            ));
        }
    }
    out
}

/// Quote one CSV field (RFC 4180).
pub fn csv_field(field: &str) -> String {
    if field.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// `fastdir top --format csv` output.
pub fn top_csv(tree: &Tree, files: &[FileEntry]) -> String {
    let with_owner = tree.has_owners();
    let mut out = String::from("path,size,disk_size,modified_iso");
    if with_owner {
        out.push_str(",owner");
    }
    out.push('\n');
    for f in files {
        let path = tree.fs_path(&f.path).unwrap_or_default();
        out.push_str(&csv_field(&path.to_string_lossy()));
        out.push_str(&format!(
            ",{},{},{}",
            f.size,
            f.disk_size,
            iso_time(f.modified).unwrap_or_default()
        ));
        if with_owner {
            out.push(',');
            out.push_str(&csv_field(tree.owner_name(f.owner).unwrap_or("")));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fastdir_core::model::{Arena, DirBuilder, Subtree};

    /// root/
    ///   docs/ (a.txt 100, b.md 50)
    ///   big.iso 1000
    pub(crate) fn sample_tree() -> Tree {
        let mut arena = Arena::default();
        let mut docs = DirBuilder::new(1_700_000_000);
        docs.add_file("a.txt", 100, 4096, 1_700_000_000);
        docs.add_file("b.md", 50, 4096, 1_600_000_000);
        let docs = docs.commit(&mut arena);
        let mut root = DirBuilder::new(1_700_000_000);
        root.add_dir("docs", docs);
        root.add_file("big.iso", 1000, 1024, 1_650_000_000);
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("root"))
    }

    fn meta() -> ScanMeta {
        ScanMeta {
            root: PathBuf::from("root"),
            scanned_at: "2026-01-01T00:00:00Z".into(),
            elapsed_ms: 12,
            errors: 0,
        }
    }

    fn scan_json(shape: TreeShape) -> Value {
        let tree = sample_tree();
        let meta = meta();
        serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &tree,
            subtree: &[],
            shape,
            filter: None,
        })
        .unwrap()
    }

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    }

    const FULL: TreeShape = TreeShape {
        max_depth: None,
        include_files: true,
    };

    #[test]
    fn scan_document_keys_are_stable() {
        let doc = scan_json(FULL);
        assert_eq!(
            keys(&doc),
            [
                "elapsed_ms",
                "errors",
                "fastdir_version",
                "root",
                "scanned_at",
                "schema",
                "tree",
                "unreadable",
                "version"
            ]
        );
        assert_eq!(doc["schema"], "fastdir.scan");
        assert_eq!(doc["unreadable"], serde_json::json!([]));
        assert_eq!(doc["version"], 1);
        let tree = &doc["tree"];
        assert_eq!(
            keys(tree),
            [
                "children",
                "disk_size",
                "error",
                "files",
                "folders",
                "hard_link",
                "items",
                "modified",
                "name",
                "path",
                "size",
                "truncated",
                "type"
            ]
        );
        // Files carry the same fields minus children/truncated.
        let file = tree["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "file")
            .unwrap();
        assert_eq!(
            keys(file),
            [
                "disk_size",
                "error",
                "files",
                "folders",
                "hard_link",
                "items",
                "modified",
                "name",
                "path",
                "size",
                "type"
            ]
        );
    }

    #[test]
    fn scan_document_values() {
        let doc = scan_json(FULL);
        let tree = &doc["tree"];
        assert_eq!(tree["type"], "dir");
        assert_eq!(tree["size"], 1150);
        assert_eq!(tree["files"], 3);
        assert_eq!(tree["folders"], 1);
        assert_eq!(tree["items"], 4);
        assert_eq!(tree["truncated"], false);
        let children = tree["children"].as_array().unwrap();
        // Largest first.
        assert_eq!(children[0]["name"], "big.iso");
        assert_eq!(children[0]["size"], 1000);
        assert_eq!(children[0]["disk_size"], 1024);
        assert_eq!(children[0]["modified"], "2022-04-15T05:20:00Z");
        let docs = &children[1];
        assert_eq!(docs["name"], "docs");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(docs["path"], format!("root{sep}docs"));
        assert_eq!(
            docs["children"][0]["path"],
            format!("root{sep}docs{sep}a.txt")
        );
    }

    #[test]
    fn unreadable_folders_are_listed_even_below_max_depth() {
        let mut arena = Arena::default();
        let mut locked = DirBuilder::new(0);
        locked.set_error();
        let locked = locked.commit(&mut arena);
        let mut sub = DirBuilder::new(0);
        sub.add_dir("locked", locked);
        let sub = sub.commit(&mut arena);
        let mut root = DirBuilder::new(0);
        root.add_dir("sub", sub);
        root.add_file("a.txt", 10, 10, 0);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("root"));
        let meta = meta();
        let doc = serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &tree,
            subtree: &[],
            shape: TreeShape {
                max_depth: Some(0),
                include_files: false,
            },
            filter: None,
        })
        .unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            doc["unreadable"],
            serde_json::json!([format!("root{sep}sub{sep}locked")])
        );
        assert_eq!(doc["tree"]["truncated"], true);
    }

    #[test]
    fn max_depth_truncates() {
        let doc = scan_json(TreeShape {
            max_depth: Some(0),
            include_files: true,
        });
        assert_eq!(doc["tree"]["truncated"], true);
        assert_eq!(doc["tree"]["children"], json!([]));
        assert_eq!(doc["tree"]["size"], 1150);

        let doc = scan_json(TreeShape {
            max_depth: Some(1),
            include_files: true,
        });
        let docs = &doc["tree"]["children"][1];
        assert_eq!(docs["truncated"], true);
        assert_eq!(docs["files"], 2);
    }

    #[test]
    fn no_files_lists_folders_only() {
        let doc = scan_json(TreeShape {
            max_depth: None,
            include_files: false,
        });
        let children = doc["tree"]["children"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["name"], "docs");
        assert_eq!(children[0]["children"], json!([]));
        assert_eq!(children[0]["truncated"], false);
        // Aggregates still count the files.
        assert_eq!(doc["tree"]["files"], 3);
    }

    #[test]
    fn filter_keeps_matches_and_ancestors() {
        let tree = sample_tree();
        let filter = fastdir_core::filter::Filter::parse("*.md", "", "", "", "").unwrap();
        let view = FilterView::new(&tree, &filter);
        let meta = meta();
        let doc = serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &tree,
            subtree: &[],
            shape: FULL,
            filter: Some(&view),
        })
        .unwrap();
        let children = doc["tree"]["children"].as_array().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0]["name"], "docs");
        assert_eq!(children[0]["children"][0]["name"], "b.md");
        // Folder totals still describe the whole subtree.
        assert_eq!(doc["tree"]["size"], 1150);
        assert_eq!(children[0]["size"], 150);
        assert_eq!(children[0]["files"], 2);

        let doc = serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &tree,
            subtree: &[],
            shape: TreeShape {
                max_depth: Some(0),
                include_files: true,
            },
            filter: Some(&view),
        })
        .unwrap();
        assert_eq!(doc["tree"]["truncated"], true);
    }

    #[test]
    fn top_document_shape() {
        let tree = sample_tree();
        let files = tree.top_files(2);
        let doc = top_doc(&meta(), &tree, &files, 2);
        assert_eq!(
            keys(&doc),
            [
                "elapsed_ms",
                "errors",
                "fastdir_version",
                "files",
                "limit",
                "root",
                "scanned_at",
                "schema",
                "total",
                "version"
            ]
        );
        assert_eq!(doc["schema"], "fastdir.top");
        assert_eq!(doc["total"]["size"], 1150);
        let files = doc["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            keys(&files[0]),
            ["disk_size", "extension", "modified", "name", "path", "size"]
        );
        assert_eq!(files[0]["name"], "big.iso");
        assert_eq!(files[0]["extension"], ".iso");
        assert_eq!(files[1]["name"], "a.txt");
    }

    #[test]
    fn owner_fields_appear_only_when_collected() {
        let mut table = fastdir_core::model::OwnerTable::default();
        let alice = table.intern(r"DOMAIN\alice");
        let mut arena = Arena::default();
        let mut dir = DirBuilder::new(0);
        dir.add_file_owned("a.txt", 10, 10, 0, alice);
        let node = dir.commit(&mut arena);
        arena.set_owners(table);
        let tree = Tree::new(
            Subtree::new_owned(arena, "root", node, alice),
            PathBuf::from("root"),
        );
        let meta = meta();
        let doc = serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &tree,
            subtree: &[],
            shape: FULL,
            filter: None,
        })
        .unwrap();
        assert_eq!(doc["tree"]["owner"], r"DOMAIN\alice");
        assert_eq!(doc["tree"]["children"][0]["owner"], r"DOMAIN\alice");

        let top = top_doc(&meta, &tree, &tree.top_files(5), 5);
        assert_eq!(top["files"][0]["owner"], r"DOMAIN\alice");
        assert!(
            top_csv(&tree, &tree.top_files(5))
                .starts_with("path,size,disk_size,modified_iso,owner\n")
        );

        // Without collection the fields stay absent and the CSV shape is
        // unchanged.
        let plain = sample_tree();
        let doc = serde_json::to_value(ScanDoc {
            meta: &meta,
            tree: &plain,
            subtree: &[],
            shape: FULL,
            filter: None,
        })
        .unwrap();
        assert!(doc["tree"].get("owner").is_none());
        assert!(!top_csv(&plain, &plain.top_files(5)).contains("owner"));
    }

    #[test]
    fn top_csv_quotes_paths() {
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file("a,b.txt", 10, 10, 0);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "r", root), PathBuf::from("r"));
        let csv = top_csv(&tree, &tree.top_files(5));
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            csv,
            format!("path,size,disk_size,modified_iso\n\"r{sep}a,b.txt\",10,10,\n")
        );
    }

    #[test]
    fn duplicates_document_shape() {
        let report = DuplicateReport {
            groups: vec![fastdir_core::duplicates::DuplicateGroup {
                size: 100,
                hash: "ab".repeat(32),
                files: vec![
                    fastdir_core::duplicates::DuplicateFile {
                        path: PathBuf::from("root/a.bin"),
                        modified: 1_700_000_000,
                    },
                    fastdir_core::duplicates::DuplicateFile {
                        path: PathBuf::from("root/b.bin"),
                        modified: 0,
                    },
                ],
            }],
            total_groups: 1,
            total_wasted: 100,
            candidates: 2,
            bytes_hashed: 200,
            hash_errors: 0,
            elapsed: std::time::Duration::from_millis(5),
            cancelled: false,
        };
        let options = DuplicateOptions {
            min_size: 50,
            extensions: vec![".bin".into()],
            limit: Some(10),
            threads: None,
        };
        let doc = duplicates_doc(&meta(), &report, &options);
        assert_eq!(
            keys(&doc),
            [
                "candidates",
                "elapsed_ms",
                "errors",
                "extensions",
                "fastdir_version",
                "groups",
                "hash_errors",
                "hashed_bytes",
                "limit",
                "min_size",
                "root",
                "scanned_at",
                "schema",
                "total_groups",
                "total_wasted",
                "version"
            ]
        );
        assert_eq!(doc["schema"], "fastdir.duplicates");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["total_wasted"], 100);
        let group = &doc["groups"][0];
        assert_eq!(group["size"], 100);
        assert_eq!(group["wasted"], 100);
        assert_eq!(group["files"][0]["path"], "root/a.bin");
        assert_eq!(group["files"][0]["modified"], "2023-11-14T22:13:20Z");
        assert_eq!(group["files"][1]["modified"], Value::Null);

        let csv = duplicates_csv(&report);
        assert_eq!(
            csv,
            "group,wasted,path,size,hash,modified_iso\n\
             1,100,root/a.bin,100,abababababababababababababababababababababababababababababababab,2023-11-14T22:13:20Z\n\
             1,100,root/b.bin,100,abababababababababababababababababababababababababababababababab,\n"
        );
    }

    #[test]
    fn unknown_modified_is_null() {
        assert_eq!(iso_time(0), None);
        assert_eq!(iso_time(-5), None);
        assert_eq!(iso_time(86_400).as_deref(), Some("1970-01-02T00:00:00Z"));
    }
}

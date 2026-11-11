//! Compare two scans of the same folder: what grew, what shrank, and what is
//! new or gone.
//!
//! Matching is by relative path under the scan root (exact names), so node
//! indices may differ between the two trees without confusing the result.
//! Changed nodes are keyed by [`Node::id`] of the **new** tree, which stays
//! stable when siblings are re-sorted; the UI can look a row up by its node
//! without carrying a second path map.
//!
//! Only changed nodes are stored, so a large scan where little moved costs
//! little extra memory. Removed subtrees are kept as one entry each (the
//! top-most missing node, with its totals), which keeps the removed list
//! useful without retaining every file below it.

use crate::model::{Node, Tree};
use std::collections::HashMap;
use std::path::Path;

/// What happened to a node that exists in the new scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    /// The node is not in the old scan.
    Added,
    /// It exists in both scans and got larger.
    Grown,
    /// It exists in both scans and got smaller.
    Shrunk,
}

impl ChangeStatus {
    /// Stable sort rank, from "needs attention" to "nothing happened".
    pub fn rank(self) -> u8 {
        match self {
            Self::Added => 0,
            Self::Grown => 1,
            Self::Shrunk => 2,
        }
    }
}

/// How one node changed between the two scans.
#[derive(Clone, Copy, Debug)]
pub struct NodeChange {
    pub status: ChangeStatus,
    /// Size in the old scan (`0` when the node is new).
    pub old_size: u64,
    /// Size in the new scan.
    pub new_size: u64,
    /// `new_size - old_size` (never overflows for real file sizes).
    pub delta: i64,
    /// Allocated size in the old scan (`0` when the node is new).
    pub old_disk_size: u64,
    /// Allocated size in the new scan.
    pub new_disk_size: u64,
}

impl NodeChange {
    /// The change as a fraction of the old size. `None` for new items, where
    /// a percentage would be meaningless.
    pub fn fraction(&self) -> Option<f64> {
        (self.old_size > 0).then(|| self.delta as f64 / self.old_size as f64)
    }
}

/// A node that existed in the old scan but not in the new one. Removed
/// folders keep their aggregate size, so one row stands for the whole subtree.
#[derive(Clone, Debug)]
pub struct RemovedEntry {
    /// Path relative to the scan root, with platform separators.
    pub path: String,
    pub name: String,
    pub size: u64,
    pub disk_size: u64,
    pub modified: i64,
    pub is_dir: bool,
    /// The node itself plus everything below it.
    pub items: u64,
}

/// Summary of a comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffTotals {
    /// Size of the root in the old scan.
    pub old_size: u64,
    /// Size of the root in the new scan.
    pub new_size: u64,
    /// Bytes in top-level added nodes (not double-counting their children).
    pub added: u64,
    /// Bytes in top-level removed nodes (not double-counting their children).
    pub removed: u64,
    pub added_items: u64,
    pub removed_items: u64,
    /// Change in free space when both scans carry drive volume information.
    pub free_delta: Option<i64>,
}

impl DiffTotals {
    /// Net change of the scanned bytes (`new - old`).
    pub fn net(&self) -> i64 {
        self.new_size as i64 - self.old_size as i64
    }
}

/// The result of [`build`]: per-node changes plus the removed entries and the
/// overall totals.
#[derive(Debug)]
pub struct ScanDiff {
    changes: HashMap<u32, NodeChange>,
    removed: Vec<RemovedEntry>,
    totals: DiffTotals,
    old_root: std::path::PathBuf,
    new_root: std::path::PathBuf,
}

impl ScanDiff {
    /// The change for `node`, or `None` when it is unchanged (or not in the
    /// new tree). `node` must belong to the new tree passed to [`build`].
    #[inline]
    pub fn change(&self, node: &Node) -> Option<&NodeChange> {
        self.changes.get(&node.id())
    }

    /// Every changed node, keyed by new-tree node id.
    pub fn changes(&self) -> &HashMap<u32, NodeChange> {
        &self.changes
    }

    /// Top-most removed nodes, largest first.
    pub fn removed(&self) -> &[RemovedEntry] {
        &self.removed
    }

    pub fn totals(&self) -> &DiffTotals {
        &self.totals
    }

    /// Root of the baseline scan.
    pub fn old_root(&self) -> &Path {
        &self.old_root
    }

    /// Root of the current scan.
    pub fn new_root(&self) -> &Path {
        &self.new_root
    }

    /// Whether both scans are of the same folder (case-insensitive on
    /// Windows, trailing separators ignored).
    pub fn roots_match(&self) -> bool {
        fn normalize(path: &Path) -> String {
            path.to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_string()
        }
        let (old, new) = (normalize(&self.old_root), normalize(&self.new_root));
        #[cfg(windows)]
        {
            old.eq_ignore_ascii_case(&new)
        }
        #[cfg(not(windows))]
        {
            old == new
        }
    }
}

/// Compare `old` with `new`, both scans of the same folder.
pub fn build(old: &Tree, new: &Tree) -> ScanDiff {
    let free_delta = match (old.free_bytes(), new.free_bytes()) {
        (Some(old_free), Some(new_free)) => Some(new_free as i64 - old_free as i64),
        _ => None,
    };
    let mut diff = ScanDiff {
        changes: HashMap::new(),
        removed: Vec::new(),
        totals: DiffTotals {
            old_size: old.scanned_bytes(),
            new_size: new.scanned_bytes(),
            free_delta,
            ..DiffTotals::default()
        },
        old_root: old.root_path.clone(),
        new_root: new.root_path.clone(),
    };
    let mut relative = Vec::new();
    diff.visit_pair(old, new, old.root(), new.root(), &mut relative);
    // A drive scan's root also carries `<Free space>` and `<Unknown>`, which
    // move in the opposite direction to the scanned bytes. The root row and
    // the totals describe the scanned data, so replace its per-node change
    // with the scanned-bytes delta.
    let root_delta = diff.totals.new_size as i64 - diff.totals.old_size as i64;
    if root_delta == 0 {
        diff.changes.remove(&new.root().id());
    } else {
        diff.changes.insert(
            new.root().id(),
            NodeChange {
                status: if root_delta > 0 {
                    ChangeStatus::Grown
                } else {
                    ChangeStatus::Shrunk
                },
                old_size: diff.totals.old_size,
                new_size: diff.totals.new_size,
                delta: root_delta,
                // The root node also carries the drive pseudo-items; report
                // the scanned data so this row agrees with `totals`.
                old_disk_size: old.scanned_disk_bytes(),
                new_disk_size: new.scanned_disk_bytes(),
            },
        );
    }
    diff.removed.sort_unstable_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    diff
}

impl ScanDiff {
    fn visit_pair(
        &mut self,
        old: &Tree,
        new: &Tree,
        old_node: &Node,
        new_node: &Node,
        relative: &mut Vec<String>,
    ) {
        // Drive pseudo-items are bookkeeping, not data the user compares.
        if old_node.is_volume_item() || new_node.is_volume_item() {
            return;
        }
        let delta = new_node.size as i64 - old_node.size as i64;
        if delta != 0 {
            let status = if delta > 0 {
                ChangeStatus::Grown
            } else {
                ChangeStatus::Shrunk
            };
            self.changes.insert(
                new_node.id(),
                NodeChange {
                    status,
                    old_size: old_node.size,
                    new_size: new_node.size,
                    delta,
                    old_disk_size: old_node.disk_size,
                    new_disk_size: new_node.disk_size,
                },
            );
        }
        if !old_node.is_dir() || !new_node.is_dir() {
            return;
        }

        let old_children = old.children(old_node);
        let by_name: HashMap<&str, usize> = old_children
            .iter()
            .enumerate()
            .map(|(ix, child)| (old.name(child), ix))
            .collect();
        let mut matched = vec![false; old_children.len()];
        for child in new.children(new_node) {
            if child.is_volume_item() {
                continue;
            }
            let name = new.name(child);
            // Exact names first; a case-only rename still names the same item
            // (Windows paths are case-insensitive), so fall back to an
            // insensitive match before calling it removed + added.
            let found = by_name.get(name).copied().or_else(|| {
                old_children
                    .iter()
                    .position(|old_child| old.name(old_child).eq_ignore_ascii_case(name))
            });
            if let Some(ix) = found {
                matched[ix] = true;
                relative.push(name.to_string());
                self.visit_pair(old, new, &old_children[ix], child, relative);
                relative.pop();
            } else {
                self.add_subtree(new, child, true);
            }
        }
        for (ix, child) in old_children.iter().enumerate() {
            if matched[ix] || child.is_volume_item() {
                continue;
            }
            relative.push(old.name(child).to_string());
            self.push_removed(old, child, relative);
            relative.pop();
        }
    }

    /// Mark every node of a subtree that exists only in the new scan.
    fn add_subtree(&mut self, tree: &Tree, node: &Node, top: bool) {
        if top {
            self.totals.added = self.totals.added.saturating_add(node.size);
            self.totals.added_items = self.totals.added_items.saturating_add(count_items(node));
        }
        self.changes.insert(
            node.id(),
            NodeChange {
                status: ChangeStatus::Added,
                old_size: 0,
                new_size: node.size,
                delta: node.size as i64,
                old_disk_size: 0,
                new_disk_size: node.disk_size,
            },
        );
        if node.is_dir() {
            for child in tree.children(node) {
                self.add_subtree(tree, child, false);
            }
        }
    }

    /// Record the top-most node of a subtree that only exists in the old
    /// scan. Its children are represented by its aggregate size.
    fn push_removed(&mut self, tree: &Tree, node: &Node, relative: &[String]) {
        self.totals.removed = self.totals.removed.saturating_add(node.size);
        self.totals.removed_items = self.totals.removed_items.saturating_add(count_items(node));
        self.removed.push(RemovedEntry {
            path: relative.join(std::path::MAIN_SEPARATOR_STR),
            name: tree.name(node).to_string(),
            size: node.size,
            disk_size: node.disk_size,
            modified: node.modified(),
            is_dir: node.is_dir(),
            items: count_items(node),
        });
    }
}

fn count_items(node: &Node) -> u64 {
    if node.is_dir() { node.items() + 1 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arena, DirBuilder, NodePath, Subtree};
    use std::path::{Path, PathBuf};

    enum Item {
        File(&'static str, u64),
        Dir(&'static str, Vec<Item>),
    }

    fn build_dir(arena: &mut Arena, items: &[Item], modified: i64) -> Node {
        let mut builder = DirBuilder::new(modified);
        for item in items {
            match item {
                Item::File(name, size) => builder.add_file(name, *size, *size, modified),
                Item::Dir(name, children) => {
                    let child = build_dir(arena, children, modified);
                    builder.add_dir(name, child);
                }
            }
        }
        builder.commit(arena)
    }

    fn tree(root: &str, items: &[Item]) -> Tree {
        let mut arena = Arena::default();
        let root_node = build_dir(&mut arena, items, 1_700_000_000);
        Tree::new(Subtree::new(arena, root, root_node), PathBuf::from(root))
    }

    fn find(tree: &Tree, path: &str) -> NodePath {
        tree.find(Path::new(path))
            .unwrap_or_else(|| panic!("{path} not found"))
    }

    fn change_at<'a>(diff: &'a ScanDiff, tree: &Tree, path: &str) -> &'a NodeChange {
        let path = find(tree, path);
        diff.change(tree.get(&path).unwrap())
            .unwrap_or_else(|| panic!("no change recorded for {path:?}"))
    }

    #[test]
    fn grew_and_shrank_per_file_and_root() {
        let old = tree("/root", &[Item::File("a.txt", 10), Item::File("b.txt", 20)]);
        let new = tree("/root", &[Item::File("a.txt", 15), Item::File("b.txt", 5)]);
        let diff = build(&old, &new);

        let a = change_at(&diff, &new, "/root/a.txt");
        assert_eq!(a.status, ChangeStatus::Grown);
        assert_eq!((a.old_size, a.new_size, a.delta), (10, 15, 5));
        let b = change_at(&diff, &new, "/root/b.txt");
        assert_eq!(b.status, ChangeStatus::Shrunk);
        assert_eq!((b.old_size, b.new_size, b.delta), (20, 5, -15));
        // The root delta is the sum of its children, not a double count.
        let root = change_at(&diff, &new, "/root");
        assert_eq!(root.status, ChangeStatus::Shrunk);
        assert_eq!(root.delta, -10);
        let totals = diff.totals();
        assert_eq!(
            (totals.old_size, totals.new_size, totals.net()),
            (30, 20, -10)
        );
        assert_eq!((totals.added, totals.removed), (0, 0));
        assert!(diff.removed().is_empty());
    }

    #[test]
    fn folder_delta_matches_the_sum_of_its_children() {
        let old = tree(
            "/root",
            &[Item::Dir(
                "sub",
                vec![Item::File("a.bin", 10), Item::File("b.bin", 20)],
            )],
        );
        let new = tree(
            "/root",
            &[Item::Dir(
                "sub",
                vec![Item::File("a.bin", 15), Item::File("b.bin", 25)],
            )],
        );
        let diff = build(&old, &new);
        let a = change_at(&diff, &new, "/root/sub/a.bin");
        let b = change_at(&diff, &new, "/root/sub/b.bin");
        let sub = change_at(&diff, &new, "/root/sub");
        assert_eq!(a.delta + b.delta, sub.delta);
        assert_eq!(sub.delta, 10);
        assert_eq!(diff.totals().net(), 10);
    }

    #[test]
    fn added_file_is_new_and_counted_once() {
        let old = tree("/root", &[Item::File("a.txt", 10)]);
        let new = tree(
            "/root",
            &[Item::File("a.txt", 10), Item::File("new.bin", 7)],
        );
        let diff = build(&old, &new);

        let added = change_at(&diff, &new, "/root/new.bin");
        assert_eq!(added.status, ChangeStatus::Added);
        assert_eq!((added.old_size, added.new_size, added.delta), (0, 7, 7));
        assert!(added.fraction().is_none());
        assert_eq!(diff.totals().added, 7);
        assert_eq!(diff.totals().added_items, 1);
        assert_eq!(diff.totals().net(), 7);
    }

    #[test]
    fn added_folder_marks_its_children_too_but_counts_it_once() {
        let old = tree("/root", &[Item::File("a.txt", 10)]);
        let new = tree(
            "/root",
            &[
                Item::File("a.txt", 10),
                Item::Dir(
                    "newdir",
                    vec![Item::File("x.bin", 4), Item::File("y.bin", 6)],
                ),
            ],
        );
        let diff = build(&old, &new);

        assert_eq!(
            change_at(&diff, &new, "/root/newdir").status,
            ChangeStatus::Added
        );
        assert_eq!(
            change_at(&diff, &new, "/root/newdir/x.bin").status,
            ChangeStatus::Added
        );
        // The folder's 10 bytes are in the treemap-visible nodes, but the
        // totals only count the top-most added node.
        assert_eq!(diff.totals().added, 10);
        assert_eq!(diff.totals().added_items, 3);
    }

    #[test]
    fn removed_file_is_listed_with_its_size() {
        let old = tree(
            "/root",
            &[Item::File("keep.txt", 10), Item::File("gone.bin", 7)],
        );
        let new = tree("/root", &[Item::File("keep.txt", 10)]);
        let diff = build(&old, &new);

        assert_eq!(diff.removed().len(), 1);
        let gone = &diff.removed()[0];
        assert_eq!(gone.name, "gone.bin");
        assert_eq!(gone.path, "gone.bin");
        assert_eq!(gone.size, 7);
        assert!(!gone.is_dir);
        assert_eq!(diff.totals().removed, 7);
        assert_eq!(diff.totals().removed_items, 1);
        assert_eq!(diff.totals().net(), -7);
    }

    #[test]
    fn a_removed_folder_is_one_entry_with_its_subtree() {
        let old = tree(
            "/root",
            &[
                Item::File("keep.txt", 10),
                Item::Dir(
                    "olddir",
                    vec![Item::File("x.bin", 4), Item::File("y.bin", 6)],
                ),
            ],
        );
        let new = tree("/root", &[Item::File("keep.txt", 10)]);
        let diff = build(&old, &new);

        assert_eq!(diff.removed().len(), 1);
        let gone = &diff.removed()[0];
        assert_eq!(gone.name, "olddir");
        assert_eq!(gone.size, 10);
        assert!(gone.is_dir);
        assert_eq!(gone.items, 3, "the folder itself and two files");
        assert_eq!(diff.totals().removed, 10);
        assert_eq!(diff.totals().removed_items, 3);
    }

    #[test]
    fn a_rename_is_removed_plus_added() {
        let old = tree("/root", &[Item::File("before.txt", 12)]);
        let new = tree("/root", &[Item::File("after.txt", 12)]);
        let diff = build(&old, &new);

        assert_eq!(
            change_at(&diff, &new, "/root/after.txt").status,
            ChangeStatus::Added
        );
        assert_eq!(diff.removed().len(), 1);
        assert_eq!(diff.removed()[0].name, "before.txt");
        assert_eq!(diff.totals().added, 12);
        assert_eq!(diff.totals().removed, 12);
        assert_eq!(diff.totals().net(), 0);
    }

    #[test]
    fn matching_works_across_different_sibling_order() {
        // Child order is by size, so the two trees store the same names in
        // different positions. Matching must still pair them by path.
        let old = tree(
            "/root",
            &[
                Item::Dir("a", vec![Item::File("x.bin", 100)]),
                Item::Dir("b", vec![Item::File("x.bin", 5)]),
            ],
        );
        let new = tree(
            "/root",
            &[
                Item::Dir("a", vec![Item::File("x.bin", 10)]),
                Item::Dir("b", vec![Item::File("x.bin", 500)]),
            ],
        );
        let diff = build(&old, &new);

        assert_eq!(
            change_at(&diff, &new, "/root/a/x.bin").status,
            ChangeStatus::Shrunk
        );
        assert_eq!(
            change_at(&diff, &new, "/root/b/x.bin").status,
            ChangeStatus::Grown
        );
        assert!(diff.removed().is_empty());
    }

    #[test]
    fn a_case_only_rename_is_the_same_item() {
        let old = tree("/root", &[Item::File("report.txt", 12)]);
        let new = tree("/root", &[Item::File("REPORT.TXT", 12)]);
        let diff = build(&old, &new);
        assert!(diff.removed().is_empty(), "not removed + added");
        assert_eq!(diff.totals().added, 0);
        assert_eq!(diff.totals().removed, 0);
    }

    #[test]
    fn drive_pseudo_items_are_not_compared() {
        let mut old = tree("/root", &[Item::File("a.txt", 10)]);
        let mut new = tree("/root", &[Item::File("a.txt", 20)]);
        old.attach_volume_info(100, 40);
        new.attach_volume_info(100, 25);
        let diff = build(&old, &new);
        assert!(
            diff.removed()
                .iter()
                .all(|entry| !entry.name.starts_with('<')),
            "free space and unknown are not data"
        );
        let free = new
            .children(new.root())
            .iter()
            .find(|node| node.is_special())
            .unwrap();
        assert!(diff.change(free).is_none());
    }

    #[test]
    fn root_mismatch_is_reported() {
        let old = tree("/old", &[Item::File("a.txt", 1)]);
        let new = tree("/new", &[Item::File("a.txt", 1)]);
        let diff = build(&old, &new);
        assert!(!diff.roots_match());

        let same = build(&old, &tree("/old/", &[Item::File("a.txt", 2)]));
        assert!(same.roots_match(), "a trailing separator is not a mismatch");
    }

    #[test]
    fn unchanged_nodes_are_not_stored() {
        let old = tree("/root", &[Item::File("a.txt", 10), Item::File("b.txt", 20)]);
        let new = tree("/root", &[Item::File("a.txt", 10), Item::File("b.txt", 30)]);
        let diff = build(&old, &new);
        let a = find(&new, "/root/a.txt");
        assert!(diff.change(new.get(&a).unwrap()).is_none());
        assert_eq!(diff.changes().len(), 2, "the root and b.txt");
    }

    #[test]
    fn free_space_change_is_reported_for_two_drive_scans() {
        let mut old = tree("/root", &[Item::File("a.txt", 10)]);
        let mut new = tree("/root", &[Item::File("a.txt", 20)]);
        old.attach_volume_info(100, 40);
        new.attach_volume_info(100, 30);
        let diff = build(&old, &new);
        assert_eq!(diff.totals().free_delta, Some(-10));
        // Totals describe the scanned data, not the (constant) volume size.
        assert_eq!(diff.totals().old_size, 10);
        assert_eq!(diff.totals().new_size, 20);
        assert_eq!(diff.totals().net(), 10);
    }
}

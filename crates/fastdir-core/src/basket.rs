//! The deletion basket: files and folders collected while browsing, to be
//! recycled or deleted together in one confirmed step.
//!
//! Items are keyed by their file-system path, not by tree position, because
//! tree positions shift whenever something is removed, rescanned or filtered.
//! The basket is plain data so it can be tested without a GUI; the UI keeps it
//! in sync with the current scan.

use std::path::{Path, PathBuf};

/// One item in the basket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasketItem {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Size in bytes (a folder's whole subtree), as of the last sync.
    pub size: u64,
    /// Files and folders this item stands for, itself included.
    pub items: u64,
}

/// An ordered collection of paths awaiting a bulk action.
#[derive(Clone, Debug, Default)]
pub struct Basket {
    items: Vec<BasketItem>,
}

/// Whether two paths name the same file. Windows paths ignore case.
fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Whether `path` is strictly inside `ancestor`.
fn inside(path: &Path, ancestor: &Path) -> bool {
    if same(path, ancestor) {
        return false;
    }
    if cfg!(windows) {
        let (p, a) = (
            PathBuf::from(path.to_string_lossy().to_lowercase()),
            PathBuf::from(ancestor.to_string_lossy().to_lowercase()),
        );
        p.starts_with(a)
    } else {
        path.starts_with(ancestor)
    }
}

impl Basket {
    /// Number of items collected (nested ones included).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.items.iter().any(|i| same(&i.path, path))
    }

    /// Add `item`, or refresh it when its path is already in the basket.
    /// Returns whether it was newly added.
    pub fn add(&mut self, item: BasketItem) -> bool {
        match self.items.iter_mut().find(|i| same(&i.path, &item.path)) {
            Some(existing) => {
                *existing = item;
                false
            }
            None => {
                self.items.push(item);
                true
            }
        }
    }

    /// Remove `path`. Returns whether it was there.
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.items.len();
        self.items.retain(|i| !same(&i.path, path));
        self.items.len() != before
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Every item in the order it was added.
    pub fn items(&self) -> &[BasketItem] {
        &self.items
    }

    /// Whether `path` sits inside a folder that is also in the basket, so it
    /// is covered by that folder and must not be counted or deleted twice.
    pub fn is_covered(&self, path: &Path) -> bool {
        self.items.iter().any(|i| i.is_dir && inside(path, &i.path))
    }

    /// The items an action acts on: those not inside another basket folder.
    pub fn effective(&self) -> Vec<&BasketItem> {
        self.items
            .iter()
            .filter(|i| !self.is_covered(&i.path))
            .collect()
    }

    /// Total size of [`Self::effective`] in bytes.
    pub fn total_size(&self) -> u64 {
        self.effective().iter().map(|i| i.size).sum()
    }

    /// Total files and folders of [`Self::effective`], folder contents included.
    pub fn total_items(&self) -> u64 {
        self.effective().iter().map(|i| i.items).sum()
    }

    /// Re-measure every item with `lookup` (`None` means it is gone from the
    /// scan and is dropped; `Some((size, items))` is its current size).
    /// Returns how many items were dropped.
    pub fn sync(&mut self, mut lookup: impl FnMut(&Path) -> Option<(u64, u64)>) -> usize {
        let before = self.items.len();
        self.items.retain_mut(|i| match lookup(&i.path) {
            Some((size, items)) => {
                i.size = size;
                i.items = items;
                true
            }
            None => false,
        });
        before - self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, is_dir: bool, size: u64) -> BasketItem {
        BasketItem {
            path: PathBuf::from(path),
            is_dir,
            size,
            items: if is_dir { 10 } else { 1 },
        }
    }

    #[test]
    fn add_remove_and_contains() {
        let mut b = Basket::default();
        assert!(b.is_empty());
        assert!(b.add(item("/a/one.txt", false, 5)));
        assert!(b.add(item("/a/two.txt", false, 7)));
        assert_eq!(b.len(), 2);
        assert!(b.contains(Path::new("/a/one.txt")));
        assert!(b.remove(Path::new("/a/one.txt")));
        assert!(!b.remove(Path::new("/a/one.txt")));
        assert!(!b.contains(Path::new("/a/one.txt")));
        b.clear();
        assert!(b.is_empty());
    }

    #[test]
    fn adding_twice_refreshes_instead_of_duplicating() {
        let mut b = Basket::default();
        assert!(b.add(item("/a/one.txt", false, 5)));
        assert!(!b.add(item("/a/one.txt", false, 9)));
        assert_eq!(b.len(), 1);
        assert_eq!(b.total_size(), 9);
    }

    #[test]
    fn totals_never_count_nested_items_twice() {
        let mut b = Basket::default();
        b.add(item("/a/inner/big.bin", false, 100));
        b.add(item("/a/inner", true, 300));
        b.add(item("/b/other.bin", false, 50));
        assert_eq!(b.len(), 3);
        // big.bin is inside the folder that is also in the basket.
        assert_eq!(b.total_size(), 350);
        assert_eq!(b.total_items(), 11);
        assert_eq!(b.effective().len(), 2);
        assert!(b.is_covered(Path::new("/a/inner/big.bin")));
        assert!(!b.is_covered(Path::new("/a/inner")));
        // A sibling whose name merely starts with the folder's is not inside.
        assert!(!b.is_covered(Path::new("/a/inner2/x.bin")));
    }

    #[test]
    fn removing_the_folder_uncovers_its_contents() {
        let mut b = Basket::default();
        b.add(item("/a/inner/big.bin", false, 100));
        b.add(item("/a/inner", true, 300));
        b.remove(Path::new("/a/inner"));
        assert_eq!(b.total_size(), 100);
    }

    #[test]
    fn sync_updates_sizes_and_drops_missing_items() {
        let mut b = Basket::default();
        b.add(item("/a/keep.bin", false, 10));
        b.add(item("/a/gone.bin", false, 20));
        b.add(item("/a/dir", true, 30));
        let dropped = b.sync(|p| match p.file_name().and_then(|n| n.to_str()) {
            Some("keep.bin") => Some((11, 1)),
            Some("dir") => Some((5, 2)),
            _ => None,
        });
        assert_eq!(dropped, 1);
        assert_eq!(b.len(), 2);
        assert_eq!(b.total_size(), 16);
        assert_eq!(b.total_items(), 3);
        assert!(!b.contains(Path::new("/a/gone.bin")));
    }

    #[test]
    fn order_of_insertion_is_kept() {
        let mut b = Basket::default();
        b.add(item("/z", false, 1));
        b.add(item("/a", false, 1));
        let names: Vec<_> = b.items().iter().map(|i| i.path.clone()).collect();
        assert_eq!(names, [PathBuf::from("/z"), PathBuf::from("/a")]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_ignore_case() {
        let mut b = Basket::default();
        b.add(item(r"C:\Data\Logs", true, 10));
        assert!(b.contains(Path::new(r"c:\data\logs")));
        assert!(b.is_covered(Path::new(r"C:\DATA\logs\a.log")));
        assert!(!b.add(item(r"c:\data\LOGS", true, 12)));
        assert_eq!(b.len(), 1);
    }
}

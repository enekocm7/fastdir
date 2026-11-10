//! Recover Microsoft Store (MSIX) packages from an unreadable `WindowsApps`
//! folder.
//!
//! Without elevation `C:\Program Files\WindowsApps` cannot be listed, so a
//! scan marks it unreadable and its bytes (often 10 GB or more) end up in
//! `<Unknown>`. Each package folder inside it can still be opened by its
//! full path, though, and the package names are in the registry (the current
//! user's package repository and the all-users store). So after a scan we
//! look each one up, scan the folders that exist and insert them under
//! `WindowsApps` as ordinary entries. The folder stays flagged unreadable,
//! because packages not registered for this machine's users are still
//! missing.

use crate::model::{NodePath, Subtree, Tree};
use crate::scanner::{self, Progress, ScanOptions};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The folder name Windows keeps Store packages in.
pub const WINDOWS_APPS: &str = "WindowsApps";

/// A package folder found inside an unreadable `WindowsApps` folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDir {
    /// The `WindowsApps` folder it belongs in.
    pub parent: PathBuf,
    /// The package folder itself.
    pub path: PathBuf,
}

/// Full names of the packages registered on this machine (installed for
/// the current user or provisioned for all users), sorted and de-duplicated.
/// Each is the name of a folder under `WindowsApps`. Empty off Windows.
pub fn installed_package_names() -> Vec<String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        const USER_REPOSITORY: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\\
             Windows\\CurrentVersion\\AppModel\\Repository\\Packages";
        const ALL_USERS: &str =
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Appx\\AppxAllUserStore\\Applications";
        let mut names: BTreeSet<String> = BTreeSet::new();
        names.extend(registry::subkey_names(HKEY_CURRENT_USER, USER_REPOSITORY));
        names.extend(registry::subkey_names(HKEY_LOCAL_MACHINE, ALL_USERS));
        names.into_iter().filter(|n| is_plain_name(n)).collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Whether `name` is a single path component (never `..` or a path).
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['\\', '/', ':'])
}

/// The unreadable `WindowsApps` folders in `tree`, as file-system paths.
pub fn unreadable_windows_apps(tree: &Tree) -> Vec<PathBuf> {
    tree.unreadable_dirs()
        .iter()
        .filter(|p| {
            tree.get(p)
                .is_some_and(|n| tree.name(n).eq_ignore_ascii_case(WINDOWS_APPS))
        })
        .filter_map(|p| tree.fs_path(p))
        .collect()
}

/// The package folders named in `names` that exist inside the unreadable
/// `WindowsApps` folders of `tree` and are not in the tree yet.
pub fn package_dirs(tree: &Tree, names: &[String]) -> Vec<PackageDir> {
    let mut out = Vec::new();
    for parent in unreadable_windows_apps(tree) {
        let present: BTreeSet<String> = tree
            .find(&parent)
            .and_then(|p| tree.get(&p).map(|n| (p, *n)))
            .map(|(_, node)| {
                tree.children(&node)
                    .iter()
                    .map(|c| tree.name(c).to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        for name in names.iter().filter(|n| is_plain_name(n)) {
            if present.contains(&name.to_ascii_lowercase()) {
                continue;
            }
            let path = parent.join(name);
            if path.is_dir() {
                out.push(PackageDir {
                    parent: parent.clone(),
                    path,
                });
            }
        }
    }
    out
}

/// Scan each package folder. Stops early (returning what it has) once
/// `progress` is cancelled. Does not touch any tree, so it can run without
/// holding a lock on a live one.
pub fn scan_packages(
    dirs: &[PackageDir],
    options: &ScanOptions,
    progress: &Progress,
) -> Vec<(PathBuf, Subtree)> {
    let mut out = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if progress.cancelled() {
            break;
        }
        *progress.current.lock() = dir.path.to_string_lossy().into_owned();
        out.push((
            dir.parent.clone(),
            scanner::scan_subtree(&dir.path, options),
        ));
    }
    out
}

/// Insert scanned packages under their `WindowsApps` folders. Returns how
/// many were inserted. Call this before `attach_volume_info`: on a drive
/// scan an insert is taken from `<Free space>`, while these bytes belong to
/// `<Unknown>`.
pub fn insert_packages(tree: &mut Tree, scanned: Vec<(PathBuf, Subtree)>) -> usize {
    let mut inserted = 0;
    for (parent, sub) in scanned {
        // Inserting re-sorts siblings, so look the parent up every time.
        let Some(path): Option<NodePath> = tree.find(&parent) else {
            continue;
        };
        if tree.insert(&path, sub).is_some() {
            inserted += 1;
        }
    }
    inserted
}

/// Find, scan and insert every registered package folder hidden in an
/// unreadable `WindowsApps` folder of `tree`. Returns how many were added.
pub fn recover_packages(tree: &mut Tree, options: &ScanOptions, progress: &Progress) -> usize {
    let dirs = package_dirs(tree, &installed_package_names());
    if dirs.is_empty() {
        return 0;
    }
    let scanned = scan_packages(&dirs, options, progress);
    let inserted = insert_packages(tree, scanned);
    if inserted > 0 {
        // Packages bring new extensions; rank them with the rest.
        tree.rebuild_extensions();
    }
    inserted
}

/// [`recover_packages`] for a tree the UI reads while the scan runs: the
/// packages are scanned without holding the lock, which is only taken for
/// the lookup and the inserts.
pub fn recover_packages_live(
    live: &parking_lot::RwLock<Tree>,
    options: &ScanOptions,
    progress: &Progress,
) -> usize {
    let dirs = {
        let tree = live.read();
        if unreadable_windows_apps(&tree).is_empty() {
            return 0;
        }
        package_dirs(&tree, &installed_package_names())
    };
    if dirs.is_empty() {
        return 0;
    }
    let scanned = scan_packages(&dirs, options, progress);
    let mut tree = live.write();
    let inserted = insert_packages(&mut tree, scanned);
    if inserted > 0 {
        // Packages bring new extensions; rank them with the rest.
        tree.rebuild_extensions();
    }
    inserted
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };

    /// Names of the direct subkeys of `root\path`; empty when it cannot be
    /// opened.
    pub fn subkey_names(root: HKEY, path: &str) -> Vec<String> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and `key` is a valid out pointer.
        if unsafe { RegOpenKeyExW(root, wide.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut buf = [0u16; 256];
        for index in 0.. {
            let mut len = buf.len() as u32;
            // SAFETY: `buf` holds `len` units; the optional outputs are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc == ERROR_SUCCESS {
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
            // Any other error (a name longer than a key may be) skips it.
        }
        // SAFETY: `key` was opened above.
        unsafe { RegCloseKey(key) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Arena, DirBuilder};
    use std::path::Path;

    /// A tree rooted at `root` whose `Program Files/WindowsApps` folder is
    /// unreadable and empty, like a non-elevated scan of `C:\`.
    fn locked_tree(root: &Path) -> Tree {
        let mut arena = Arena::default();
        let mut apps = DirBuilder::new(0);
        apps.set_error();
        let apps = apps.commit(&mut arena);
        let mut pf = DirBuilder::new(0);
        pf.add_dir(WINDOWS_APPS, apps);
        pf.add_file("other.txt", 7, 7, 0);
        let pf = pf.commit(&mut arena);
        let mut top = DirBuilder::new(0);
        top.add_dir("Program Files", pf);
        let top = top.commit(&mut arena);
        Tree::new(
            Subtree::new(arena, &root.to_string_lossy(), top),
            root.to_path_buf(),
        )
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fastdir-winapps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn plain_names_only() {
        assert!(is_plain_name("Microsoft.App_1.0.0.0_x64__8wekyb3d8bbwe"));
        assert!(!is_plain_name(".."));
        assert!(!is_plain_name("a\\b"));
        assert!(!is_plain_name("a/b"));
        assert!(!is_plain_name("C:"));
        assert!(!is_plain_name(""));
    }

    #[test]
    fn registered_packages_are_scanned_into_windows_apps() {
        let root = temp_root("recover");
        let apps = root.join("Program Files").join(WINDOWS_APPS);
        let pkg = apps.join("Vendor.App_1.0.0.0_x64__abc");
        std::fs::create_dir_all(pkg.join("Assets")).unwrap();
        std::fs::write(pkg.join("app.exe"), vec![0u8; 1000]).unwrap();
        std::fs::write(pkg.join("Assets").join("logo.png"), vec![0u8; 300]).unwrap();

        let mut tree = locked_tree(&root);
        assert_eq!(unreadable_windows_apps(&tree), vec![apps.clone()]);
        let names = vec![
            "Vendor.App_1.0.0.0_x64__abc".to_string(),
            "Not.Installed_1.0.0.0_x64__xyz".to_string(),
            "..".to_string(),
        ];
        let dirs = package_dirs(&tree, &names);
        assert_eq!(
            dirs,
            vec![PackageDir {
                parent: apps.clone(),
                path: pkg.clone(),
            }]
        );
        let scanned = scan_packages(&dirs, &ScanOptions::default(), &Progress::default());
        assert_eq!(insert_packages(&mut tree, scanned), 1);

        let apps_node = tree.get(&tree.find(&apps).unwrap()).unwrap();
        assert_eq!(apps_node.size, 1300);
        assert_eq!(apps_node.files, 2);
        assert!(
            apps_node.error(),
            "still flagged: other packages may be missing"
        );
        assert_eq!(tree.root().size, 1307);
        assert!(tree.find(&pkg.join("Assets").join("logo.png")).is_some());
        // Already present now, so a second pass finds nothing new.
        assert!(package_dirs(&tree, &names).is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn readable_windows_apps_is_left_alone() {
        let root = temp_root("readable");
        let mut arena = Arena::default();
        let apps = DirBuilder::new(0).commit(&mut arena);
        let mut top = DirBuilder::new(0);
        top.add_dir(WINDOWS_APPS, apps);
        let top = top.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "r", top), root);
        assert!(unreadable_windows_apps(&tree).is_empty());
        assert!(package_dirs(&tree, &["x".to_string()]).is_empty());
    }
}

//! Enumerate mounted volumes with their capacity.

use std::path::PathBuf;

#[cfg(not(windows))]
use std::ffi::OsStr;
#[cfg(not(windows))]
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Drive {
    pub path: PathBuf,
    pub label: String,
    pub fs: String,
    pub kind: DriveKind,
    pub total: u64,
    pub free: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

impl DriveKind {
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Fixed => "Local Disk",
            DriveKind::Removable => "Removable",
            DriveKind::Network => "Network",
            DriveKind::Optical => "Optical",
            DriveKind::Ram => "RAM Disk",
            DriveKind::Unknown => "Drive",
        }
    }

    /// Stable id for localization (`fixed`, `removable`, `network`,
    /// `optical`, `ram`, `unknown`); see `fastdir_locale::drive_kind`.
    pub fn id(self) -> &'static str {
        match self {
            DriveKind::Fixed => "fixed",
            DriveKind::Removable => "removable",
            DriveKind::Network => "network",
            DriveKind::Optical => "optical",
            DriveKind::Ram => "ram",
            DriveKind::Unknown => "unknown",
        }
    }
}

impl Drive {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            self.used() as f32 / self.total as f32
        }
    }

    pub fn title(&self) -> String {
        self.title_in(self.kind.label())
    }

    /// Drive title with a pre-localized kind label
    /// (from `fastdir_locale::drive_kind`).
    pub fn title_in(&self, kind_label: &str) -> String {
        let letter = self
            .path
            .to_string_lossy()
            .trim_end_matches('\\')
            .to_string();
        if self.label.is_empty() {
            format!("{} ({})", kind_label, letter)
        } else {
            format!("{} ({})", self.label, letter)
        }
    }
}

#[cfg(windows)]
pub fn list() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
    };

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    let mut buf = vec![0u16; 512];
    let len = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    if len == 0 || len > buf.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for root in buf[..len].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut root_z: Vec<u16> = root.to_vec();
        root_z.push(0);
        let kind = match unsafe { GetDriveTypeW(root_z.as_ptr()) } {
            DRIVE_FIXED => DriveKind::Fixed,
            DRIVE_REMOVABLE => DriveKind::Removable,
            DRIVE_REMOTE => DriveKind::Network,
            DRIVE_CDROM => DriveKind::Optical,
            DRIVE_RAMDISK => DriveKind::Ram,
            _ => DriveKind::Unknown,
        };
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let has_info = unsafe {
            GetVolumeInformationW(
                root_z.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        } != 0;
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let has_space =
            unsafe { GetDiskFreeSpaceExW(root_z.as_ptr(), &mut avail, &mut total, &mut free) } != 0;
        // Skip empty card readers / optical drives without media.
        if !has_space && !has_info {
            continue;
        }
        let wstr = |b: &[u16]| {
            let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf16_lossy(&b[..n])
        };
        out.push(Drive {
            path: PathBuf::from(String::from_utf16_lossy(root)),
            label: wstr(&label),
            fs: wstr(&fs),
            kind,
            total,
            free,
        });
    }
    out
}

#[cfg(not(windows))]
pub fn list() -> Vec<Drive> {
    let mut out: Vec<Drive> = mounts()
        .into_iter()
        .filter(|m| {
            !is_pseudo_fstype(&m.fstype)
                && !under_pseudo_root(&m.mount)
                && !is_hidden_system_volume(&m.mount)
        })
        .filter_map(|m| {
            // Without sizes the entry is useless in the drive picker; stale
            // or unreadable mounts (a dead autofs trigger, a denied FUSE
            // mount) are dropped instead of shown with zeros.
            let (total, free) = m.space.or_else(|| fs_space(&m.mount))?;
            Some(Drive {
                label: label_for(&m.device, &m.mount),
                kind: classify(&m.fstype, &m.mount.to_string_lossy()),
                path: m.mount,
                fs: m.fstype,
                total,
                free,
            })
        })
        .collect();
    if out.is_empty() {
        // Last resort so the drive picker is never blank: the root with
        // whatever sizes the OS reports (possibly zeros).
        let root = PathBuf::from("/");
        let (total, free) = fs_space(&root).unwrap_or((0, 0));
        out.push(Drive {
            path: root,
            label: "Root".into(),
            fs: String::new(),
            kind: DriveKind::Fixed,
            total,
            free,
        });
    }
    out.sort_by(|a, b| {
        // `/` first, then alphabetical by mount point.
        (a.path.as_os_str() != OsStr::new("/"))
            .cmp(&(b.path.as_os_str() != OsStr::new("/")))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// One mounted filesystem the OS reports.
#[cfg(not(windows))]
struct Mount {
    device: String,
    mount: PathBuf,
    fstype: String,
    /// Sizes known up front (macOS `statfs`); otherwise `statvfs` at use.
    space: Option<(u64, u64)>,
}

/// Usable capacity of the filesystem `path` lives on, via `statvfs`.
/// `None` when the OS cannot stat it.
#[cfg(unix)]
fn fs_space(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `cpath` is a valid NUL-terminated path and `st` a valid
    // out-pointer for the duration of the call.
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let block = st.f_frsize as u64;
    Some((st.f_blocks as u64 * block, st.f_bavail as u64 * block))
}

/// Mounts the drive picker must never show: kernel pseudo-filesystems,
/// sandboxes and container overlays. Real `tmpfs` mounts are skipped too;
/// they hold no disk data and are reachable via "Scan a folder".
#[cfg(not(windows))]
fn is_pseudo_fstype(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "cgroup"
            | "cgroup2"
            | "devpts"
            | "devtmpfs"
            | "tmpfs"
            | "debugfs"
            | "tracefs"
            | "securityfs"
            | "configfs"
            | "selinuxfs"
            | "fusectl"
            | "pstore"
            | "bpf"
            | "autofs"
            | "mqueue"
            | "hugetlbfs"
            | "binfmt_misc"
            | "rpc_pipefs"
            | "nfsd"
            | "overlay"
            | "squashfs"
            | "nsfs"
            | "devfs"
            | "procfs"
            | "kernfs"
            | "fdescfs"
            | "fuse.gvfsd-fuse"
            | "gvfsd-fuse"
            | "fuse.portal"
            | "portal"
    )
}

/// Mount points below the kernel trees, whatever filesystem they claim.
#[cfg(not(windows))]
fn under_pseudo_root(mount: &PathBuf) -> bool {
    let s = mount.to_string_lossy();
    s == "/proc"
        || s == "/sys"
        || s == "/dev"
        || s.starts_with("/proc/")
        || s.starts_with("/sys/")
        || s.starts_with("/dev/")
}

/// macOS helper volumes (`Preboot`, `VM`, `Update`, …) that are never scan
/// targets. `/System/Volumes/Data` is kept: it is the writable data volume.
#[cfg(not(windows))]
fn is_hidden_system_volume(mount: &PathBuf) -> bool {
    #[cfg(target_os = "macos")]
    {
        let s = mount.to_string_lossy();
        s.starts_with("/System/Volumes/") && s != "/System/Volumes/Data"
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mount;
        false
    }
}

#[cfg(not(windows))]
fn classify(fstype: &str, mount: &str) -> DriveKind {
    match fstype {
        "nfs" | "nfs4" | "smbfs" | "cifs" | "smb2" | "afpfs" | "afs" | "ceph" | "fuse.sshfs" => {
            DriveKind::Network
        }
        "iso9660" | "udf" | "cd9660" => DriveKind::Optical,
        "tmpfs" | "ramfs" => DriveKind::Ram,
        _ if mount.starts_with("/Volumes/")
            || mount.starts_with("/media/")
            || mount.starts_with("/run/media/")
            || mount.starts_with("/mnt/") =>
        {
            DriveKind::Removable
        }
        _ => DriveKind::Fixed,
    }
}

#[cfg(not(windows))]
fn label_for(device: &str, mount: &Path) -> String {
    if mount.as_os_str() == OsStr::new("/") {
        return "Root".into();
    }
    mount
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| device.rsplit('/').next().unwrap_or(device).to_string())
}

/// Real mount points from `/proc/mounts` (falling back to `/etc/mtab`).
/// Pure `std`: the format is `device mount fstype options dump pass` with
/// spaces and other special bytes octal-escaped (`\040`).
#[cfg(target_os = "linux")]
fn mounts() -> Vec<Mount> {
    let text = std::fs::read_to_string("/proc/mounts")
        .or_else(|_| std::fs::read_to_string("/etc/mtab"))
        .unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(m) = parse_mounts_line(line) {
            // Bind mounts list the same target twice; the picker shows it once.
            if !out.iter().any(|e: &Mount| e.mount == m.mount) {
                out.push(m);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn parse_mounts_line(line: &str) -> Option<Mount> {
    let mut parts = line.split_whitespace();
    let (device, mount, fstype) = (parts.next()?, parts.next()?, parts.next()?);
    Some(Mount {
        device: unescape_mnt(device),
        mount: PathBuf::from(unescape_mnt(mount)),
        fstype: fstype.to_string(),
        space: None,
    })
}

/// Decode the octal escapes `/proc/mounts` uses (`\040` space, `\012`
/// newline, `\134` backslash, `\011` tab).
#[cfg(target_os = "linux")]
fn unescape_mnt(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let code: String = chars.clone().take(3).collect();
            if code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(n) = u32::from_str_radix(&code, 8) {
                    if let Some(ch) = char::from_u32(n) {
                        out.push(ch);
                        chars.nth(2);
                        continue;
                    }
                }
            }
            out.push('\\');
        } else {
            out.push(c);
        }
    }
    out
}

/// Real mount points from `getmntinfo(3)`, with sizes straight from the
/// returned `statfs` entries (no per-mount `statvfs` round trip).
#[cfg(target_os = "macos")]
fn mounts() -> Vec<Mount> {
    fn cstr(buf: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    let mut out = Vec::new();
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: on success `buf` points to `count` valid `statfs` entries
    // owned by the system; they must not be freed.
    let count = unsafe { libc::getmntinfo(&mut buf, 0) };
    if count <= 0 || buf.is_null() {
        return out;
    }
    for i in 0..count as isize {
        // SAFETY: `i` is within the `count` entries from the call above.
        let fs = unsafe { &*buf.offset(i) };
        let mount = PathBuf::from(cstr(&fs.f_mntonname));
        if out.iter().any(|e: &Mount| e.mount == mount) {
            continue;
        }
        let block = fs.f_bsize.max(0) as u64;
        if block == 0 {
            continue;
        }
        out.push(Mount {
            device: cstr(&fs.f_mntfromname),
            mount,
            fstype: cstr(&fs.f_fstypename),
            space: Some((fs.f_blocks as u64 * block, fs.f_bavail as u64 * block)),
        });
    }
    out
}

/// Other Unixes without a dedicated enumerator: the root only. Sizes come
/// from `statvfs` in the shared `list()` above.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn mounts() -> Vec<Mount> {
    vec![Mount {
        device: String::new(),
        mount: PathBuf::from("/"),
        fstype: String::new(),
        space: None,
    }]
}

/// Non-Unix, non-Windows fallback: no `statvfs`, so no sizes.
#[cfg(not(any(windows, unix)))]
fn mounts() -> Vec<Mount> {
    vec![Mount {
        device: String::new(),
        mount: PathBuf::from("/"),
        fstype: String::new(),
        space: None,
    }]
}

/// `statvfs` does not exist off Unix; every such mount reports zeros.
#[cfg(not(any(windows, unix)))]
fn fs_space(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_never_returns_blank_paths() {
        for drive in list() {
            assert!(!drive.path.as_os_str().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn list_finds_at_least_the_root() {
        // The root fallback guarantees a non-empty list on Unix.
        let drives = list();
        assert!(!drives.is_empty());
        assert!(drives.iter().any(|d| d.path == PathBuf::from("/")));
        // `/` sorts first.
        assert_eq!(drives[0].path, PathBuf::from("/"));
    }

    #[cfg(not(windows))]
    #[test]
    fn pseudo_filesystems_are_skipped() {
        for fs in [
            "proc",
            "sysfs",
            "cgroup",
            "cgroup2",
            "devpts",
            "devtmpfs",
            "tmpfs",
            "debugfs",
            "tracefs",
            "securityfs",
            "autofs",
            "overlay",
            "squashfs",
            "nsfs",
            "devfs",
        ] {
            assert!(is_pseudo_fstype(fs), "{fs} should be skipped");
        }
        for fs in [
            "ext4", "xfs", "btrfs", "apfs", "hfs", "ntfs", "vfat", "zfs", "nfs",
        ] {
            assert!(!is_pseudo_fstype(fs), "{fs} should be listed");
        }
        assert!(under_pseudo_root(&PathBuf::from("/proc/bus")));
        assert!(under_pseudo_root(&PathBuf::from("/sys")));
        assert!(!under_pseudo_root(&PathBuf::from("/")));
        assert!(!under_pseudo_root(&PathBuf::from("/mnt/data")));
    }

    #[cfg(not(windows))]
    #[test]
    fn kinds_cover_network_optical_and_removable() {
        assert_eq!(classify("nfs", "/mnt/nas"), DriveKind::Network);
        assert_eq!(classify("cifs", "/mnt/share"), DriveKind::Network);
        assert_eq!(classify("iso9660", "/media/cd"), DriveKind::Optical);
        assert_eq!(classify("udf", "/media/dvd"), DriveKind::Optical);
        assert_eq!(classify("vfat", "/media/usb"), DriveKind::Removable);
        assert_eq!(
            classify("exfat", "/run/media/user/disk"),
            DriveKind::Removable
        );
        assert_eq!(classify("ext4", "/"), DriveKind::Fixed);
        assert_eq!(classify("apfs", "/"), DriveKind::Fixed);
        assert_eq!(label_for("/dev/sda1", &PathBuf::from("/")), "Root");
        assert_eq!(label_for("/dev/sdb1", &PathBuf::from("/mnt/data")), "data");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mounts_lines_parse_with_octal_escapes() {
        let m = parse_mounts_line("/dev/sda1 / ext4 rw,relatime 0 1").unwrap();
        assert_eq!(m.device, "/dev/sda1");
        assert_eq!(m.mount, PathBuf::from("/"));
        assert_eq!(m.fstype, "ext4");
        let m = parse_mounts_line("/dev/sdb1 /media/user/My\\040Disk exfat rw,nosuid 0 0").unwrap();
        assert_eq!(m.mount, PathBuf::from("/media/user/My Disk"));
        assert!(parse_mounts_line("garbage").is_none());
        assert!(parse_mounts_line("").is_none());
        assert_eq!(unescape_mnt("a\\134b\\011c\\012"), "a\\b\tc\n");
    }
}

//! Enumerate mounted volumes with their capacity.

use std::path::PathBuf;

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
        let letter = self.path.to_string_lossy().trim_end_matches('\\').to_string();
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
    vec![Drive {
        path: PathBuf::from("/"),
        label: "Root".into(),
        fs: String::new(),
        kind: DriveKind::Fixed,
        total: 0,
        free: 0,
    }]
}

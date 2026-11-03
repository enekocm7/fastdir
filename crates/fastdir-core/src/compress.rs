//! NTFS compression and CompactOS / WOF actions.
//!
//! NTFS compression (`LZNT1`) is applied with `FSCTL_SET_COMPRESSION`. The
//! CompactOS algorithms (`XPRESS4K`, `XPRESS8K`, `XPRESS16K`, `LZX`) go through
//! WOF (`FSCTL_SET_EXTERNAL_BACKING`), which is what `compact.exe /EXE:` uses.
//!
//! Both are Windows + NTFS only: [`compression_supported`] is the single place
//! the UI asks, and every entry point here refuses unsupported volumes instead
//! of failing per file.

use std::path::{Path, PathBuf};

use crate::fsops::OpProgress;

/// How a file is compressed on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    /// Classic NTFS compression, also used for the folder "inherit to new
    /// files" flag.
    Lznt1,
    Xpress4K,
    Xpress8K,
    Xpress16K,
    Lzx,
}

/// The algorithms offered by the UI, in display order.
pub const ALGORITHMS: &[Algorithm] = &[
    Algorithm::Lznt1,
    Algorithm::Xpress4K,
    Algorithm::Xpress8K,
    Algorithm::Xpress16K,
    Algorithm::Lzx,
];

impl Algorithm {
    /// The default for files: fast decompression and a good size ratio.
    pub const DEFAULT: Self = Self::Xpress8K;

    /// Stable id for localization and persisted choices.
    pub fn id(self) -> &'static str {
        match self {
            Self::Lznt1 => "lznt1",
            Self::Xpress4K => "xpress4k",
            Self::Xpress8K => "xpress8k",
            Self::Xpress16K => "xpress16k",
            Self::Lzx => "lzx",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "lznt1" => Some(Self::Lznt1),
            "xpress4k" => Some(Self::Xpress4K),
            "xpress8k" => Some(Self::Xpress8K),
            "xpress16k" => Some(Self::Xpress16K),
            "lzx" => Some(Self::Lzx),
            _ => None,
        }
    }

    /// The `FILE_PROVIDER_COMPRESSION_*` value WOF expects, `None` for NTFS
    /// compression.
    pub fn wof_id(self) -> Option<u32> {
        match self {
            Self::Lznt1 => None,
            Self::Xpress4K => Some(0),
            Self::Xpress8K => Some(2),
            Self::Xpress16K => Some(3),
            Self::Lzx => Some(1),
        }
    }

    /// A deliberately conservative guess for the dialog's estimate. Real
    /// ratios depend on the data; this is only shown as "about".
    pub fn estimated_ratio(self) -> f64 {
        match self {
            Self::Lznt1 => 0.30,
            Self::Xpress4K => 0.25,
            Self::Xpress8K => 0.35,
            Self::Xpress16K => 0.40,
            Self::Lzx => 0.50,
        }
    }
}

/// What to compress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressOptions {
    pub algorithm: Algorithm,
    /// Set the NTFS compression flag on folders too, so files created there
    /// later are compressed with LZNT1.
    pub folders_too: bool,
    /// Leave archives, media and other already-compressed formats alone.
    pub skip_poorly_compressed: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::DEFAULT,
            folders_too: true,
            skip_poorly_compressed: true,
        }
    }
}

/// Extensions that are already compressed and would hardly shrink again.
pub const POORLY_COMPRESSED_EXTS: &[&str] = &[
    "7z", "aac", "apk", "avi", "avif", "bz2", "cab", "docx", "flac", "gif", "gz", "heic", "jpeg",
    "jpg", "jxl", "m4a", "m4v", "mkv", "mov", "mp3", "mp4", "mpeg", "mpg", "msi", "ogg", "opus",
    "png", "pptx", "rar", "webm", "webp", "xlsx", "xz", "zip", "zst",
];

/// Whether `path` looks like a format that compression cannot shrink much.
pub fn poorly_compressed(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            POORLY_COMPRESSED_EXTS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
}

/// What a compression / decompression pass did to one selection.
#[derive(Debug, Default)]
pub struct CompressReport {
    /// Paths that failed, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Files skipped because they are already compressed.
    pub skipped: u64,
    /// Files whose compression state was changed.
    pub files: u64,
    /// The operation was cancelled before the next item.
    pub cancelled: bool,
}

impl CompressReport {
    pub fn complete(&self) -> bool {
        self.failed.is_empty() && !self.cancelled
    }
}

/// Whether this volume supports the compression actions. False off Windows
/// and on FAT / exFAT / ReFS / network share volumes.
#[cfg(not(windows))]
pub fn compression_supported(_path: &Path) -> bool {
    false
}

#[cfg(windows)]
pub fn compression_supported(path: &Path) -> bool {
    volume_file_system(path).is_some_and(|fs| fs.eq_ignore_ascii_case("NTFS"))
}

/// Compress `paths` (files or folders) recursively, reporting progress and
/// honouring cancellation between items. Failures are collected and the pass
/// keeps going.
pub fn compress_tracked(
    paths: &[PathBuf],
    options: CompressOptions,
    progress: &OpProgress,
) -> CompressReport {
    let mut report = CompressReport::default();
    for path in paths {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        walk(path, options, progress, &mut report, true);
        if report.cancelled {
            break;
        }
    }
    report
}

/// Remove compression and WOF backing below `paths`.
pub fn uncompress_tracked(paths: &[PathBuf], progress: &OpProgress) -> CompressReport {
    let mut report = CompressReport::default();
    for path in paths {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        walk(
            path,
            CompressOptions::default(),
            progress,
            &mut report,
            false,
        );
        if report.cancelled {
            break;
        }
    }
    report
}

/// Depth-first walk. Symlinks and junctions are not followed, like the scanner.
fn walk(
    path: &Path,
    options: CompressOptions,
    progress: &OpProgress,
    report: &mut CompressReport,
    compress: bool,
) {
    if progress.is_cancelled() {
        report.cancelled = true;
        return;
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.failed.push((path.to_path_buf(), e.to_string()));
            return;
        }
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        if compress && options.folders_too {
            if let Err(e) = set_folder_compression(path, true) {
                report.failed.push((path.to_path_buf(), e));
            }
        } else if !compress {
            // A folder that cannot be opened is reported with its files below.
            let _ = set_folder_compression(path, false);
        }
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => walk(&entry.path(), options, progress, report, compress),
                        Err(e) => report.failed.push((path.to_path_buf(), e.to_string())),
                    }
                    if report.cancelled {
                        return;
                    }
                }
            }
            Err(e) => report.failed.push((path.to_path_buf(), e.to_string())),
        }
        progress.add(1, 0);
        return;
    }
    if !meta.is_file() {
        return;
    }
    // Counted before the work so a skip still moves the progress bar.
    let bytes = meta.len();
    if compress && options.skip_poorly_compressed && poorly_compressed(path) {
        report.skipped += 1;
        progress.add(1, bytes);
        return;
    }
    let result = if compress {
        set_file_compression(path, options.algorithm)
    } else {
        uncompress_file(path)
    };
    match result {
        Ok(()) => report.files += 1,
        Err(e) => report.failed.push((path.to_path_buf(), e)),
    }
    progress.add(1, bytes);
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn volume_file_system(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut root = vec![0u16; 261];
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return None;
    }
    let mut fs = vec![0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some(String::from_utf16_lossy(&fs[..len]))
}

/// `\\?\`-prefixed UTF-16 path, so paths beyond `MAX_PATH` work.
#[cfg(windows)]
fn wide_z(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let mut wide: Vec<u16> = Vec::with_capacity(raw.len() + 8);
    if !raw.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        wide.extend(r"\\?\".encode_utf16());
    }
    wide.extend(raw);
    wide.push(0);
    wide
}

/// Open a file or folder for an `FSCTL_*` call.
#[cfg(windows)]
fn open_handle(path: &Path) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide = wide_z(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(handle)
}

#[cfg(windows)]
fn close_handle(handle: windows_sys::Win32::Foundation::HANDLE) {
    use windows_sys::Win32::Foundation::CloseHandle;
    unsafe {
        CloseHandle(handle);
    }
}

/// Turn the last Win32 error into a readable message.
#[cfg(windows)]
fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}

/// Send an `FSCTL_*` control code to an open handle.
#[cfg(windows)]
fn ioctl(
    handle: windows_sys::Win32::Foundation::HANDLE,
    code: u32,
    input: &[u8],
) -> Result<(), String> {
    use std::ffi::c_void;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            code,
            input.as_ptr() as *const c_void,
            input.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

#[cfg(windows)]
fn set_file_compression(path: &Path, algorithm: Algorithm) -> Result<(), String> {
    let handle = open_handle(path)?;
    let result = match algorithm.wof_id() {
        Some(algorithm) => set_wof(handle, algorithm),
        None => set_lznt1(handle, true),
    };
    close_handle(handle);
    result
}

#[cfg(windows)]
fn set_folder_compression(path: &Path, compressed: bool) -> Result<(), String> {
    let handle = open_handle(path)?;
    let result = set_lznt1(handle, compressed);
    close_handle(handle);
    result
}

#[cfg(windows)]
fn uncompress_file(path: &Path) -> Result<(), String> {
    let handle = open_handle(path)?;
    // WOF files have no NTFS compression flag; NTFS-compressed files have no
    // external backing. Try both and ignore the "not in that state" errors.
    let wof = delete_wof(handle);
    let lznt1 = set_lznt1(handle, false);
    close_handle(handle);
    match (wof, lznt1) {
        (Ok(()), _) | (_, Ok(())) => Ok(()),
        (Err(wof), Err(lznt1)) => Err(format!("{lznt1} (WOF: {wof})")),
    }
}

#[cfg(windows)]
fn set_lznt1(
    handle: windows_sys::Win32::Foundation::HANDLE,
    compressed: bool,
) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::{
        COMPRESSION_FORMAT_LZNT1, COMPRESSION_FORMAT_NONE,
    };
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_COMPRESSION;

    let format = if compressed {
        COMPRESSION_FORMAT_LZNT1
    } else {
        COMPRESSION_FORMAT_NONE
    };
    ioctl(handle, FSCTL_SET_COMPRESSION, &format.to_ne_bytes())
}

/// Input for `FSCTL_SET_EXTERNAL_BACKING` with the WOF file provider:
/// `WOF_EXTERNAL_INFO` followed by `FILE_PROVIDER_EXTERNAL_INFO_V1`.
#[cfg(windows)]
#[repr(C)]
struct WofSetCompression {
    /// `WOF_EXTERNAL_INFO::Version`
    wof_version: u32,
    /// `WOF_EXTERNAL_INFO::Provider`
    provider: u32,
    /// `FILE_PROVIDER_EXTERNAL_INFO_V1::Version`
    file_version: u32,
    algorithm: u32,
    flags: u32,
}

#[cfg(windows)]
fn set_wof(handle: windows_sys::Win32::Foundation::HANDLE, algorithm: u32) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::WOF_PROVIDER_FILE;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_EXTERNAL_BACKING;

    // `WOF_CURRENT_VERSION` and `FILE_PROVIDER_CURRENT_VERSION` are both 1.
    let info = WofSetCompression {
        wof_version: 1,
        provider: WOF_PROVIDER_FILE,
        file_version: 1,
        algorithm,
        flags: 0,
    };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            &info as *const WofSetCompression as *const u8,
            std::mem::size_of::<WofSetCompression>(),
        )
    };
    ioctl(handle, FSCTL_SET_EXTERNAL_BACKING, bytes)
}

#[cfg(windows)]
fn delete_wof(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_DELETE_EXTERNAL_BACKING;

    // FSCTL_DELETE_EXTERNAL_BACKING takes no input at all.
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_DELETE_EXTERNAL_BACKING,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    // A file that is not WOF-backed is already in the requested state.
    // ERROR_OBJECT_NOT_EXTERNALLY_BACKED (0x8906).
    if code == 0x8906 {
        return Ok(());
    }
    Err(std::io::Error::from_raw_os_error(code as i32).to_string())
}

#[cfg(not(windows))]
fn set_file_compression(_path: &Path, _algorithm: Algorithm) -> Result<(), String> {
    Err("Compression is only available on Windows / NTFS".into())
}

#[cfg(not(windows))]
fn set_folder_compression(_path: &Path, _compressed: bool) -> Result<(), String> {
    Err("Compression is only available on Windows / NTFS".into())
}

#[cfg(not(windows))]
fn uncompress_file(_path: &Path) -> Result<(), String> {
    Err("Compression is only available on Windows / NTFS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fastdir-compress-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn algorithm_ids_round_trip() {
        for algorithm in ALGORITHMS {
            assert_eq!(Algorithm::from_id(algorithm.id()), Some(*algorithm));
            assert!(algorithm.estimated_ratio() > 0.0 && algorithm.estimated_ratio() < 1.0);
        }
        assert_eq!(Algorithm::from_id("deflate"), None);
        assert_eq!(Algorithm::DEFAULT, Algorithm::Xpress8K);
    }

    #[test]
    fn already_compressed_extensions_are_recognised() {
        assert!(poorly_compressed(Path::new("holiday.JPG")));
        assert!(poorly_compressed(Path::new("setup.zip")));
        assert!(poorly_compressed(Path::new("clip.mp4")));
        assert!(!poorly_compressed(Path::new("notes.txt")));
        assert!(!poorly_compressed(Path::new("Cargo.toml")));
        assert!(!poorly_compressed(Path::new("no-extension")));
    }

    #[cfg(not(windows))]
    #[test]
    fn compression_is_hidden_off_windows() {
        assert!(!compression_supported(Path::new("/tmp")));
    }

    #[cfg(windows)]
    #[test]
    fn compress_and_uncompress_a_file_in_place() {
        let dir = temp_dir();
        if !compression_supported(&dir) {
            // The temp volume is not NTFS (rare on Windows); nothing to test.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let file = dir.join("report.txt");
        let data: String = "FastDir compression test line.\n".repeat(40_000);
        std::fs::write(&file, data.as_bytes()).unwrap();
        let before = crate::scanner::allocated_size(&file);
        let logical = std::fs::metadata(&file).unwrap().len();

        let progress = OpProgress::default();
        let report = compress_tracked(
            std::slice::from_ref(&file),
            CompressOptions {
                algorithm: Algorithm::Xpress8K,
                folders_too: false,
                skip_poorly_compressed: true,
            },
            &progress,
        );
        assert!(report.complete(), "{report:?}");
        assert_eq!(report.files, 1);
        let compressed = crate::scanner::allocated_size(&file);
        assert!(
            compressed < before,
            "compressed {compressed} should be smaller than {before} (logical {logical})"
        );

        let report = uncompress_tracked(std::slice::from_ref(&file), &OpProgress::default());
        assert!(report.complete(), "{report:?}");
        let restored = crate::scanner::allocated_size(&file);
        assert!(
            restored >= logical,
            "restored {restored} should be at least the logical size {logical}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn poor_files_are_skipped_and_cancelling_stops() {
        let dir = temp_dir();
        if !compression_supported(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::write(dir.join("photo.jpg"), vec![7u8; 64 * 1024]).unwrap();
        std::fs::write(dir.join("notes.txt"), vec![7u8; 64 * 1024]).unwrap();

        let progress = OpProgress::default();
        let report = compress_tracked(
            std::slice::from_ref(&dir),
            CompressOptions::default(),
            &progress,
        );
        assert!(report.complete(), "{report:?}");
        assert_eq!(report.skipped, 1);
        assert_eq!(report.files, 1);

        let progress = OpProgress::default();
        progress.cancel();
        let report = uncompress_tracked(std::slice::from_ref(&dir), &progress);
        assert!(report.cancelled);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! In-app updater: asks GitHub Releases for a newer version, downloads the
//! matching asset (verifying its SHA-256) and applies it.
//!
//! Everything the updater creates lives in `%TEMP%\FastDir-update\` or next to
//! the executable as `<exe>.old` / `<exe>.new`, and [`cleanup`] removes all of
//! it on every startup.

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

const LATEST_URL: &str = "https://api.github.com/repos/enekocm7/fastdir/releases/latest";
const SUMS_NAME: &str = "SHA256SUMS.txt";
const UPDATE_DIR: &str = "FastDir-update";

/// Version of the running binary.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn user_agent() -> String {
    format!("FastDir/{CURRENT_VERSION}")
}

// ----------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------

#[derive(Debug)]
pub enum UpdateError {
    /// No network (DNS failure, connection refused, timeout…).
    Offline,
    /// GitHub answered 403 / 429.
    RateLimited,
    Cancelled,
    HashMismatch,
    Other(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("no internet connection"),
            Self::RateLimited => f.write_str("GitHub rate limit reached, try again later"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::HashMismatch => {
                f.write_str("the downloaded file is corrupted (SHA-256 mismatch) and was deleted")
            }
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.to_string())
    }
}

impl From<ureq::Error> for UpdateError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(403 | 429) => Self::RateLimited,
            ureq::Error::StatusCode(404) => Self::Other("release not found".into()),
            ureq::Error::StatusCode(code) => Self::Other(format!("GitHub returned HTTP {code}")),
            ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed => Self::Offline,
            other => Self::Other(other.to_string()),
        }
    }
}

// ----------------------------------------------------------------------
// Release metadata
// ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
}

/// `v1.2.3` / `1.2.3` -> `Version`.
pub fn parse_version(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches(['v', 'V'])).ok()
}

/// True when `candidate` is strictly newer than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(cur), Some(cand)) => cand > cur,
        _ => false,
    }
}

/// Parses the `releases/latest` payload. Drafts, pre-releases and tags that
/// are not valid semver yield `None`.
pub fn parse_release(json: &str) -> Result<Option<Release>, UpdateError> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| UpdateError::Other(format!("bad release data: {e}")))?;
    if raw.draft || raw.prerelease {
        return Ok(None);
    }
    let Some(version) = parse_version(&raw.tag_name) else {
        return Ok(None);
    };
    Ok(Some(Release {
        version,
        notes: raw.body.unwrap_or_default().replace("\r\n", "\n"),
        assets: raw
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
            })
            .collect(),
    }))
}

impl Release {
    pub fn is_newer_than_current(&self) -> bool {
        is_newer(CURRENT_VERSION, &self.version.to_string())
    }

    /// The asset to download for this kind of install.
    pub fn asset_for(&self, kind: InstallKind) -> Option<&Asset> {
        let suffix = match kind {
            InstallKind::Installed { .. } => "-setup-x64.exe",
            InstallKind::Portable => "-portable-x64.zip",
        };
        self.assets
            .iter()
            .find(|a| a.name.starts_with("FastDir-") && a.name.ends_with(suffix))
    }

    fn sums_asset(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == SUMS_NAME)
    }
}

/// Finds the hash for `file` in a `sha256sum`-style listing
/// (`<hex>  <name>` or `<hex> *<name>`).
pub fn hash_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn agent(timeouts: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder()
        .https_only(true)
        .user_agent(user_agent())
        .timeout_connect(Some(Duration::from_secs(15)));
    let builder = if timeouts {
        builder.timeout_global(Some(Duration::from_secs(20)))
    } else {
        builder
    };
    builder.build().into()
}

/// Asks GitHub for the latest stable release. `Ok(None)` means there is no
/// usable release (draft / pre-release / odd tag).
pub fn fetch_latest() -> Result<Option<Release>, UpdateError> {
    let response = match agent(true)
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        // The repo has no published release yet.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    parse_release(&response.into_body().read_to_string()?)
}

// ----------------------------------------------------------------------
// Install kind
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by the Inno Setup installer.
    Installed { all_users: bool },
    /// Loose exe (unzipped portable build).
    Portable,
}

pub fn detect_kind() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Portable;
    };
    let Some(dir) = exe.parent() else {
        return InstallKind::Portable;
    };
    // Inno Setup always drops its uninstaller (`unins000.exe`) in the app dir.
    if !dir.join("unins000.exe").is_file() {
        return InstallKind::Portable;
    }
    let all_users = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|pf| dir.starts_with(pf));
    InstallKind::Installed { all_users }
}

// ----------------------------------------------------------------------
// Cleanup
// ----------------------------------------------------------------------

fn update_dir() -> PathBuf {
    std::env::temp_dir().join(UPDATE_DIR)
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = exe.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_leftovers(exe: Option<&Path>) -> bool {
    let mut clean = true;
    let dir = update_dir();
    if dir.exists() && fs::remove_dir_all(&dir).is_err() {
        clean = false;
    }
    if let Some(exe) = exe {
        for suffix in [".old", ".new"] {
            let p = sibling(exe, suffix);
            if p.exists() && fs::remove_file(&p).is_err() {
                clean = false;
            }
        }
    }
    clean
}

/// Removes everything a previous update left behind. Call first thing on
/// startup. The previous process (or the installer) may still be shutting
/// down, so anything that is still locked is retried in the background.
pub fn cleanup() {
    let exe = std::env::current_exe().ok();
    if remove_leftovers(exe.as_deref()) {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..15 {
            std::thread::sleep(Duration::from_secs(2));
            if remove_leftovers(exe.as_deref()) {
                break;
            }
        }
    });
}

/// Deletes the whole update directory (used after cancel / failure).
pub fn discard_downloads() {
    let _ = fs::remove_dir_all(update_dir());
}

// ----------------------------------------------------------------------
// Download
// ----------------------------------------------------------------------

#[derive(Default)]
pub struct DownloadProgress {
    pub downloaded: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl DownloadProgress {
    /// 0..=100, or `None` while the size is unknown.
    pub fn percent(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.downloaded.load(Ordering::Relaxed) as f32 / total as f32 * 100.).min(100.))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Downloads the asset for `kind` into `%TEMP%\FastDir-update\`, checking its
/// SHA-256 against the release's `SHA256SUMS.txt`. Data goes to a `.part`
/// file that is renamed only after the hash matches. On any failure nothing
/// is left on disk.
pub fn download(release: &Release, kind: InstallKind, progress: &DownloadProgress) -> Result<PathBuf, UpdateError> {
    let result = download_inner(release, kind, progress);
    if result.is_err() {
        discard_downloads();
    }
    result
}

fn download_inner(release: &Release, kind: InstallKind, progress: &DownloadProgress) -> Result<PathBuf, UpdateError> {
    let asset = release
        .asset_for(kind)
        .ok_or_else(|| UpdateError::Other("this release has no download for your install type".into()))?;
    let sums_asset = release
        .sums_asset()
        .ok_or_else(|| UpdateError::Other("this release has no SHA256SUMS.txt".into()))?;

    let http = agent(false);
    let sums = http
        .get(&sums_asset.url)
        .call()?
        .into_body()
        .read_to_string()?;
    let expected = hash_for(&sums, &asset.name)
        .ok_or_else(|| UpdateError::Other(format!("no checksum listed for {}", asset.name)))?;

    let dir = update_dir();
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let final_path = dir.join(&asset.name);
    let part_path = dir.join(format!("{}.part", asset.name));

    let response = http.get(&asset.url).call()?;
    progress
        .total
        .store(response.body().content_length().unwrap_or(0), Ordering::Relaxed);
    progress.downloaded.store(0, Ordering::Relaxed);
    let mut reader = response.into_body().into_with_config().limit(u64::MAX).reader();

    let mut file = File::create(&part_path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| {
            // ureq reports network failures through io::Error.
            UpdateError::Other(format!("download interrupted: {e}"))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        progress.downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush()?;
    drop(file);

    if hex(&hasher.finalize()) != expected {
        return Err(UpdateError::HashMismatch);
    }
    fs::rename(&part_path, &final_path)?;
    Ok(final_path)
}

// ----------------------------------------------------------------------
// Apply
// ----------------------------------------------------------------------

#[cfg(windows)]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    cmd.spawn().map(|_| ())
}

/// Applies the verified download. On `Ok` the caller must exit right away: a
/// new FastDir is (or is about to be) running.
pub fn apply(downloaded: &Path, kind: InstallKind) -> Result<(), UpdateError> {
    match kind {
        InstallKind::Installed { all_users } => {
            let scope = if all_users { "/ALLUSERS" } else { "/CURRENTUSER" };
            spawn_detached(
                std::process::Command::new(downloaded).args([
                    "/VERYSILENT",
                    "/SUPPRESSMSGBOXES",
                    "/NORESTART",
                    "/CLOSEAPPLICATIONS",
                    scope,
                    "/RELAUNCH=1",
                ]),
            )?;
            // The installer runs from the update dir, so that dir is cleaned
            // on the next launch (it can't be deleted while setup is running).
            Ok(())
        }
        InstallKind::Portable => {
            apply_portable(downloaded)?;
            discard_downloads();
            Ok(())
        }
    }
}

fn apply_portable(zip_path: &Path) -> Result<(), UpdateError> {
    let exe = std::env::current_exe()?;
    let new = sibling(&exe, ".new");
    let old = sibling(&exe, ".old");

    extract_exe(zip_path, &new).inspect_err(|_| {
        let _ = fs::remove_file(&new);
    })?;

    let _ = fs::remove_file(&old);
    // Windows lets us rename a running executable.
    fs::rename(&exe, &old).inspect_err(|_| {
        let _ = fs::remove_file(&new);
    })?;
    if let Err(e) = fs::rename(&new, &exe) {
        let _ = fs::rename(&old, &exe);
        let _ = fs::remove_file(&new);
        return Err(e.into());
    }
    if let Err(e) = spawn_detached(&mut std::process::Command::new(&exe)) {
        // Roll back so the user keeps a working copy.
        let _ = fs::remove_file(&exe);
        let _ = fs::rename(&old, &exe);
        return Err(e.into());
    }
    Ok(())
}

/// Writes the `FastDir.exe` entry of the portable zip to `dest`.
fn extract_exe(zip_path: &Path, dest: &Path) -> Result<(), UpdateError> {
    let mut archive = zip::ZipArchive::new(File::open(zip_path)?)
        .map_err(|e| UpdateError::Other(format!("bad zip: {e}")))?;
    let index = (0..archive.len())
        .find(|&i| {
            archive.by_index_raw(i).is_ok_and(|f| {
                f.name().is_ok_and(|name| {
                    Path::new(name.as_ref())
                        .file_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case("fastdir.exe"))
                })
            })
        })
        .ok_or_else(|| UpdateError::Other("FastDir.exe is missing from the zip".into()))?;
    let mut entry = archive
        .by_index(index)
        .map_err(|e| UpdateError::Other(format!("bad zip: {e}")))?;
    let mut out = File::create(dest)?;
    std::io::copy(&mut entry, &mut out)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_json(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(
            r#"{{
                "tag_name": "{tag}", "draft": {draft}, "prerelease": {prerelease},
                "body": "line1\r\nline2",
                "assets": [
                    {{"name": "FastDir-0.2.0-setup-x64.exe", "browser_download_url": "https://x/setup.exe"}},
                    {{"name": "FastDir-0.2.0-portable-x64.zip", "browser_download_url": "https://x/p.zip"}},
                    {{"name": "SHA256SUMS.txt", "browser_download_url": "https://x/sums"}}
                ]
            }}"#
        )
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.1.0", "v0.1.1"));
        assert!(is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("0.9.0", "v0.10.0"));
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("0.2.0", "v0.1.9"));
        assert!(!is_newer("0.1.0", "not-a-version"));
        // A stable release is newer than its own pre-release.
        assert!(is_newer("0.2.0-beta.1", "0.2.0"));
    }

    #[test]
    fn parses_release_and_selects_assets() {
        let rel = parse_release(&release_json("v0.2.0", false, false))
            .unwrap()
            .unwrap();
        assert_eq!(rel.version, Version::new(0, 2, 0));
        assert_eq!(rel.notes, "line1\nline2");
        assert_eq!(
            rel.asset_for(InstallKind::Installed { all_users: false }).unwrap().name,
            "FastDir-0.2.0-setup-x64.exe"
        );
        assert_eq!(
            rel.asset_for(InstallKind::Installed { all_users: true }).unwrap().name,
            "FastDir-0.2.0-setup-x64.exe"
        );
        assert_eq!(
            rel.asset_for(InstallKind::Portable).unwrap().name,
            "FastDir-0.2.0-portable-x64.zip"
        );
        assert_eq!(rel.sums_asset().unwrap().name, "SHA256SUMS.txt");
    }

    #[test]
    fn missing_asset_is_none() {
        let mut rel = parse_release(&release_json("v0.2.0", false, false))
            .unwrap()
            .unwrap();
        rel.assets.retain(|a| !a.name.ends_with(".zip"));
        assert!(rel.asset_for(InstallKind::Portable).is_none());
    }

    #[test]
    fn ignores_drafts_prereleases_and_bad_tags() {
        assert!(parse_release(&release_json("v0.2.0", true, false)).unwrap().is_none());
        assert!(parse_release(&release_json("v0.2.0", false, true)).unwrap().is_none());
        assert!(parse_release(&release_json("nightly", false, false)).unwrap().is_none());
        assert!(parse_release("{").is_err());
    }

    #[test]
    fn parses_sha256sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  FastDir-0.2.0-setup-x64.exe\r\n{b} *FastDir-0.2.0-portable-x64.zip\n\ngarbage\n");
        assert_eq!(hash_for(&sums, "FastDir-0.2.0-setup-x64.exe"), Some(a));
        assert_eq!(hash_for(&sums, "FastDir-0.2.0-portable-x64.zip"), Some("b".repeat(64)));
        assert_eq!(hash_for(&sums, "other.exe"), None);
        assert_eq!(hash_for("abc  FastDir.exe", "FastDir.exe"), None);
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }

    #[test]
    fn sibling_appends_suffix() {
        assert_eq!(
            sibling(Path::new(r"C:\a\FastDir.exe"), ".old"),
            PathBuf::from(r"C:\a\FastDir.exe.old")
        );
    }
}

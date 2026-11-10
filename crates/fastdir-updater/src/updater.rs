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
const RELEASES_URL: &str = "https://api.github.com/repos/enekocm7/fastdir/releases?per_page=20";
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

/// Which releases the update check looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Finished releases only (GitHub's `releases/latest`).
    #[default]
    Stable,
    /// Also offer pre-releases (the newest tag wins, drafts excluded).
    Prerelease,
}

/// Parses the `releases/latest` payload. Drafts, pre-releases and tags that
/// are not valid semver yield `None`.
pub fn parse_release(json: &str) -> Result<Option<Release>, UpdateError> {
    parse_release_inner(json, false)
}

/// Like [`parse_release`], but keeps pre-releases when `allow_prerelease`.
pub fn parse_release_allowing_prerelease(json: &str) -> Result<Option<Release>, UpdateError> {
    parse_release_inner(json, true)
}

fn parse_release_inner(json: &str, allow_prerelease: bool) -> Result<Option<Release>, UpdateError> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| UpdateError::Other(format!("bad release data: {e}")))?;
    if raw.draft || (!allow_prerelease && raw.prerelease) {
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

    /// The asset to download for this kind of install. `None` for installs
    /// FastDir does not update itself (see [`InstallKind::self_updates`]).
    pub fn asset_for(&self, kind: InstallKind) -> Option<&Asset> {
        let suffix = match kind {
            InstallKind::Installed { .. } => "-setup-x64.exe",
            InstallKind::Portable => "-portable-x64.zip",
            InstallKind::AppImage => "-x86_64.AppImage",
            InstallKind::PackageManager | InstallKind::Manual => return None,
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

/// Asks GitHub for the newest release on `channel`. `Ok(None)` means there is
/// no usable release (draft / pre-release on the stable channel / odd tag).
pub fn fetch_channel(channel: Channel) -> Result<Option<Release>, UpdateError> {
    match channel {
        Channel::Stable => fetch_latest(),
        Channel::Prerelease => fetch_latest_including_prereleases(),
    }
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

/// Newest release including pre-releases (drafts still excluded). Used by
/// the "pre-release" update channel.
pub fn fetch_latest_including_prereleases() -> Result<Option<Release>, UpdateError> {
    let response = match agent(true)
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let raws: Vec<RawRelease> = serde_json::from_str(&response.into_body().read_to_string()?)
        .map_err(|e| UpdateError::Other(format!("bad release data: {e}")))?;
    Ok(pick_newest(&raws))
}

/// Newest non-draft release in a `releases` listing, pre-releases included.
fn pick_newest(raws: &[RawRelease]) -> Option<Release> {
    raws.iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = parse_version(&r.tag_name)?;
            Some((
                version.clone(),
                Release {
                    version,
                    notes: r.body.clone().unwrap_or_default().replace("\r\n", "\n"),
                    assets: r
                        .assets
                        .iter()
                        .map(|a| Asset {
                            name: a.name.clone(),
                            url: a.browser_download_url.clone(),
                        })
                        .collect(),
                },
            ))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, release)| release)
}

// ----------------------------------------------------------------------
// Install kind
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by the Inno Setup installer (Windows).
    Installed { all_users: bool },
    /// Loose exe (unzipped Windows portable build).
    Portable,
    /// Linux AppImage: the file at `$APPIMAGE` is replaced in place.
    AppImage,
    /// Owned by a package manager (deb, rpm, AUR, Nix, Flatpak, Snap,
    /// Homebrew). FastDir never overwrites these binaries; the user updates
    /// through the package manager instead.
    PackageManager,
    /// Unpacked Linux / macOS tarball: no self-update, the user downloads the
    /// new release by hand.
    Manual,
}

impl InstallKind {
    /// Whether "Update now" can download and apply the update itself.
    pub fn self_updates(self) -> bool {
        !matches!(self, Self::PackageManager | Self::Manual)
    }
}

#[cfg(not(windows))]
pub fn detect_kind() -> InstallKind {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let appimage = var("APPIMAGE").map(PathBuf::from);
    let sandboxed = var("FLATPAK_ID").is_some() || var("SNAP").is_some();
    let exe = std::env::current_exe()
        .ok()
        .map(|exe| fs::canonicalize(&exe).unwrap_or(exe));
    classify_unix(exe.as_deref(), appimage.as_deref(), sandboxed)
}

/// Install kind of a non-Windows build from where it runs. `appimage` is the
/// `$APPIMAGE` variable the AppImage runtime sets; `sandboxed` is true inside
/// Flatpak or Snap.
#[cfg_attr(windows, allow(dead_code))]
fn classify_unix(exe: Option<&Path>, appimage: Option<&Path>, sandboxed: bool) -> InstallKind {
    if cfg!(target_os = "linux") && appimage.is_some_and(Path::is_absolute) {
        return InstallKind::AppImage;
    }
    if sandboxed {
        return InstallKind::PackageManager;
    }
    let Some(exe) = exe else {
        return InstallKind::Manual;
    };
    // Directories owned by dpkg / rpm / pacman, Nix, Snap and Homebrew.
    // `/usr/local` (outside Homebrew's Cellar) and `/opt` are where people
    // unpack tarballs by hand.
    const MANAGED: [&str; 6] = [
        "/usr",
        "/nix/store",
        "/run/current-system",
        "/snap",
        "/opt/homebrew",
        "/home/linuxbrew/.linuxbrew",
    ];
    let by_hand = exe.starts_with("/usr/local") && !exe.starts_with("/usr/local/Cellar");
    if MANAGED.iter().any(|root| exe.starts_with(root)) && !by_hand {
        InstallKind::PackageManager
    } else {
        InstallKind::Manual
    }
}

#[cfg(windows)]
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

#[cfg(unix)]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    // Own process group, so the new FastDir is not tied to this one.
    cmd.process_group(0).spawn().map(|_| ())
}

#[cfg(not(any(windows, unix)))]
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
        InstallKind::AppImage => {
            let target = std::env::var_os("APPIMAGE")
                .map(PathBuf::from)
                .ok_or_else(|| UpdateError::Other("$APPIMAGE is not set".into()))?;
            replace_appimage(downloaded, &target)?;
            discard_downloads();
            Ok(())
        }
        InstallKind::PackageManager | InstallKind::Manual => Err(UpdateError::Other(
            "this install is not updated by FastDir itself".into(),
        )),
    }
}

/// Swaps the AppImage at `target` for the verified download and starts it.
/// The running AppImage keeps its (now unlinked) file open, so replacing it
/// with a rename is safe; on failure the old file is left untouched.
fn replace_appimage(downloaded: &Path, target: &Path) -> Result<(), UpdateError> {
    // Copy next to the target first: the download sits in the temp dir,
    // often another file system, and only a same-directory rename is atomic.
    let new = sibling(target, ".new");
    let staged = fs::copy(downloaded, &new)
        .and_then(|_| set_executable(&new))
        .and_then(|()| fs::rename(&new, target));
    if let Err(e) = staged {
        let _ = fs::remove_file(&new);
        return Err(e.into());
    }
    spawn_detached(&mut std::process::Command::new(target))?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
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
                    {{"name": "FastDir-0.2.0-linux-x64.tar.gz", "browser_download_url": "https://x/l.tar.gz"}},
                    {{"name": "FastDir-0.2.0-amd64.deb", "browser_download_url": "https://x/f.deb"}},
                    {{"name": "FastDir-0.2.0-x86_64.AppImage", "browser_download_url": "https://x/f.AppImage"}},
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
        assert_eq!(
            rel.asset_for(InstallKind::AppImage).unwrap().name,
            "FastDir-0.2.0-x86_64.AppImage"
        );
        // Packaged and hand-unpacked installs never download anything, in
        // particular not the Windows zip.
        assert!(rel.asset_for(InstallKind::PackageManager).is_none());
        assert!(rel.asset_for(InstallKind::Manual).is_none());
        assert_eq!(rel.sums_asset().unwrap().name, "SHA256SUMS.txt");
    }

    #[test]
    fn self_update_support() {
        assert!(InstallKind::Installed { all_users: true }.self_updates());
        assert!(InstallKind::Portable.self_updates());
        assert!(InstallKind::AppImage.self_updates());
        assert!(!InstallKind::PackageManager.self_updates());
        assert!(!InstallKind::Manual.self_updates());
    }

    #[test]
    fn package_managed_installs_are_never_applied() {
        let path = Path::new("unused");
        assert!(apply(path, InstallKind::PackageManager).is_err());
        assert!(apply(path, InstallKind::Manual).is_err());
    }

    #[test]
    fn classifies_unix_installs() {
        let kind = |exe: &str| classify_unix(Some(Path::new(exe)), None, false);
        // deb / rpm / AUR, Nix, Snap, Homebrew.
        assert_eq!(kind("/usr/bin/fastdir"), InstallKind::PackageManager);
        assert_eq!(
            kind("/usr/lib/fastdir/fastdir"),
            InstallKind::PackageManager
        );
        assert_eq!(
            kind("/nix/store/abc-fastdir-0.3.2/bin/fastdir"),
            InstallKind::PackageManager
        );
        assert_eq!(
            kind("/snap/fastdir/12/bin/fastdir"),
            InstallKind::PackageManager
        );
        assert_eq!(
            kind("/opt/homebrew/bin/fastdir"),
            InstallKind::PackageManager
        );
        assert_eq!(
            kind("/usr/local/Cellar/fastdir/0.3.2/bin/fastdir"),
            InstallKind::PackageManager
        );
        // Unpacked by hand.
        assert_eq!(kind("/usr/local/bin/fastdir"), InstallKind::Manual);
        assert_eq!(kind("/opt/fastdir/FastDir"), InstallKind::Manual);
        assert_eq!(kind("/home/me/Downloads/FastDir"), InstallKind::Manual);
        assert_eq!(kind("/usrlocal/FastDir"), InstallKind::Manual);
        assert_eq!(classify_unix(None, None, false), InstallKind::Manual);
        // Flatpak / Snap sandboxes are package-managed wherever the exe is.
        assert_eq!(
            classify_unix(Some(Path::new("/app/bin/fastdir")), None, true),
            InstallKind::PackageManager
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_wins_over_the_mount_path() {
        // Inside an AppImage the exe lives in the runtime's mount point.
        let exe = Path::new("/tmp/.mount_FastDiXyz/usr/bin/fastdir");
        let image = Path::new("/home/me/Apps/FastDir-0.3.2-x86_64.AppImage");
        assert_eq!(
            classify_unix(Some(exe), Some(image), false),
            InstallKind::AppImage
        );
        assert_eq!(
            classify_unix(Some(exe), Some(Path::new("relative")), false),
            InstallKind::Manual
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaces_appimage_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("fastdir-appimage-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A tiny script stands in for the AppImage so the relaunch succeeds.
        let script = "#!/bin/sh\nexit 0\n";
        let download = dir.join("download.AppImage");
        fs::write(&download, script).unwrap();
        let target = dir.join("FastDir.AppImage");
        fs::write(&target, "old").unwrap();
        replace_appimage(&download, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), script);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!sibling(&target, ".new").exists());
        let _ = fs::remove_dir_all(&dir);
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
    fn prerelease_channel_picks_the_newest_tag() {
        // The stable parser still rejects pre-releases…
        assert!(
            parse_release(&release_json("v0.3.0-beta.1", false, true))
                .unwrap()
                .is_none()
        );
        // …while the opt-in parser keeps them.
        assert!(
            parse_release_allowing_prerelease(&release_json("v0.3.0-beta.1", false, true))
                .unwrap()
                .is_some()
        );
        let list = format!(
            "[{}, {}, {}]",
            release_json("v0.2.0", false, false),
            release_json("v0.3.0-beta.1", false, true),
            release_json("v9.9.9", true, false),
        );
        let raws: Vec<RawRelease> = serde_json::from_str(&list).unwrap();
        let newest = pick_newest(&raws).unwrap();
        // The draft ( however high its tag) loses to the beta.
        assert_eq!(newest.version, Version::parse("0.3.0-beta.1").unwrap());
        assert!(pick_newest(&[]).is_none());
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

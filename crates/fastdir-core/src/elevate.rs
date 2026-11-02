//! Elevation helpers: detect whether the process runs as administrator and
//! relaunch itself elevated.
//!
//! Scanning `C:\` without elevation skips protected folders (access denied),
//! so those bytes are missing from the totals. The UI offers a
//! "Restart as administrator" button that relaunches through `ShellExecuteW`
//! with the `runas` verb, passing the current path so the elevated instance
//! can resume the same scan. A future MFT scanner needs the same helper.

use std::path::Path;

/// Whether the current process runs elevated.
///
/// On Windows this checks `IsUserAnAdmin`. Any failure to query is treated
/// as "not elevated" so the UI offers the restart button. On other platforms
/// there is no UAC concept, so this returns `true` to hide the button.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        // SAFETY: `IsUserAnAdmin` has no preconditions.
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Command-line argument forwarded to the elevated instance.
///
/// `None` when no path should be passed (drive overview). Otherwise the path
/// is quoted so `C:\Program Files\…` survives as a single argument; the
/// existing startup parsing in `fastdir-ui` already strips one layer of
/// quotes and accepts a bare `C:` drive form.
pub fn relaunch_args(path: Option<&Path>) -> Option<String> {
    path.map(|p| format!("\"{}\"", p.display()))
}

/// Relaunch the current executable elevated via `ShellExecuteW` + `runas`.
///
/// On success the new elevated process is starting; the caller should exit
/// (`cx.quit()` in the UI) so only one instance keeps scanning.
#[cfg(windows)]
pub fn restart_as_admin(path: Option<&Path>) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args = relaunch_args(path);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[cfg(windows)]
    fn wide_os(s: &std::ffi::OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    let verb = wide("runas");
    let file = wide_os(exe.as_os_str());
    let params = args.as_deref().map(wide);

    // SAFETY: all pointers are valid null-terminated UTF-16 for the call.
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // `ShellExecuteW` returns a value > 32 on success.
    if (rc as isize) > 32 {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    if code == 1223 {
        // ERROR_CANCELLED: the user dismissed the UAC prompt.
        return Err("Elevation was cancelled".into());
    }
    Err(format!("Could not restart as administrator (error {code})"))
}

/// Non-Windows stub: elevation via `runas` does not exist.
#[cfg(not(windows))]
pub fn restart_as_admin(_path: Option<&Path>) -> Result<(), String> {
    Err("Restart as administrator is only supported on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_args_quotes_the_path() {
        assert_eq!(relaunch_args(None), None);
        let p = Path::new(r"C:\Program Files\FastDir");
        assert_eq!(
            relaunch_args(Some(p)).as_deref(),
            Some(r#""C:\Program Files\FastDir""#)
        );
    }

    #[test]
    fn is_elevated_does_not_panic() {
        // Only asserts the query itself is safe to call.
        let _ = is_elevated();
    }

    #[test]
    #[cfg(not(windows))]
    fn restart_is_unsupported_off_windows() {
        assert!(restart_as_admin(None).is_err());
    }
}

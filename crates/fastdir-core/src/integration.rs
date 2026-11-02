//! Single-instance forwarding and Explorer context-menu integration.
//!
//! The TCP loopback protocol is portable (`std` only). The context menu is
//! Windows-only: it mirrors the installer's `Directory`/`Drive` shell keys
//! under HKCU so the in-app toggle matches a per-user install exactly.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
#[cfg(windows)]
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
};

/// Loopback port the primary instance listens on.
pub const SINGLE_INSTANCE_PORT: u16 = 47831;

const BANNER: &[u8] = b"FastDir/1\n";
const BANNER_LEN: usize = 10;
const MAX_MSG_BYTES: usize = 32 * 1024;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Result of attempting to become the primary instance.
pub enum Claim {
    /// This process owns the port; keep the listener alive.
    Primary(TcpListener),
    /// Another instance already owns the port.
    Unavailable,
}

/// Bind `127.0.0.1:port`. The bind itself is the lock.
pub fn try_claim(port: u16) -> Claim {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => Claim::Primary(listener),
        Err(_) => Claim::Unavailable,
    }
}

/// Frame an open request: `u32` little-endian length + UTF-8 path bytes.
/// An empty payload means `None` (plain launch).
pub fn encode_open_request(path: Option<&Path>) -> Vec<u8> {
    let bytes: Vec<u8> = match path {
        Some(p) => p.as_os_str().to_string_lossy().into_owned().into_bytes(),
        None => Vec::new(),
    };
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
    out
}

/// Decode one framed message; errors on overlong/truncated/invalid data.
pub fn decode_open_request(msg: &[u8]) -> io::Result<Option<PathBuf>> {
    if msg.len() < 4 {
        return Err(invalid("truncated open request"));
    }
    let len = u32::from_le_bytes([msg[0], msg[1], msg[2], msg[3]]) as usize;
    if len > MAX_MSG_BYTES {
        return Err(invalid("open request too large"));
    }
    if msg.len() != 4 + len {
        return Err(invalid("truncated open request"));
    }
    if len == 0 {
        return Ok(None);
    }
    let s =
        std::str::from_utf8(&msg[4..]).map_err(|_| invalid("open request is not valid UTF-8"))?;
    Ok(Some(PathBuf::from(s)))
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Option<PathBuf>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MSG_BYTES {
        return Err(invalid("open request too large"));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    let mut msg = Vec::with_capacity(4 + len);
    msg.extend_from_slice(&len_buf);
    msg.extend_from_slice(&payload);
    decode_open_request(&msg)
}

/// Forward an open request to the primary instance (banner handshake first).
pub fn forward_open_request(port: u16, path: Option<&Path>) -> io::Result<()> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut banner = [0u8; BANNER_LEN];
    stream
        .read_exact(&mut banner)
        .map_err(|_| invalid("single-instance handshake failed"))?;
    if banner.as_slice() != BANNER {
        return Err(invalid("single-instance handshake mismatch"));
    }
    stream.write_all(&encode_open_request(path))?;
    Ok(())
}

/// Spawn a detached accept loop: per connection, write the banner, read one
/// framed message and forward it to `tx`. Ends when the receiver is dropped.
pub fn serve_requests(listener: TcpListener, tx: mpsc::Sender<Option<PathBuf>>) {
    thread::spawn(move || {
        for conn in listener.incoming() {
            let mut stream = match conn {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            if stream.write_all(BANNER).is_err() {
                continue;
            }
            let request = match read_frame(&mut stream) {
                Ok(request) => request,
                Err(_) => continue,
            };
            if tx.send(request).is_err() {
                break;
            }
        }
    });
}

/// Where the Explorer context menu entry currently comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextMenuState {
    /// Both per-user (`HKCU`) command keys exist.
    PerUser,
    /// A machine-wide (`HKLM`) command key exists.
    Machine,
    /// No entry found.
    Absent,
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
struct Key(HKEY);

#[cfg(windows)]
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

#[cfg(windows)]
fn key_exists(hive: HKEY, subkey: &str) -> bool {
    let sub = wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(hive, sub.as_ptr(), 0, KEY_READ, &mut hkey) };
    if status == ERROR_SUCCESS {
        unsafe {
            RegCloseKey(hkey);
        }
        true
    } else {
        false
    }
}

#[cfg(windows)]
fn create_key(hive: HKEY, subkey: &str) -> io::Result<Key> {
    let sub = wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            hive,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if status == ERROR_SUCCESS {
        Ok(Key(hkey))
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(windows)]
fn set_sz(hkey: HKEY, name: Option<&str>, value: &str) -> io::Result<()> {
    let data = wide(value);
    let cb = (data.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe {
        match name {
            Some(n) => {
                let name_w = wide(n);
                RegSetValueExW(
                    hkey,
                    name_w.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    cb,
                )
            }
            None => RegSetValueExW(
                hkey,
                std::ptr::null(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                cb,
            ),
        }
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(windows)]
fn set_at(base: &str, exe: &str, enabled: bool) -> io::Result<()> {
    const KINDS: [&str; 2] = ["Directory", "Drive"];
    if enabled {
        let label = "Analyze with FastDir";
        let icon = format!("\"{exe}\",0");
        let command = format!("\"{exe}\" \"%1\"");
        for kind in KINDS {
            let shell = format!("{base}\\{kind}\\shell\\FastDir");
            let cmd = format!("{shell}\\command");
            let shell_key = create_key(HKEY_CURRENT_USER, &shell)?;
            set_sz(shell_key.0, None, label)?;
            set_sz(shell_key.0, Some("Icon"), &icon)?;
            let cmd_key = create_key(HKEY_CURRENT_USER, &cmd)?;
            set_sz(cmd_key.0, None, &command)?;
        }
        Ok(())
    } else {
        for kind in KINDS {
            let tree = wide(&format!("{base}\\{kind}\\shell\\FastDir"));
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, tree.as_ptr()) };
            if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn state_at(base: &str) -> ContextMenuState {
    let dir_cmd = format!("{base}\\Directory\\shell\\FastDir\\command");
    let drive_cmd = format!("{base}\\Drive\\shell\\FastDir\\command");
    if key_exists(HKEY_CURRENT_USER, &dir_cmd) && key_exists(HKEY_CURRENT_USER, &drive_cmd) {
        ContextMenuState::PerUser
    } else if key_exists(HKEY_LOCAL_MACHINE, &dir_cmd)
        || key_exists(HKEY_LOCAL_MACHINE, &drive_cmd)
    {
        ContextMenuState::Machine
    } else {
        ContextMenuState::Absent
    }
}

#[cfg(windows)]
const REAL_BASE: &str = "Software\\Classes";

/// Report where the Explorer context menu entry comes from (Windows).
#[cfg(windows)]
pub fn context_menu_state() -> ContextMenuState {
    state_at(REAL_BASE)
}

/// Create or remove the per-user Explorer context menu entries (Windows).
#[cfg(windows)]
pub fn set_context_menu(enabled: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    set_at(REAL_BASE, &exe.to_string_lossy(), enabled)
}

/// The Explorer context menu is Windows-only.
#[cfg(not(windows))]
pub fn context_menu_state() -> ContextMenuState {
    ContextMenuState::Absent
}

/// The Explorer context menu is Windows-only.
#[cfg(not(windows))]
pub fn set_context_menu(_enabled: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the Explorer context menu is Windows-only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_roundtrip() {
        assert_eq!(decode_open_request(&encode_open_request(None)).unwrap(), None);

        let ascii = PathBuf::from(r"C:\Users\Test\Documents");
        assert_eq!(
            decode_open_request(&encode_open_request(Some(&ascii))).unwrap(),
            Some(ascii)
        );

        let non_ascii = PathBuf::from("C:\\Users\\Tëst\\données\\文件");
        assert_eq!(
            decode_open_request(&encode_open_request(Some(&non_ascii))).unwrap(),
            Some(non_ascii)
        );

        // Overlong declared length.
        let big = vec![b'a'; MAX_MSG_BYTES + 1];
        let mut overlong = (big.len() as u32).to_le_bytes().to_vec();
        overlong.extend_from_slice(&big);
        assert!(decode_open_request(&overlong).is_err());

        // Truncated payloads.
        let full = encode_open_request(Some(Path::new(r"C:\foo")));
        assert!(decode_open_request(&full[..full.len() - 1]).is_err());
        assert!(decode_open_request(&full[..2]).is_err());
        assert!(decode_open_request(&[]).is_err());

        // Invalid UTF-8.
        let mut bad = 2u32.to_le_bytes().to_vec();
        bad.extend_from_slice(&[0xFF, 0xFE]);
        assert!(decode_open_request(&bad).is_err());

        // Trailing garbage.
        let mut trailing = full.clone();
        trailing.push(0);
        assert!(decode_open_request(&trailing).is_err());
    }

    #[test]
    fn live_forwarding_roundtrip() {
        let listener = match try_claim(0) {
            Claim::Primary(listener) => listener,
            Claim::Unavailable => panic!("could not bind loopback test port"),
        };
        let port = listener.local_addr().expect("listener addr").port();
        assert!(matches!(try_claim(port), Claim::Unavailable));

        let (tx, rx) = mpsc::channel();
        serve_requests(listener, tx);

        let path = PathBuf::from(r"C:\Users\Tëst\Music");
        forward_open_request(port, Some(&path)).expect("forward Some");
        forward_open_request(port, None).expect("forward None");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("first request"),
            Some(path)
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("second request"),
            None
        );
    }

    #[test]
    fn rejects_bad_banner() {
        let rogue = TcpListener::bind("127.0.0.1:0").expect("rogue bind");
        let port = rogue.local_addr().expect("rogue addr").port();
        thread::spawn(move || {
            let (mut conn, _) = rogue.accept().expect("rogue accept");
            conn.write_all(b"EVIL/00000").expect("rogue write");
            thread::sleep(Duration::from_secs(3));
        });
        let err = forward_open_request(port, None).expect_err("rogue banner must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(windows)]
    #[test]
    fn context_menu_set_and_state() {
        use windows_sys::Win32::System::Registry::{
            RegOpenKeyExW, RegQueryValueExW,
        };

        fn read_sz(hive: HKEY, subkey: &str, name: Option<&str>) -> String {
            let sub = wide(subkey);
            let mut hkey: HKEY = std::ptr::null_mut();
            let status =
                unsafe { RegOpenKeyExW(hive, sub.as_ptr(), 0, KEY_READ, &mut hkey) };
            assert_eq!(status, ERROR_SUCCESS, "open {subkey}");
            let name_w;
            let pname = match name {
                Some(n) => {
                    name_w = wide(n);
                    name_w.as_ptr()
                }
                None => std::ptr::null(),
            };
            let mut ty: u32 = 0;
            let mut cb: u32 = 0;
            let status = unsafe {
                RegQueryValueExW(
                    hkey,
                    pname,
                    std::ptr::null_mut(),
                    &mut ty,
                    std::ptr::null_mut(),
                    &mut cb,
                )
            };
            assert_eq!(status, ERROR_SUCCESS, "query size {subkey}");
            assert_eq!(ty, REG_SZ);
            let mut buf = vec![0u16; cb as usize / 2 + 1];
            let status = unsafe {
                RegQueryValueExW(
                    hkey,
                    pname,
                    std::ptr::null_mut(),
                    &mut ty,
                    buf.as_mut_ptr() as *mut u8,
                    &mut cb,
                )
            };
            unsafe {
                RegCloseKey(hkey);
            }
            assert_eq!(status, ERROR_SUCCESS, "query data {subkey}");
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        }

        let base = format!("Software\\FastDirSelfTest{}", std::process::id());
        let fake_exe = r"C:\Fake\fastdir.exe";

        let result = (|| -> io::Result<()> {
            set_at(&base, fake_exe, true)?;
            assert_eq!(state_at(&base), ContextMenuState::PerUser);
            for kind in ["Directory", "Drive"] {
                let shell = format!("{base}\\{kind}\\shell\\FastDir");
                let cmd = format!("{shell}\\command");
                assert_eq!(read_sz(HKEY_CURRENT_USER, &shell, None), "Analyze with FastDir");
                assert_eq!(
                    read_sz(HKEY_CURRENT_USER, &shell, Some("Icon")),
                    format!("\"{fake_exe}\",0")
                );
                assert_eq!(
                    read_sz(HKEY_CURRENT_USER, &cmd, None),
                    format!("\"{fake_exe}\" \"%1\"")
                );
            }
            set_at(&base, fake_exe, false)?;
            assert_eq!(state_at(&base), ContextMenuState::Absent);
            Ok(())
        })();

        // Clean up the temp tree even on failure, then report.
        let _ = set_at(&base, fake_exe, false);
        let tree = wide(&base);
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, tree.as_ptr());
        }
        result.expect("registry roundtrip");
    }
}

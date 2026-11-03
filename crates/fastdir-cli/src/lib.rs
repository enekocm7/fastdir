//! Headless command line interface and MCP server for FastDir.
//!
//! Everything here is GUI-free: it drives `fastdir_core`'s scanner directly
//! and prints versioned JSON (see [`schema`]) or CSV. Two binaries use it:
//!
//! - `fastdir-cli`, a console program that always stays in the terminal;
//! - `fastdir` (the GUI), which hands over to [`run`] when its first
//!   argument is a subcommand such as `scan`, after attaching to the parent
//!   console (see [`attach_parent_console`]).

pub mod args;
pub mod mcp;
pub mod schema;
mod signal;

use args::{
    Command, DuplicatesCommand, Format, HelpTopic, OutputArgs, ScanArgs, ScanCommand, TopCommand,
};
use fastdir_core::duplicates::{self, DuplicateOptions, DuplicateProgress};
use fastdir_core::export::{ExportOptions, export_csv};
use fastdir_core::format;
use fastdir_core::model::Tree;
use fastdir_core::scanner::{self, Progress};
use schema::{ScanDoc, ScanMeta, TreeShape};
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use args::is_cli_invocation;

/// Process exit codes (documented in the README and `--help`).
pub mod exit {
    pub const OK: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const NOT_FOUND: i32 = 3;
    pub const PERMISSION: i32 = 4;
    pub const CANCELLED: i32 = 130;
}

/// A failure that ends the command with a message on stderr.
#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

impl CliError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Run the command line with `args` (without the program name) and return
/// the process exit code.
///
/// The work runs on a thread with a large stack: the JSON writer recurses
/// once per folder level, and deep trees would overflow the main thread's
/// 1 MB stack on Windows.
pub fn run(args: Vec<OsString>) -> i32 {
    let fallback = args.clone();
    std::thread::Builder::new()
        .name("fastdir-cli".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run_inline(&args))
        .map(|h| h.join().unwrap_or(exit::ERROR))
        // Could not spawn the thread: run here and hope the tree is shallow.
        .unwrap_or_else(|_| run_inline(&fallback))
}

fn run_inline(args: &[OsString]) -> i32 {
    let command = match args::parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fastdir: {e}");
            eprintln!("Run `fastdir --help` for usage.");
            return exit::USAGE;
        }
    };
    let result = match command {
        Command::Help(topic) => {
            print_stdout(match topic {
                HelpTopic::General => args::GENERAL_HELP,
                HelpTopic::Scan => args::SCAN_HELP,
                HelpTopic::Top => args::TOP_HELP,
                HelpTopic::Duplicates => args::DUPLICATES_HELP,
                HelpTopic::Mcp => args::MCP_HELP,
            });
            Ok(())
        }
        Command::Version => {
            print_stdout(&format!("fastdir {}\n", env!("CARGO_PKG_VERSION")));
            Ok(())
        }
        Command::Scan(c) => run_scan_command(&c),
        Command::Top(c) => run_top_command(&c),
        Command::Duplicates(c) => run_duplicates_command(&c),
        Command::Mcp(c) => mcp::serve(c.allow_destructive).map_err(io_error),
    };
    match result {
        Ok(()) => exit::OK,
        Err(e) => {
            if !e.message.is_empty() {
                eprintln!("fastdir: {}", e.message);
            }
            e.code
        }
    }
}

fn print_stdout(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn io_error(e: io::Error) -> CliError {
    // A closed pipe (`fastdir scan . | head`) is the reader's choice, not
    // an error worth reporting.
    if e.kind() == io::ErrorKind::BrokenPipe {
        return CliError::new(exit::OK, "");
    }
    CliError::new(exit::ERROR, e.to_string())
}

/// Make `path` absolute and check it is a readable directory.
pub fn resolve_root(path: &Path) -> Result<PathBuf, CliError> {
    let shown = path.display();
    let abs = std::path::absolute(path)
        .map_err(|e| CliError::new(exit::NOT_FOUND, format!("{shown}: {e}")))?;
    let classify = |e: io::Error| match e.kind() {
        io::ErrorKind::NotFound => {
            CliError::new(exit::NOT_FOUND, format!("{shown}: path not found"))
        }
        io::ErrorKind::PermissionDenied => {
            CliError::new(exit::PERMISSION, format!("{shown}: permission denied"))
        }
        _ => CliError::new(exit::ERROR, format!("{shown}: {e}")),
    };
    let meta = std::fs::metadata(&abs).map_err(classify)?;
    if !meta.is_dir() {
        return Err(CliError::new(
            exit::NOT_FOUND,
            format!("{shown}: not a directory"),
        ));
    }
    std::fs::read_dir(&abs).map_err(classify)?;
    Ok(abs)
}

/// A finished scan plus the facts the documents report about it.
pub struct Scanned {
    pub tree: Tree,
    pub meta: ScanMeta,
}

/// Scan `args.path`, reporting progress on stderr unless `args.quiet`.
/// `Ctrl+C` cancels the scan and yields [`exit::CANCELLED`].
pub fn scan_with_progress(args: &ScanArgs) -> Result<Scanned, CliError> {
    let root = resolve_root(&args.path)?;
    signal::install();
    let progress = Arc::new(Progress::default());
    let done = Arc::new(AtomicBool::new(false));
    let reporter = {
        let progress = progress.clone();
        let done = done.clone();
        let quiet = args.quiet;
        std::thread::spawn(move || report_progress(&progress, &done, quiet))
    };
    let scanned_at = schema::now_iso();
    let result = scanner::scan(root.clone(), progress.clone(), &args.scan_options());
    done.store(true, Ordering::Relaxed);
    let _ = reporter.join();
    if result.cancelled || signal::interrupted() {
        return Err(CliError::new(exit::CANCELLED, "scan cancelled"));
    }
    let errors = progress.errors.load(Ordering::Relaxed);
    let tree = result.tree;
    if !args.quiet {
        let root_node = tree.root();
        let mut line = format!(
            "Scanned {}: {} files, {} folders, {} in {}",
            root.display(),
            format::count(root_node.files as u64),
            format::count(root_node.dirs as u64),
            format::bytes(root_node.size),
            format::duration(result.elapsed),
        );
        if errors > 0 {
            line.push_str(&format!(" ({} unreadable)", format::count(errors)));
        }
        eprintln!("{line}");
    }
    Ok(Scanned {
        tree,
        meta: ScanMeta {
            root,
            scanned_at,
            elapsed_ms: result.elapsed.as_millis() as u64,
            errors,
        },
    })
}

/// Forwards `Ctrl+C` to the scanner and, on an interactive stderr, draws a
/// one-line live counter until `done`.
fn report_progress(progress: &Progress, done: &AtomicBool, quiet: bool) {
    let live = !quiet && io::stderr().is_terminal();
    let mut last_draw = Instant::now();
    let mut drew = false;
    while !done.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        if signal::interrupted() {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        if live && last_draw.elapsed() >= Duration::from_millis(250) {
            last_draw = Instant::now();
            let line = format!(
                "Scanning... {} files, {} folders, {}  {}",
                format::count(progress.files.load(Ordering::Relaxed)),
                format::count(progress.dirs.load(Ordering::Relaxed)),
                format::bytes(progress.bytes.load(Ordering::Relaxed)),
                progress.current.lock().as_str(),
            );
            let line: String = line.chars().take(110).collect();
            eprint!("\r{line:<110}");
            drew = true;
        }
    }
    if drew {
        eprint!("\r{:<110}\r", "");
    }
}

/// Open the destination of `out`: a buffered file or stdout.
fn open_output(out: &OutputArgs) -> Result<Box<dyn Write>, CliError> {
    match &out.output {
        None => Ok(Box::new(io::BufWriter::new(io::stdout().lock()))),
        Some(path) => {
            let file = std::fs::File::create(path).map_err(|e| {
                let code = if e.kind() == io::ErrorKind::PermissionDenied {
                    exit::PERMISSION
                } else {
                    exit::ERROR
                };
                CliError::new(code, format!("{}: {e}", path.display()))
            })?;
            Ok(Box::new(io::BufWriter::new(file)))
        }
    }
}

fn write_json(out: &OutputArgs, value: &impl serde::Serialize) -> Result<(), CliError> {
    let mut w = open_output(out)?;
    let res = if out.pretty {
        serde_json::to_writer_pretty(&mut w, value)
    } else {
        serde_json::to_writer(&mut w, value)
    };
    res.map_err(io::Error::from).map_err(io_error)?;
    w.write_all(b"\n").and_then(|_| w.flush()).map_err(io_error)
}

fn run_scan_command(c: &ScanCommand) -> Result<(), CliError> {
    let scanned = scan_with_progress(&c.scan)?;
    match c.out.format {
        Format::Json => write_json(
            &c.out,
            &ScanDoc {
                meta: &scanned.meta,
                tree: &scanned.tree,
                subtree: &[],
                shape: TreeShape {
                    max_depth: c.max_depth,
                    include_files: c.include_files,
                },
            },
        ),
        Format::Csv => {
            let mut w = open_output(&c.out)?;
            let options = ExportOptions {
                max_depth: c.max_depth,
                include_files: c.include_files,
            };
            export_csv(&scanned.tree, &[], &options, &mut w).map_err(io_error)?;
            w.flush().map_err(io_error)
        }
    }
}

fn run_top_command(c: &TopCommand) -> Result<(), CliError> {
    let scanned = scan_with_progress(&c.scan)?;
    let files = scanned.tree.top_files(c.limit);
    match c.out.format {
        Format::Json => write_json(
            &c.out,
            &schema::top_doc(&scanned.meta, &scanned.tree, &files, c.limit),
        ),
        Format::Csv => {
            let mut w = open_output(&c.out)?;
            w.write_all(schema::top_csv(&scanned.tree, &files).as_bytes())
                .and_then(|_| w.flush())
                .map_err(io_error)
        }
    }
}

fn duplicate_options(c: &DuplicatesCommand) -> DuplicateOptions {
    DuplicateOptions {
        min_size: c.min_size,
        extensions: c.extensions.clone(),
        limit: c.limit,
        threads: c.scan.threads,
    }
}

fn run_duplicates_command(c: &DuplicatesCommand) -> Result<(), CliError> {
    let scanned = scan_with_progress(&c.scan)?;
    signal::install();
    let progress = Arc::new(DuplicateProgress::default());
    let done = Arc::new(AtomicBool::new(false));
    let reporter = {
        let progress = progress.clone();
        let done = done.clone();
        let quiet = c.scan.quiet;
        std::thread::spawn(move || report_duplicate_progress(&progress, &done, quiet))
    };
    let options = duplicate_options(c);
    let report = duplicates::find(&scanned.tree, &[], &options, progress.clone());
    done.store(true, Ordering::Relaxed);
    let _ = reporter.join();
    if report.cancelled || signal::interrupted() {
        return Err(CliError::new(exit::CANCELLED, "duplicates cancelled"));
    }
    if !c.scan.quiet {
        let shown = if c.limit.is_some_and(|limit| limit < report.total_groups) {
            format!(
                "{} (showing {})",
                format::count(report.total_groups as u64),
                format::count(report.groups.len() as u64)
            )
        } else {
            format::count(report.total_groups as u64)
        };
        let mut line = format!(
            "Found {} duplicate groups: {} reclaimable ({} hashed in {})",
            shown,
            format::bytes(report.total_wasted),
            format::bytes(report.bytes_hashed),
            format::duration(report.elapsed),
        );
        if report.hash_errors > 0 {
            line.push_str(&format!(
                " ({} unreadable)",
                format::count(report.hash_errors)
            ));
        }
        eprintln!("{line}");
    }
    match c.out.format {
        Format::Json => write_json(
            &c.out,
            &schema::duplicates_doc(&scanned.meta, &report, &options),
        ),
        Format::Csv => {
            let mut w = open_output(&c.out)?;
            w.write_all(schema::duplicates_csv(&report).as_bytes())
                .and_then(|_| w.flush())
                .map_err(io_error)
        }
    }
}

/// Draws the hashing counter on an interactive stderr and forwards `Ctrl+C`
/// to the duplicate scanner.
fn report_duplicate_progress(progress: &DuplicateProgress, done: &AtomicBool, quiet: bool) {
    let live = !quiet && io::stderr().is_terminal();
    let mut last_draw = Instant::now();
    let mut drew = false;
    while !done.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        if signal::interrupted() {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        if live && last_draw.elapsed() >= Duration::from_millis(250) {
            last_draw = Instant::now();
            let line = format!(
                "Hashing... {} files, {}  {}",
                format::count(progress.files_hashed.load(Ordering::Relaxed)),
                format::bytes(progress.bytes_hashed.load(Ordering::Relaxed)),
                progress.current.lock().as_str(),
            );
            let line: String = line.chars().take(110).collect();
            eprint!("\r{line:<110}");
            drew = true;
        }
    }
    if drew {
        eprint!("\r{:<110}\r", "");
    }
}

/// Connect a GUI-subsystem process to the console it was started from, so
/// `fastdir scan ...` can print. Redirected handles (pipes, files) are
/// inherited regardless and are left alone; only missing ones are pointed
/// at the parent's console. A no-op elsewhere and when there is no parent
/// console (e.g. started from Explorer).
#[cfg(windows)]
pub fn attach_parent_console() {
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    fn missing(which: STD_HANDLE) -> bool {
        let h: HANDLE = unsafe { GetStdHandle(which) };
        h.is_null() || h == INVALID_HANDLE_VALUE
    }

    let wanted: Vec<STD_HANDLE> = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .filter(|&h| missing(h))
        .collect();
    if wanted.is_empty() {
        return;
    }
    // SAFETY: plain Win32 calls with valid, NUL-terminated arguments.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for which in wanted {
            let device = if which == STD_INPUT_HANDLE {
                "CONIN$"
            } else {
                "CONOUT$"
            };
            let name: Vec<u16> = device.encode_utf16().chain([0]).collect();
            let h = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(which, h);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn attach_parent_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_root_classifies_errors() {
        let missing = std::env::temp_dir().join("fastdir-cli-test-definitely-missing");
        let e = resolve_root(&missing).unwrap_err();
        assert_eq!(e.code, exit::NOT_FOUND);

        let dir = std::env::temp_dir().join(format!("fastdir-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, b"hello").unwrap();
        assert_eq!(resolve_root(&file).unwrap_err().code, exit::NOT_FOUND);
        let resolved = resolve_root(&dir).unwrap();
        assert!(resolved.is_absolute());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn duplicates_writes_versioned_json_to_a_file() {
        let dir = std::env::temp_dir().join(format!("fastdir-cli-dupes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), b"identical").unwrap();
        std::fs::write(dir.join("b.bin"), b"identical").unwrap();
        std::fs::write(dir.join("c.bin"), b"different").unwrap();
        let out =
            std::env::temp_dir().join(format!("fastdir-cli-dupes-{}.json", std::process::id()));
        let code = run(vec![
            "duplicates".into(),
            dir.clone().into_os_string(),
            "-q".into(),
            "--min-size".into(),
            "1".into(),
            "-o".into(),
            out.clone().into_os_string(),
        ]);
        assert_eq!(code, exit::OK);
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(doc["schema"], "fastdir.duplicates");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["total_groups"], 1);
        assert_eq!(doc["total_wasted"], 9);
        assert_eq!(doc["groups"][0]["size"], 9);
        assert_eq!(doc["groups"][0]["files"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_file(&out);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn usage_error_exit_code() {
        assert_eq!(run(vec!["scan".into()]), exit::USAGE);
        assert_eq!(run(vec!["bogus".into()]), exit::USAGE);
        assert_eq!(run(vec!["--version".into()]), exit::OK);
    }

    #[test]
    fn scan_missing_path_exit_code() {
        let missing = std::env::temp_dir().join("fastdir-cli-test-definitely-missing");
        assert_eq!(
            run(vec!["scan".into(), "-q".into(), missing.into_os_string()]),
            exit::NOT_FOUND
        );
    }

    #[test]
    fn scan_writes_versioned_json_to_a_file() {
        let dir = std::env::temp_dir().join(format!("fastdir-cli-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("a.bin"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("b.txt"), b"hi").unwrap();
        let out =
            std::env::temp_dir().join(format!("fastdir-cli-scan-{}.json", std::process::id()));
        let code = run(vec![
            "scan".into(),
            dir.clone().into_os_string(),
            "-q".into(),
            "-o".into(),
            out.clone().into_os_string(),
        ]);
        assert_eq!(code, exit::OK);
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(doc["schema"], "fastdir.scan");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["tree"]["size"], 3002);
        assert_eq!(doc["tree"]["files"], 2);
        assert_eq!(doc["tree"]["folders"], 1);
        assert_eq!(doc["tree"]["children"][0]["name"], "sub");
        let _ = std::fs::remove_file(&out);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

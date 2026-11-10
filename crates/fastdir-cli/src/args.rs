//! Command-line argument parsing.
//!
//! Hand-rolled on purpose: the grammar is small, and keeping it dependency-free
//! keeps the GUI binary (which forwards subcommands here) lean. Options accept
//! both `--name value` and `--name=value`; `--` ends option parsing.

use fastdir_core::filter::{Filter, parse_modified, parse_size};
use fastdir_core::scanner::ScanOptions;
use std::ffi::OsString;
use std::path::PathBuf;

/// Subcommand names (and flags) that switch the `fastdir` binary from the GUI
/// to the command line. Anything else (no argument, or a folder path) keeps
/// opening the window.
const CLI_WORDS: &[&str] = &[
    "scan",
    "top",
    "duplicates",
    "diff",
    "mcp",
    "config",
    "help",
    "version",
    "--help",
    "-h",
    "-?",
    "/?",
    "--version",
    "-V",
];

/// Whether `args` (without the program name) asks for the command line
/// rather than the GUI.
pub fn is_cli_invocation(args: &[OsString]) -> bool {
    args.first()
        .and_then(|a| a.to_str())
        .is_some_and(|a| CLI_WORDS.contains(&a))
}

/// Output format of `scan` and `top`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
}

/// Where `--config` reads scan settings from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    /// `%APPDATA%\FastDir\config.toml` (or the platform equivalent).
    Default,
    File(PathBuf),
}

/// Options shared by every command that runs a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanArgs {
    pub path: PathBuf,
    pub threads: Option<usize>,
    pub exclude: Vec<String>,
    pub follow_junctions: bool,
    pub cross_volumes: bool,
    pub skip_hidden_system: bool,
    pub skip_windows_dir: bool,
    pub collect_owners: bool,
    /// `--mft`: read the NTFS MFT when possible (falls back to a walk).
    pub use_mft: bool,
    pub quiet: bool,
    /// `--config`: layer the `[scan]` section of a config file underneath
    /// these flags. `None` leaves config files alone.
    pub config: Option<ConfigSource>,
}

impl ScanArgs {
    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            excluded_globs: self.exclude.clone(),
            skip_hidden_system: self.skip_hidden_system,
            follow_junctions: self.follow_junctions,
            cross_volumes: self.cross_volumes,
            threads: self.threads,
            collect_owners: self.collect_owners,
            skip_windows_dir: self.skip_windows_dir,
            use_mft: self.use_mft,
        }
    }
}

/// Where and how to write a command's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputArgs {
    pub format: Format,
    /// `None` means stdout (`-o -` or no `--output`).
    pub output: Option<PathBuf>,
    pub pretty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanCommand {
    pub scan: ScanArgs,
    pub out: OutputArgs,
    pub max_depth: Option<usize>,
    pub include_files: bool,
    /// Name, size, extension and date restrictions (matches everything when
    /// inactive).
    pub filter: Filter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopCommand {
    pub scan: ScanArgs,
    pub out: OutputArgs,
    pub limit: usize,
    /// Chooses which files are ranked.
    pub filter: Filter,
}

/// `fastdir diff <old.json> <new.json|path>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffCommand {
    /// Saved scan to compare against (the baseline).
    pub old: PathBuf,
    /// Saved scan or live folder to compare it with.
    pub new: PathBuf,
    /// Scan options for when `new` is a folder rather than a saved scan.
    pub scan: ScanArgs,
    pub out: OutputArgs,
    /// Hide changes smaller than this many bytes.
    pub min_change: u64,
    pub max_depth: Option<usize>,
    /// Cap on the number of entries (`None` = all).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicatesCommand {
    pub scan: ScanArgs,
    pub out: OutputArgs,
    /// Smallest file to hash; zero-byte files are always skipped.
    pub min_size: u64,
    /// Only files with one of these extensions (repeatable, empty = all).
    pub extensions: Vec<String>,
    /// Most groups to report (`None` = all).
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCommand {
    /// Expose `recycle_path` / `delete_path`. Off by default.
    pub allow_destructive: bool,
    /// `--config`: scan settings applied to every scan the server runs.
    pub config: Option<ConfigSource>,
}

/// `fastdir config`: print the effective scan options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigCommand {
    /// Defaults to the standard config file.
    pub source: ConfigSource,
}

/// Which help text to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpTopic {
    General,
    Scan,
    Top,
    Duplicates,
    Diff,
    Mcp,
    Config,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help(HelpTopic),
    Version,
    Scan(ScanCommand),
    Top(TopCommand),
    Duplicates(DuplicatesCommand),
    Diff(DiffCommand),
    Mcp(McpCommand),
    Config(ConfigCommand),
}

/// Default number of files `top` reports.
pub const DEFAULT_TOP_LIMIT: usize = 20;

/// Default smallest file `duplicates` hashes.
pub const DEFAULT_DUPLICATE_MIN_SIZE: u64 = fastdir_core::duplicates::DEFAULT_MIN_SIZE;

/// A usage error, printed with a hint to run `--help`. Exit code 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, UsageError> {
    Err(UsageError(msg.into()))
}

/// Turns `C:` into `C:\` (a bare drive letter means "current directory on
/// that drive" to Windows, which is never what a user typing it means) and
/// strips stray quotes left by shells.
pub fn normalize_path(raw: &str) -> PathBuf {
    let mut arg = raw.trim().trim_matches('"').to_string();
    if arg.len() == 2 && arg.ends_with(':') {
        arg.push(std::path::MAIN_SEPARATOR);
    }
    PathBuf::from(arg)
}

/// Splits `--name=value` into its parts.
fn split_eq(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

struct Cursor {
    args: std::vec::IntoIter<String>,
}

impl Cursor {
    /// The value of `name`: the inline `=value` or the next argument.
    fn value(&mut self, name: &str, inline: Option<&str>) -> Result<String, UsageError> {
        if let Some(v) = inline {
            return Ok(v.to_string());
        }
        match self.args.next() {
            Some(v) => Ok(v),
            None => err(format!("{name} needs a value")),
        }
    }
}

impl Cursor {
    /// `--config` takes an optional file: `--config=<file>`, or a following
    /// argument ending in `.toml`. A bare `--config` means the default file.
    /// (Any other following argument stays a path to scan.)
    fn config_source(&mut self, inline: Option<&str>) -> ConfigSource {
        if let Some(v) = inline {
            return ConfigSource::File(normalize_path(v));
        }
        let next_is_toml = self
            .args
            .as_slice()
            .first()
            .is_some_and(|a| a.to_ascii_lowercase().ends_with(".toml"));
        if next_is_toml && let Some(v) = self.args.next() {
            return ConfigSource::File(normalize_path(&v));
        }
        ConfigSource::Default
    }
}

fn parse_number(name: &str, raw: &str, min: usize) -> Result<usize, UsageError> {
    match raw.trim().parse::<usize>() {
        Ok(n) if n >= min => Ok(n),
        _ if min > 0 => err(format!(
            "{name} expects a whole number of at least {min}, got \"{raw}\""
        )),
        _ => err(format!("{name} expects a whole number, got \"{raw}\"")),
    }
}

fn parse_format(raw: &str) -> Result<Format, UsageError> {
    match raw.to_ascii_lowercase().as_str() {
        "json" => Ok(Format::Json),
        "csv" => Ok(Format::Csv),
        _ => err(format!("--format expects json or csv, got \"{raw}\"")),
    }
}

/// Parse the arguments after the program name.
pub fn parse(args: &[OsString]) -> Result<Command, UsageError> {
    let mut strings = Vec::with_capacity(args.len());
    for a in args {
        match a.to_str() {
            Some(s) => strings.push(s.to_string()),
            None => {
                return err(format!(
                    "argument is not valid Unicode: {}",
                    a.to_string_lossy()
                ));
            }
        }
    }
    let mut cur = Cursor {
        args: strings.into_iter(),
    };
    let Some(first) = cur.args.next() else {
        return Ok(Command::Help(HelpTopic::General));
    };
    match first.as_str() {
        "--help" | "-h" | "-?" | "/?" => Ok(Command::Help(HelpTopic::General)),
        "--version" | "-V" | "version" => Ok(Command::Version),
        "help" => match cur.args.next().as_deref() {
            None => Ok(Command::Help(HelpTopic::General)),
            Some("scan") => Ok(Command::Help(HelpTopic::Scan)),
            Some("top") => Ok(Command::Help(HelpTopic::Top)),
            Some("duplicates") => Ok(Command::Help(HelpTopic::Duplicates)),
            Some("diff") => Ok(Command::Help(HelpTopic::Diff)),
            Some("mcp") => Ok(Command::Help(HelpTopic::Mcp)),
            Some("config") => Ok(Command::Help(HelpTopic::Config)),
            Some(other) => err(format!("unknown command \"{other}\"")),
        },
        "scan" => parse_scan_like(cur, HelpTopic::Scan),
        "top" => parse_scan_like(cur, HelpTopic::Top),
        "duplicates" => parse_duplicates(cur),
        "diff" => parse_diff(cur),
        "mcp" => parse_mcp(cur),
        "config" => parse_config(cur),
        other if other.starts_with('-') => err(format!("unknown option \"{other}\"")),
        other => err(format!("unknown command \"{other}\"")),
    }
}

fn parse_scan_like(mut cur: Cursor, topic: HelpTopic) -> Result<Command, UsageError> {
    let mut path: Option<String> = None;
    let mut threads = None;
    let mut exclude = Vec::new();
    let mut follow_junctions = false;
    let mut cross_volumes = false;
    let mut skip_hidden_system = false;
    let mut skip_windows_dir = false;
    let mut use_mft = false;
    let mut collect_owners = false;
    let mut quiet = false;
    let mut config = None;
    let mut format = Format::Json;
    let mut output = None;
    let mut pretty = false;
    let mut max_depth = None;
    let mut include_files = true;
    let mut limit = DEFAULT_TOP_LIMIT;
    let mut name_filter = String::new();
    let mut min_size = 0u64;
    let mut extensions = Vec::new();
    let mut newer_than = None;
    let mut older_than = None;
    let is_scan = topic == HelpTopic::Scan;
    let mut positional_only = false;

    while let Some(arg) = cur.args.next() {
        if positional_only || !arg.starts_with('-') || arg == "-" {
            if path.is_some() {
                return err(format!("unexpected extra argument \"{arg}\""));
            }
            path = Some(arg);
            continue;
        }
        let (name, inline) = split_eq(&arg);
        let takes_value = matches!(
            name,
            "--threads"
                | "--config"
                | "--exclude"
                | "--format"
                | "--output"
                | "-o"
                | "--max-depth"
                | "--limit"
                | "-n"
                | "--name"
                | "--min-size"
                | "--ext"
                | "--newer-than"
                | "--older-than"
        );
        if inline.is_some() && !takes_value {
            return err(format!("{name} does not take a value"));
        }
        match name {
            "--" => positional_only = true,
            "--help" | "-h" => return Ok(Command::Help(topic)),
            "--threads" => {
                let n = parse_number(name, &cur.value(name, inline)?, 1)?;
                if n > 64 {
                    return err("--threads expects a number between 1 and 64");
                }
                threads = Some(n);
            }
            "--exclude" => exclude.push(cur.value(name, inline)?),
            "--follow-junctions" => follow_junctions = true,
            "--cross-volumes" => cross_volumes = true,
            "--skip-hidden" | "--skip-system" | "--skip-hidden-system" => skip_hidden_system = true,
            "--skip-windows" => skip_windows_dir = true,
            "--mft" => use_mft = true,
            "--owners" => collect_owners = true,
            "--quiet" | "-q" => quiet = true,
            "--config" => config = Some(cur.config_source(inline)),
            "--format" => format = parse_format(&cur.value(name, inline)?)?,
            "--output" | "-o" => {
                let v = cur.value(name, inline)?;
                output = if v == "-" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            "--pretty" => pretty = true,
            "--max-depth" if is_scan => {
                max_depth = Some(parse_number(name, &cur.value(name, inline)?, 0)?)
            }
            "--no-files" if is_scan => include_files = false,
            "--limit" | "-n" if !is_scan => {
                limit = parse_number(name, &cur.value(name, inline)?, 1)?
            }
            "--name" => name_filter = cur.value(name, inline)?,
            "--min-size" => {
                let raw = cur.value(name, inline)?;
                min_size = parse_size(&raw).ok_or_else(|| {
                    UsageError(format!(
                        "--min-size expects a size such as 500MB or 1.5GiB, got \"{raw}\""
                    ))
                })?;
            }
            "--ext" => extensions.push(cur.value(name, inline)?),
            "--newer-than" => {
                let raw = cur.value(name, inline)?;
                newer_than = Some(parse_modified(&raw, false).ok_or_else(|| {
                    UsageError(format!(
                        "--newer-than expects a date such as 2026-01-01 or an age such as 30d, 6m or 1y, got \"{raw}\""
                    ))
                })?);
            }
            "--older-than" => {
                let raw = cur.value(name, inline)?;
                older_than = Some(parse_modified(&raw, true).ok_or_else(|| {
                    UsageError(format!(
                        "--older-than expects a date such as 2026-01-01 or an age such as 30d, 6m or 1y, got \"{raw}\""
                    ))
                })?);
            }
            _ => return err(format!("unknown option \"{name}\"")),
        }
    }
    if let (Some(after), Some(before)) = (newer_than, older_than)
        && after > before
    {
        return err("--newer-than is later than --older-than");
    }
    let Some(path) = path else {
        return err("missing <path> to scan");
    };
    let filter =
        Filter::with_extensions(&name_filter, min_size, &extensions, newer_than, older_than);
    let scan = ScanArgs {
        path: normalize_path(&path),
        threads,
        exclude,
        follow_junctions,
        cross_volumes,
        skip_hidden_system,
        skip_windows_dir,
        collect_owners,
        use_mft,
        quiet,
        config,
    };
    let out = OutputArgs {
        format,
        output,
        pretty,
    };
    Ok(if is_scan {
        Command::Scan(ScanCommand {
            scan,
            out,
            max_depth,
            include_files,
            filter,
        })
    } else {
        Command::Top(TopCommand {
            scan,
            out,
            limit,
            filter,
        })
    })
}

fn parse_duplicates(mut cur: Cursor) -> Result<Command, UsageError> {
    let mut path: Option<String> = None;
    let mut threads = None;
    let mut exclude = Vec::new();
    let mut follow_junctions = false;
    let mut cross_volumes = false;
    let mut skip_hidden_system = false;
    let mut skip_windows_dir = false;
    let mut use_mft = false;
    let mut quiet = false;
    let mut config = None;
    let mut format = Format::Json;
    let mut output = None;
    let mut pretty = false;
    let mut min_size = DEFAULT_DUPLICATE_MIN_SIZE;
    let mut extensions = Vec::new();
    let mut limit = None;
    let mut positional_only = false;

    while let Some(arg) = cur.args.next() {
        if positional_only || !arg.starts_with('-') || arg == "-" {
            if path.is_some() {
                return err(format!("unexpected extra argument \"{arg}\""));
            }
            path = Some(arg);
            continue;
        }
        let (name, inline) = split_eq(&arg);
        let takes_value = matches!(
            name,
            "--threads"
                | "--config"
                | "--exclude"
                | "--format"
                | "--output"
                | "-o"
                | "--min-size"
                | "--ext"
                | "--limit"
                | "-n"
        );
        if inline.is_some() && !takes_value {
            return err(format!("{name} does not take a value"));
        }
        match name {
            "--" => positional_only = true,
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Duplicates)),
            "--threads" => {
                let n = parse_number(name, &cur.value(name, inline)?, 1)?;
                if n > 64 {
                    return err("--threads expects a number between 1 and 64");
                }
                threads = Some(n);
            }
            "--exclude" => exclude.push(cur.value(name, inline)?),
            "--follow-junctions" => follow_junctions = true,
            "--cross-volumes" => cross_volumes = true,
            "--skip-hidden" | "--skip-system" | "--skip-hidden-system" => skip_hidden_system = true,
            "--skip-windows" => skip_windows_dir = true,
            "--mft" => use_mft = true,
            "--quiet" | "-q" => quiet = true,
            "--config" => config = Some(cur.config_source(inline)),
            "--format" => format = parse_format(&cur.value(name, inline)?)?,
            "--output" | "-o" => {
                let v = cur.value(name, inline)?;
                output = if v == "-" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            "--pretty" => pretty = true,
            "--min-size" => {
                let raw = cur.value(name, inline)?;
                min_size = fastdir_core::filter::parse_size(&raw).ok_or_else(|| {
                    UsageError(format!(
                        "--min-size expects a size such as 500MB or 1 GiB, got \"{raw}\""
                    ))
                })?;
            }
            "--ext" => extensions.push(cur.value(name, inline)?),
            "--limit" | "-n" => limit = Some(parse_number(name, &cur.value(name, inline)?, 1)?),
            _ => return err(format!("unknown option \"{name}\"")),
        }
    }
    let Some(path) = path else {
        return err("missing <path> to scan");
    };
    Ok(Command::Duplicates(DuplicatesCommand {
        scan: ScanArgs {
            path: normalize_path(&path),
            threads,
            exclude,
            follow_junctions,
            cross_volumes,
            skip_hidden_system,
            skip_windows_dir,
            collect_owners: false,
            use_mft,
            quiet,
            config,
        },
        out: OutputArgs {
            format,
            output,
            pretty,
        },
        min_size,
        extensions,
        limit,
    }))
}

fn parse_diff(mut cur: Cursor) -> Result<Command, UsageError> {
    let mut positional: Vec<String> = Vec::new();
    let mut threads = None;
    let mut exclude = Vec::new();
    let mut follow_junctions = false;
    let mut cross_volumes = false;
    let mut skip_hidden_system = false;
    let mut skip_windows_dir = false;
    let mut use_mft = false;
    let mut collect_owners = false;
    let mut quiet = false;
    let mut config = None;
    let mut format = Format::Json;
    let mut output = None;
    let mut pretty = false;
    let mut min_change = 0;
    let mut max_depth = None;
    let mut limit = None;
    let mut positional_only = false;

    while let Some(arg) = cur.args.next() {
        if positional_only || !arg.starts_with('-') || arg == "-" {
            positional.push(arg);
            continue;
        }
        let (name, inline) = split_eq(&arg);
        let takes_value = matches!(
            name,
            "--threads"
                | "--config"
                | "--exclude"
                | "--format"
                | "--output"
                | "-o"
                | "--min-change"
                | "--max-depth"
                | "--limit"
                | "-n"
        );
        if inline.is_some() && !takes_value {
            return err(format!("{name} does not take a value"));
        }
        match name {
            "--" => positional_only = true,
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Diff)),
            "--threads" => {
                let n = parse_number(name, &cur.value(name, inline)?, 1)?;
                if n > 64 {
                    return err("--threads expects a number between 1 and 64");
                }
                threads = Some(n);
            }
            "--exclude" => exclude.push(cur.value(name, inline)?),
            "--follow-junctions" => follow_junctions = true,
            "--cross-volumes" => cross_volumes = true,
            "--skip-hidden" | "--skip-system" | "--skip-hidden-system" => skip_hidden_system = true,
            "--skip-windows" => skip_windows_dir = true,
            "--mft" => use_mft = true,
            "--owners" => collect_owners = true,
            "--quiet" | "-q" => quiet = true,
            "--config" => config = Some(cur.config_source(inline)),
            "--format" => format = parse_format(&cur.value(name, inline)?)?,
            "--output" | "-o" => {
                let v = cur.value(name, inline)?;
                output = if v == "-" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            "--pretty" => pretty = true,
            "--min-change" => {
                let raw = cur.value(name, inline)?;
                min_change = parse_size(&raw).ok_or_else(|| {
                    UsageError(format!(
                        "--min-change expects a size such as 500MB or 1.5GB, got \"{raw}\""
                    ))
                })?;
            }
            "--max-depth" => max_depth = Some(parse_number(name, &cur.value(name, inline)?, 0)?),
            "--limit" | "-n" => limit = Some(parse_number(name, &cur.value(name, inline)?, 1)?),
            _ => return err(format!("unknown option \"{name}\"")),
        }
    }
    if positional.len() != 2 {
        return err("diff needs <old.json> and <new.json|path>");
    }
    let new = normalize_path(&positional[1]);
    let scan = ScanArgs {
        path: new.clone(),
        threads,
        exclude,
        follow_junctions,
        cross_volumes,
        skip_hidden_system,
        skip_windows_dir,
        collect_owners,
        use_mft,
        quiet,
        config,
    };
    Ok(Command::Diff(DiffCommand {
        old: normalize_path(&positional[0]),
        new,
        scan,
        out: OutputArgs {
            format,
            output,
            pretty,
        },
        min_change,
        max_depth,
        limit,
    }))
}

fn parse_mcp(mut cur: Cursor) -> Result<Command, UsageError> {
    let mut allow_destructive = false;
    let mut config = None;
    while let Some(arg) = cur.args.next() {
        let (name, inline) = split_eq(&arg);
        match name {
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Mcp)),
            "--allow-destructive" if inline.is_none() => allow_destructive = true,
            "--config" => config = Some(cur.config_source(inline)),
            other if other.starts_with('-') => return err(format!("unknown option \"{other}\"")),
            other => return err(format!("unexpected argument \"{other}\"")),
        }
    }
    Ok(Command::Mcp(McpCommand {
        allow_destructive,
        config,
    }))
}

fn parse_config(mut cur: Cursor) -> Result<Command, UsageError> {
    let mut source = ConfigSource::Default;
    while let Some(arg) = cur.args.next() {
        let (name, inline) = split_eq(&arg);
        match name {
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Config)),
            "--config" => source = cur.config_source(inline),
            other if other.starts_with('-') => return err(format!("unknown option \"{other}\"")),
            // `fastdir config <file>` is accepted as shorthand.
            other => source = ConfigSource::File(normalize_path(other)),
        }
    }
    Ok(Command::Config(ConfigCommand { source }))
}

pub const GENERAL_HELP: &str = "\
FastDir - fast disk usage analyzer

Usage:
  fastdir [<folder>]              open the GUI (optionally scanning <folder>)
  fastdir scan <path> [options]   scan <path> and print the tree as JSON or CSV
  fastdir top <path> [options]    print the largest files under <path>
  fastdir duplicates <path> [options]
                                  list groups of identical files and the space they waste
  fastdir diff <old> <new>        show what changed between two scans
  fastdir mcp [options]           run an MCP server on stdin/stdout
  fastdir config [<file>]         print the scan options a config file gives
  fastdir help [<command>]        show help for a command
  fastdir --version               print the version

`fastdir-cli` accepts the same commands and always stays in the terminal.

Exit codes:
  0    success
  1    error while scanning or writing the output
  2    invalid arguments
  3    path not found or not a directory
  4    permission denied
  130  cancelled with Ctrl+C
";

pub const SCAN_HELP: &str = "\
Usage: fastdir scan <path> [options]

Scans <path> and writes the tree to stdout (JSON by default). Progress and a
summary go to stderr.

Output:
  --format json|csv       output format (default: json)
  -o, --output <file|->   write to a file instead of stdout
  --pretty                indent the JSON
  --max-depth <n>         only include entries up to <n> levels below <path>
                          (0 = just <path>); sizes still count everything
  --no-files              only include folders

Filtering:
  --name <text|glob>      only entries whose name matches; a glob when it
                          contains * or ? (case-insensitive)
  --min-size <size>       only entries at least this large (500MB, 1.5 GiB)
  --ext <ext>             only files with this extension (repeatable)
  --newer-than <when>     modified on/after a date (2026-01-01) or within an
                          age (30d, 6m, 1y)
  --older-than <when>     modified on/before a date or older than an age
  Matching entries and their ancestors are kept; folder totals still count
  everything below them.

Scanning:
  --threads <n>           scan threads, 1-64 (default: automatic)
  --exclude <glob>        skip entries whose name matches (repeatable)
  --follow-junctions      descend into junctions and symlinked folders
  --cross-volumes         with --follow-junctions, also enter other volumes
  --skip-hidden           skip hidden and system entries (alias: --skip-system)
  --skip-windows          skip the Windows folder (%SystemRoot%) for a faster
                          system-drive scan
  --mft                   read the NTFS Master File Table directly (much faster;
                          needs administrator rights, falls back to a normal
                          scan otherwise)
  --owners                record each file's owner (Owner column, per-user totals)
  -q, --quiet             no progress or summary on stderr
  --config[=<file>]       use the [scan] section of config.toml (default file, or
                          <file>); flags override it, --exclude adds to it
";

pub const TOP_HELP: &str = "\
Usage: fastdir top <path> [options]

Scans <path> and writes its largest files to stdout, largest first.

Output:
  -n, --limit <n>         how many files to list (default: 20)
  --format json|csv       output format (default: json)
  -o, --output <file|->   write to a file instead of stdout
  --pretty                indent the JSON

Filtering (chooses which files are ranked):
  --name <text|glob>, --min-size <size>, --ext <ext>,
  --newer-than <when>, --older-than <when>   as for `fastdir scan`

Scanning:
  --threads <n>, --exclude <glob>, --follow-junctions, --cross-volumes,
  --skip-hidden, --skip-windows, --mft, --owners, -q/--quiet, --config
                          as for `fastdir scan`
";

pub const DUPLICATES_HELP: &str = "\
Usage: fastdir duplicates <path> [options]

Scans <path> and lists groups of byte-identical files, largest waste first.
Files are grouped by size, then the first and last 64 KB are hashed, and only
the files that still match get a full BLAKE3 hash. Zero-byte files and extra
hard links to a file are not duplicates.

Output:
  --format json|csv       output format (default: json)
  -o, --output <file|->   write to a file instead of stdout
  --pretty                indent the JSON

Duplicates:
  --min-size <size>       smallest file to consider (default: 1 MB);
                          accepts 500MB or 1.5 GiB
  --ext <ext>             only files with this extension (repeatable), e.g. .mp4
  -n, --limit <groups>    how many groups to report (default: all);
                          the totals still count every group found

Scanning:
  --threads <n>, --exclude <glob>, --follow-junctions, --cross-volumes,
  --skip-hidden, --skip-windows, --mft, -q/--quiet, --config
                          as for `fastdir scan`
";

pub const DIFF_HELP: &str = "\
Usage: fastdir diff <old.json> <new.json|path> [options]

Compares a saved scan (the baseline) with another saved scan or with a fresh
scan of a folder or drive, and lists what was added, removed, grew or shrank.
Entries are matched by relative path, so the baseline may come from another
machine.

Output:
  --format json|csv       output format (default: json)
  -o, --output <file|->   write to a file instead of stdout
  --pretty                indent the JSON
  --min-change <size>     hide changes smaller than <size> (e.g. 20GB)
  --max-depth <n>         only entries up to <n> levels below the root
                          (0 = the root row only)
  -n, --limit <n>         cap the number of entries (largest change first)

Scanning (only when <new> is a folder):
  --threads <n>           scan threads, 1-64 (default: automatic)
  --exclude <glob>        skip entries whose name matches (repeatable)
  --follow-junctions      descend into junctions and symlinked folders
  --cross-volumes         with --follow-junctions, also enter other volumes
  --skip-hidden           skip hidden and system entries (alias: --skip-system)
  --skip-windows          skip the Windows folder (%SystemRoot%) for a faster
                          system-drive scan
  --mft                   read the NTFS Master File Table directly (much faster;
                          needs administrator rights, falls back to a normal
                          scan otherwise)
  -q, --quiet             no progress or summary on stderr
  --config[=<file>]       use the [scan] section of config.toml (default file, or
                          <file>); flags override it, --exclude adds to it
";

pub const CONFIG_HELP: &str = "\
Usage: fastdir config [<file> | --config[=<file>]]

Prints, as JSON, the scan options that `--config` would apply: the [scan]
section of the default config.toml (%APPDATA%\\FastDir\\config.toml on
Windows) or of <file>. Other sections are ignored. A missing or invalid file
is an error (exit code 2).

Commands only read the file when given --config; without it nothing changes.
";

pub const MCP_HELP: &str = "\
Usage: fastdir mcp [--allow-destructive] [--config[=<file>]]

Runs a Model Context Protocol server over stdio (newline-delimited JSON-RPC).
Scans are kept in memory, so follow-up questions about the same folder do
not rescan it.

Tools: scan_path, get_children, largest_files, search, path_info,
find_duplicates, compare_scans, export_scan. With --allow-destructive,
recycle_path and delete_path are also offered; they are off by default.

--config[=<file>] applies the [scan] section of config.toml (exclusions,
hidden/system and Windows-folder skipping, junctions, volumes, threads) to every scan the server
runs, so the agent sees the same scan as the app. Per-call arguments add to
or override it. A missing or invalid file stops the server with exit code 2.
Without --config the file is ignored.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn parse_ok(list: &[&str]) -> Command {
        parse(&args(list)).expect("parses")
    }

    fn parse_err(list: &[&str]) -> String {
        parse(&args(list)).expect_err("fails").0
    }

    fn scan_cmd(list: &[&str]) -> ScanCommand {
        match parse_ok(list) {
            Command::Scan(c) => c,
            other => panic!("expected scan, got {other:?}"),
        }
    }

    #[test]
    fn cli_invocation_detection() {
        assert!(is_cli_invocation(&args(&["scan", "C:\\"])));
        assert!(is_cli_invocation(&args(&["top"])));
        assert!(is_cli_invocation(&args(&["duplicates", "C:\\"])));
        assert!(is_cli_invocation(&args(&["diff"])));
        assert!(is_cli_invocation(&args(&["mcp"])));
        assert!(is_cli_invocation(&args(&["--version"])));
        assert!(is_cli_invocation(&args(&["--help"])));
        assert!(!is_cli_invocation(&args(&[])));
        assert!(!is_cli_invocation(&args(&["C:\\Users"])));
        assert!(!is_cli_invocation(&args(&["D:"])));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse_ok(&[]), Command::Help(HelpTopic::General));
        assert_eq!(parse_ok(&["--help"]), Command::Help(HelpTopic::General));
        assert_eq!(parse_ok(&["-h"]), Command::Help(HelpTopic::General));
        assert_eq!(parse_ok(&["help", "scan"]), Command::Help(HelpTopic::Scan));
        assert_eq!(parse_ok(&["help", "top"]), Command::Help(HelpTopic::Top));
        assert_eq!(parse_ok(&["help", "diff"]), Command::Help(HelpTopic::Diff));
        assert_eq!(
            parse_ok(&["help", "duplicates"]),
            Command::Help(HelpTopic::Duplicates)
        );
        assert_eq!(
            parse_ok(&["duplicates", "--help"]),
            Command::Help(HelpTopic::Duplicates)
        );
        assert_eq!(
            parse_ok(&["scan", "--help"]),
            Command::Help(HelpTopic::Scan)
        );
        assert_eq!(parse_ok(&["top", "x", "-h"]), Command::Help(HelpTopic::Top));
        assert_eq!(
            parse_ok(&["diff", "a", "b", "-h"]),
            Command::Help(HelpTopic::Diff)
        );
        assert_eq!(parse_ok(&["mcp", "--help"]), Command::Help(HelpTopic::Mcp));
        assert_eq!(parse_ok(&["--version"]), Command::Version);
        assert_eq!(parse_ok(&["-V"]), Command::Version);
        assert_eq!(parse_ok(&["version"]), Command::Version);
    }

    #[test]
    fn scan_defaults() {
        let c = scan_cmd(&["scan", "some/dir"]);
        assert_eq!(c.scan.path, PathBuf::from("some/dir"));
        assert_eq!(c.out.format, Format::Json);
        assert_eq!(c.out.output, None);
        assert!(!c.out.pretty);
        assert_eq!(c.max_depth, None);
        assert!(c.include_files);
        assert_eq!(c.scan.threads, None);
        assert!(c.scan.exclude.is_empty());
        assert!(!c.scan.follow_junctions);
        assert!(!c.scan.cross_volumes);
        assert!(!c.scan.skip_hidden_system);
        assert!(!c.scan.collect_owners);
        assert!(!c.scan.quiet);
        assert!(!c.filter.is_active());
    }

    #[test]
    fn scan_and_top_filters() {
        let c = scan_cmd(&[
            "scan",
            "dir",
            "--name",
            "*.iso",
            "--min-size",
            "1.5 GiB",
            "--ext",
            "iso",
            "--ext=.img",
            "--newer-than",
            "2026-01-01",
            "--older-than",
            "30d",
        ]);
        assert_eq!(c.filter.query(), "*.iso");
        assert_eq!(c.filter.min_size(), 3 * 1024 * 1024 * 1024 / 2);
        assert_eq!(
            c.filter.extensions(),
            [".iso".to_string(), ".img".to_string()]
        );
        assert!(c.filter.modified_after().is_some());
        assert!(c.filter.modified_before().is_some());
        assert!(c.filter.is_active());

        match parse_ok(&["top", "dir", "--name", "readme", "--ext", ".TXT"]) {
            Command::Top(t) => {
                assert_eq!(t.filter.query(), "readme");
                assert_eq!(t.filter.extensions(), [".txt".to_string()]);
            }
            other => panic!("expected top, got {other:?}"),
        }
    }

    #[test]
    fn scan_all_options() {
        let c = scan_cmd(&[
            "scan",
            "--format=csv",
            "-o",
            "out.csv",
            "--max-depth",
            "2",
            "--no-files",
            "--threads=8",
            "--exclude",
            "node_modules",
            "--exclude=*.tmp",
            "--follow-junctions",
            "--cross-volumes",
            "--skip-system",
            "--skip-windows",
            "--mft",
            "--owners",
            "-q",
            "--pretty",
            "dir",
        ]);
        assert_eq!(c.out.format, Format::Csv);
        assert_eq!(c.out.output, Some(PathBuf::from("out.csv")));
        assert!(c.out.pretty);
        assert_eq!(c.max_depth, Some(2));
        assert!(!c.include_files);
        assert_eq!(c.scan.threads, Some(8));
        assert_eq!(c.scan.exclude, vec!["node_modules", "*.tmp"]);
        assert!(c.scan.follow_junctions && c.scan.cross_volumes);
        assert!(c.scan.skip_hidden_system && c.scan.quiet && c.scan.collect_owners);
        assert!(c.scan.skip_windows_dir && c.scan.scan_options().skip_windows_dir);
        assert!(c.scan.use_mft && c.scan.scan_options().use_mft);
        assert_eq!(c.scan.path, PathBuf::from("dir"));
        let opts = c.scan.scan_options();
        assert_eq!(opts.threads, Some(8));
        assert!(opts.skip_hidden_system && opts.follow_junctions && opts.cross_volumes);
        assert!(opts.collect_owners);
    }

    #[test]
    fn output_dash_means_stdout() {
        assert_eq!(scan_cmd(&["scan", "d", "-o", "-"]).out.output, None);
        assert_eq!(scan_cmd(&["scan", "d", "--output=-"]).out.output, None);
    }

    #[test]
    fn double_dash_allows_dash_paths() {
        let c = scan_cmd(&["scan", "--", "-weird"]);
        assert_eq!(c.scan.path, PathBuf::from("-weird"));
    }

    #[test]
    fn drive_letter_gets_a_separator() {
        let c = scan_cmd(&["scan", "C:"]);
        let expected = format!("C:{}", std::path::MAIN_SEPARATOR);
        assert_eq!(c.scan.path, PathBuf::from(expected));
        assert_eq!(normalize_path("\"D:\\Data\""), PathBuf::from("D:\\Data"));
    }

    #[test]
    fn top_options() {
        match parse_ok(&["top", "dir", "--limit", "5", "--format", "CSV"]) {
            Command::Top(t) => {
                assert_eq!(t.limit, 5);
                assert_eq!(t.out.format, Format::Csv);
                assert_eq!(t.scan.path, PathBuf::from("dir"));
            }
            other => panic!("expected top, got {other:?}"),
        }
        match parse_ok(&["top", "dir"]) {
            Command::Top(t) => assert_eq!(t.limit, DEFAULT_TOP_LIMIT),
            other => panic!("expected top, got {other:?}"),
        }
        match parse_ok(&["top", "-n", "3", "dir"]) {
            Command::Top(t) => assert_eq!(t.limit, 3),
            other => panic!("expected top, got {other:?}"),
        }
    }

    #[test]
    fn duplicates_options() {
        match parse_ok(&["duplicates", "dir"]) {
            Command::Duplicates(d) => {
                assert_eq!(d.scan.path, PathBuf::from("dir"));
                assert_eq!(d.out.format, Format::Json);
                assert_eq!(d.min_size, DEFAULT_DUPLICATE_MIN_SIZE);
                assert!(d.extensions.is_empty());
                assert_eq!(d.limit, None);
            }
            other => panic!("expected duplicates, got {other:?}"),
        }
        let c = match parse_ok(&[
            "duplicates",
            "D:\\",
            "--min-size",
            "1.5 GiB",
            "--ext",
            "mp4",
            "--ext=.iso",
            "-n",
            "5",
            "--format",
            "csv",
            "-o",
            "dupes.csv",
            "--threads",
            "4",
            "--exclude",
            "node_modules",
            "--skip-system",
            "-q",
            "--pretty",
        ]) {
            Command::Duplicates(d) => d,
            other => panic!("expected duplicates, got {other:?}"),
        };
        assert_eq!(c.min_size, 3 * 1024 * 1024 * 1024 / 2);
        assert_eq!(c.extensions, vec!["mp4", ".iso"]);
        assert_eq!(c.limit, Some(5));
        assert_eq!(c.out.format, Format::Csv);
        assert_eq!(c.out.output, Some(PathBuf::from("dupes.csv")));
        assert_eq!(c.scan.threads, Some(4));
        assert_eq!(c.scan.exclude, vec!["node_modules"]);
        assert!(c.scan.skip_hidden_system && c.scan.quiet);
        assert!(c.out.pretty);

        assert!(parse_err(&["duplicates"]).contains("missing <path>"));
        assert!(parse_err(&["duplicates", "d", "--min-size", "lots"]).contains("--min-size"));
        assert!(parse_err(&["duplicates", "d", "--limit", "0"]).contains("at least 1"));
        assert!(parse_err(&["duplicates", "d", "--max-depth", "2"]).contains("--max-depth"));
    }

    #[test]
    fn diff_options() {
        let c = match parse_ok(&[
            "diff",
            "old.json",
            "C:\\Users",
            "--format",
            "csv",
            "-o",
            "out.csv",
            "--pretty",
            "--min-change",
            "1.5GB",
            "--max-depth",
            "2",
            "--limit",
            "10",
            "--threads",
            "4",
            "--exclude",
            "node_modules",
            "--owners",
            "-q",
        ]) {
            Command::Diff(c) => c,
            other => panic!("expected diff, got {other:?}"),
        };
        assert_eq!(c.old, PathBuf::from("old.json"));
        assert_eq!(c.new, PathBuf::from("C:\\Users"));
        assert_eq!(c.scan.path, PathBuf::from("C:\\Users"));
        assert_eq!(c.out.format, Format::Csv);
        assert_eq!(c.out.output, Some(PathBuf::from("out.csv")));
        assert!(c.out.pretty);
        assert_eq!(c.min_change, 1024 * 1024 * 1024 * 3 / 2);
        assert_eq!(c.max_depth, Some(2));
        assert_eq!(c.limit, Some(10));
        assert_eq!(c.scan.threads, Some(4));
        assert_eq!(c.scan.exclude, vec!["node_modules"]);
        assert!(c.scan.quiet && c.scan.collect_owners);

        let defaults = match parse_ok(&["diff", "old.json", "new.json"]) {
            Command::Diff(c) => c,
            other => panic!("expected diff, got {other:?}"),
        };
        assert_eq!(defaults.min_change, 0);
        assert_eq!(defaults.max_depth, None);
        assert_eq!(defaults.limit, None);
        assert_eq!(defaults.out.format, Format::Json);
    }

    #[test]
    fn mcp_options() {
        assert_eq!(
            parse_ok(&["mcp"]),
            Command::Mcp(McpCommand {
                allow_destructive: false,
                config: None
            })
        );
        assert_eq!(
            parse_ok(&["mcp", "--allow-destructive"]),
            Command::Mcp(McpCommand {
                allow_destructive: true,
                config: None
            })
        );
        assert!(parse_err(&["mcp", "--nope"]).contains("--nope"));
    }

    #[test]
    fn config_flag() {
        let c = scan_cmd(&["scan", "data", "--config"]);
        assert_eq!(c.scan.config, Some(ConfigSource::Default));
        assert_eq!(c.scan.path, PathBuf::from("data"));
        // A bare --config before the path must not swallow it.
        let c = scan_cmd(&["scan", "--config", "data"]);
        assert_eq!(c.scan.config, Some(ConfigSource::Default));
        assert_eq!(c.scan.path, PathBuf::from("data"));
        let c = scan_cmd(&["scan", "--config=my.toml", "x"]);
        assert_eq!(c.scan.config, Some(ConfigSource::File("my.toml".into())));
        let c = scan_cmd(&["scan", "--config", "my.toml", "x"]);
        assert_eq!(c.scan.config, Some(ConfigSource::File("my.toml".into())));
        assert_eq!(scan_cmd(&["scan", "x"]).scan.config, None);
        assert_eq!(
            parse_ok(&["mcp", "--config"]),
            Command::Mcp(McpCommand {
                allow_destructive: false,
                config: Some(ConfigSource::Default)
            })
        );
        assert_eq!(
            parse_ok(&["config", "a.toml"]),
            Command::Config(ConfigCommand {
                source: ConfigSource::File("a.toml".into())
            })
        );
        assert_eq!(
            parse_ok(&["config"]),
            Command::Config(ConfigCommand {
                source: ConfigSource::Default
            })
        );
    }

    #[test]
    fn usage_errors() {
        assert!(parse_err(&["scan"]).contains("missing <path>"));
        assert!(parse_err(&["scan", "a", "b"]).contains("extra argument"));
        assert!(parse_err(&["scan", "a", "--format", "xml"]).contains("json or csv"));
        assert!(parse_err(&["scan", "a", "--threads", "0"]).contains("at least 1"));
        assert!(parse_err(&["scan", "a", "--threads", "65"]).contains("between 1 and 64"));
        assert!(parse_err(&["scan", "a", "--threads"]).contains("needs a value"));
        assert!(parse_err(&["scan", "a", "--max-depth", "x"]).contains("whole number"));
        assert!(parse_err(&["scan", "a", "--quiet=yes"]).contains("does not take a value"));
        assert!(parse_err(&["scan", "a", "--bogus"]).contains("--bogus"));
        assert!(parse_err(&["scan", "a", "--min-size", "lots"]).contains("--min-size"));
        assert!(parse_err(&["top", "a", "--newer-than", "yesterday"]).contains("--newer-than"));
        assert!(parse_err(&["scan", "a", "--older-than", "soon"]).contains("--older-than"));
        assert!(
            parse_err(&[
                "scan",
                "a",
                "--newer-than",
                "2026-02-01",
                "--older-than",
                "2026-01-01"
            ])
            .contains("later")
        );
        assert!(parse_err(&["duplicates", "a", "--name", "x"]).contains("--name"));
        // Options of the other command are rejected, not silently ignored.
        assert!(parse_err(&["scan", "a", "--limit", "3"]).contains("--limit"));
        assert!(parse_err(&["top", "a", "--max-depth", "3"]).contains("--max-depth"));
        assert!(parse_err(&["top", "a", "--limit", "0"]).contains("at least 1"));
        assert!(parse_err(&["frobnicate"]).contains("unknown command"));
        assert!(parse_err(&["--frobnicate"]).contains("unknown option"));
        assert!(parse_err(&["help", "nope"]).contains("unknown command"));
        // diff has its own positional pair and options.
        assert!(parse_err(&["diff", "old.json"]).contains("old.json"));
        assert!(parse_err(&["diff", "a", "b", "c"]).contains("old.json"));
        assert!(parse_err(&["diff", "a", "b", "--min-change", "lots"]).contains("size"));
        assert!(parse_err(&["diff", "a", "b", "--limit", "0"]).contains("at least 1"));
        assert!(parse_err(&["diff", "a", "b", "--max-depth", "x"]).contains("whole number"));
        assert!(parse_err(&["diff", "a", "b", "--no-files"]).contains("--no-files"));
    }
}

//! A Model Context Protocol server over stdio (`fastdir mcp`).
//!
//! Messages are newline-delimited JSON-RPC 2.0, as the MCP stdio transport
//! specifies. Scans are kept in memory (the most recent few roots), so an
//! agent can scan once and then drill down with `get_children`, `search` and
//! friends without rescanning. Any tool given a path outside every cached
//! scan scans that folder first.
//!
//! Only read-only tools are offered by default. `recycle_path` and
//! `delete_path` exist only when the server is started with
//! `--allow-destructive`.

use crate::diff as diff_doc;
use crate::schema::{self, ScanDoc, ScanMeta, TreeShape, entry_summary};
use crate::{CliError, Scanned, resolve_root};
use fastdir_core::diff as core_diff;
use fastdir_core::duplicates::{self, DuplicateOptions, DuplicateProgress};
use fastdir_core::export::{ExportOptions, export_csv, import_json_with_meta};
use fastdir_core::filter::{Filter, parse_modified, parse_size};
use fastdir_core::model::{Node, NodeKind, NodePath, Tree};
use fastdir_core::scanner::{self, Progress, ScanOptions};
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// MCP protocol revisions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// How many scanned roots stay in memory.
const MAX_CACHED_SCANS: usize = 4;
/// Upper bound for every `limit` argument.
const MAX_LIMIT: usize = 1000;

const INSTRUCTIONS: &str = "FastDir analyzes disk usage. Call scan_path on a folder or drive \
first (it can take a while on a whole drive), then use get_children to drill down, \
largest_files for cleanup candidates (optionally filtered by name, size, extension or date), \
search to find entries by name or glob, find_duplicates for groups of identical files, and \
path_info for one entry. compare_scans shows what changed since a cached scan or a saved JSON \
scan. Results come from the in-memory scan; call scan_path again to refresh. Sizes are bytes; \
`disk_size` is the allocated size on disk.";

// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Run the server until stdin closes.
pub fn serve(allow_destructive: bool, defaults: ScanOptions) -> io::Result<()> {
    let mut server = Server::new(allow_destructive).with_scan_defaults(defaults);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg),
            Err(e) => Some(rpc_error(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {e}"),
            )),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut stdout, &reply)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A tool failure, reported to the model as an `isError` result.
struct ToolError(String);

impl From<CliError> for ToolError {
    fn from(e: CliError) -> Self {
        ToolError(e.message)
    }
}

type ToolResult = Result<Value, ToolError>;

fn tool_err<T>(msg: impl Into<String>) -> Result<T, ToolError> {
    Err(ToolError(msg.into()))
}

/// The server state: cached scans, most recently used last.
pub struct Server {
    scans: Vec<Scanned>,
    allow_destructive: bool,
    /// Scan settings from `--config`; every scan starts from these.
    scan_defaults: ScanOptions,
}

impl Server {
    pub fn new(allow_destructive: bool) -> Self {
        Self {
            scans: Vec::new(),
            allow_destructive,
            scan_defaults: ScanOptions::default(),
        }
    }

    /// Base options for every scan (per-call arguments are layered on top).
    pub fn with_scan_defaults(mut self, defaults: ScanOptions) -> Self {
        self.scan_defaults = defaults;
        self
    }

    /// Handle one JSON-RPC message; `None` for notifications.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "expected a JSON-RPC object",
            ));
        };
        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(Value::as_str);
        let (Some(id), Some(method)) = (id, method) else {
            // Notifications (`notifications/initialized`, cancellations) and
            // stray responses need no reply.
            return None;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let version = requested
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "fastdir", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({ "tools": self.tool_list() })),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(rpc_error(id, INVALID_PARAMS, "missing tool name"));
                };
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.call(name, &args) {
                    None => rpc_error(id, INVALID_PARAMS, &format!("unknown tool: {name}")),
                    Some(Ok(value)) => rpc_result(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": value.to_string() }],
                            "isError": false,
                        }),
                    ),
                    Some(Err(ToolError(message))) => rpc_result(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": message }],
                            "isError": true,
                        }),
                    ),
                }
            }
            _ => rpc_error(id, METHOD_NOT_FOUND, &format!("method not found: {method}")),
        })
    }

    fn tool_list(&self) -> Vec<Value> {
        let path = json!({ "type": "string", "description": "Absolute path of a folder, drive (e.g. C:\\) or file" });
        let limit = |default: usize| json!({ "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": default });
        let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
        let mut tools = vec![
            json!({
                "name": "scan_path",
                "description": "Scan a folder or drive and cache the result. Returns the totals and the largest entries directly inside it. Rescans if the folder was scanned before.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "limit": limit(50),
                        "exclude": { "type": "array", "items": { "type": "string" }, "description": "Names or globs to skip, e.g. node_modules or *.tmp" },
                        "follow_junctions": { "type": "boolean", "default": false },
                        "skip_hidden": { "type": "boolean", "default": false, "description": "Skip hidden and system entries" },
                        "skip_windows": { "type": "boolean", "default": false, "description": "Skip the Windows folder (%SystemRoot%) for a faster system-drive scan" },
                        "threads": { "type": "integer", "minimum": 1, "maximum": 64 },
                    },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "get_children",
                "description": "List the entries directly inside a scanned folder, largest first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "limit": limit(50),
                        "include_files": { "type": "boolean", "default": true },
                    },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "largest_files",
                "description": "The largest files anywhere under a folder, largest first. Hard links are counted once. Optional filters choose which files are ranked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "limit": limit(20),
                        "query": { "type": "string", "default": "", "description": "Name substring, or a glob when it contains * or ? (matched against the whole name)" },
                        "min_size": { "type": ["string", "integer"], "description": "Only files at least this large: bytes, or a size such as 500MB or 1.5 GB (binary units)" },
                        "extension": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Only files with one of these extensions, e.g. .iso or [\".iso\", \".img\"]" },
                        "newer_than": { "type": "string", "description": "Only files modified on/after this date (2026-01-01) or within this age (30d, 6m, 1y)" },
                        "older_than": { "type": "string", "description": "Only files modified on/before this date or older than this age" },
                    },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "search",
                "description": "Find files and folders under a folder by name. `query` is a case-insensitive substring, or a glob when it contains * or ? (matched against the whole name). Results are largest first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "query": { "type": "string", "default": "" },
                        "min_size": { "type": ["string", "integer"], "description": "Bytes, or a size such as 500MB or 1.5 GB (binary units)" },
                        "extension": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Only files with one of these extensions, e.g. .iso or [\".iso\", \".img\"]" },
                        "newer_than": { "type": "string", "description": "Only entries modified on/after this date (2026-01-01) or within this age (30d, 6m, 1y)" },
                        "older_than": { "type": "string", "description": "Only entries modified on/before this date or older than this age" },
                        "type": { "type": "string", "enum": ["any", "file", "dir"], "default": "any" },
                        "limit": limit(50),
                    },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "path_info",
                "description": "Size, item counts and last change of one file or folder, plus its share of its parent and of the scan.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "path": path },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "find_duplicates",
                "description": "Groups of byte-identical files under a folder, largest waste first. Reuses the cached scan for size grouping; only files that can still match are hashed.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "min_size": { "type": ["string", "integer"], "default": duplicates::DEFAULT_MIN_SIZE, "description": "Smallest file to hash in bytes, or a size such as 500MB or 1 GiB (default: 1 MB). Zero-byte files are skipped." },
                        "extensions": { "type": "array", "items": { "type": "string" }, "description": "Only files with one of these extensions, e.g. [\".mp4\"]" },
                        "limit": limit(100),
                    },
                    "required": ["path"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "compare_scans",
                "description": "Show what changed between two scans of the same folder: added, removed, grown and shrunk paths with their old and new sizes, largest absolute change first. `old` is a cached scan root or a saved JSON scan file; `new` is a saved JSON scan file or a folder to scan now.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "old": { "type": "string", "description": "Root of a scan cached in this session, or a saved fastdir.scan JSON file" },
                        "new": { "type": "string", "description": "Folder or drive to scan now, or a saved fastdir.scan JSON file" },
                        "min_change": { "type": ["string", "integer"], "description": "Hide changes smaller than this: bytes, or a size such as 20GB (binary units)" },
                        "max_depth": { "type": "integer", "minimum": 0, "description": "Only entries up to this many levels below the root (0 = the root row)" },
                        "limit": limit(50),
                    },
                    "required": ["old", "new"],
                },
                "annotations": read_only,
            }),
            json!({
                "name": "export_scan",
                "description": "Write a scanned folder (or a subfolder) to a JSON (fastdir.scan schema) or CSV file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": path,
                        "output": { "type": "string", "description": "File to write" },
                        "format": { "type": "string", "enum": ["json", "csv"], "default": "json" },
                        "max_depth": { "type": "integer", "minimum": 0 },
                        "include_files": { "type": "boolean", "default": true },
                    },
                    "required": ["path", "output"],
                },
                "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false },
            }),
        ];
        if self.allow_destructive {
            let destructive = json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false });
            tools.push(json!({
                "name": "recycle_path",
                "description": "Move a file or folder to the Recycle Bin / trash. The cached scan is updated.",
                "inputSchema": { "type": "object", "properties": { "path": path }, "required": ["path"] },
                "annotations": destructive,
            }));
            tools.push(json!({
                "name": "delete_path",
                "description": "Permanently delete a file or folder (cannot be undone). Requires confirm: true.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "path": path, "confirm": { "type": "boolean" } },
                    "required": ["path", "confirm"],
                },
                "annotations": destructive,
            }));
        }
        tools
    }

    /// Dispatch a tool; `None` when `name` is not offered.
    fn call(&mut self, name: &str, args: &Value) -> Option<ToolResult> {
        Some(match name {
            "scan_path" => self.scan_path(args),
            "get_children" => self.get_children(args),
            "largest_files" => self.largest_files(args),
            "search" => self.search(args),
            "path_info" => self.path_info(args),
            "find_duplicates" => self.find_duplicates(args),
            "compare_scans" => self.compare_scans(args),
            "export_scan" => self.export_scan(args),
            "recycle_path" if self.allow_destructive => self.remove_path(args, false),
            "delete_path" if self.allow_destructive => {
                if args.get("confirm").and_then(Value::as_bool) != Some(true) {
                    tool_err("delete_path permanently deletes; pass confirm: true to proceed")
                } else {
                    self.remove_path(args, true)
                }
            }
            _ => return None,
        })
    }

    // --- scans and lookup ---------------------------------------------------

    fn scan(&mut self, path: &Path, options: &ScanOptions) -> Result<usize, ToolError> {
        let scanned = scan_folder(path, options)?;
        let root = scanned.meta.root.clone();
        self.scans.retain(|s| !same_path(&s.meta.root, &root));
        if self.scans.len() >= MAX_CACHED_SCANS {
            self.scans.remove(0);
        }
        self.scans.push(scanned);
        Ok(self.scans.len() - 1)
    }

    /// The cached scan holding `path` (deepest root wins) and the node's
    /// index path in it, scanning `path` first when no scan covers it.
    fn locate(&mut self, path: &Path) -> Result<(usize, NodePath, bool), ToolError> {
        let target = absolute(path)?;
        if let Some((i, node)) = self.find_cached(&target) {
            // Mark as most recently used.
            let s = self.scans.remove(i);
            self.scans.push(s);
            return Ok((self.scans.len() - 1, node, false));
        }
        if target.is_file() {
            return tool_err(format!(
                "{} is not inside a scanned folder; call scan_path on its folder first",
                target.display()
            ));
        }
        let i = self.scan(&target, &self.scan_defaults.clone())?;
        Ok((i, Vec::new(), true))
    }

    fn find_cached(&self, target: &Path) -> Option<(usize, NodePath)> {
        self.scans
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                find_in(&s.tree, target).map(|node| (i, node, s.meta.root.components().count()))
            })
            .max_by_key(|&(_, _, depth)| depth)
            .map(|(i, node, _)| (i, node))
    }

    // --- tools ----------------------------------------------------------------

    fn scan_path(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let limit = arg_limit(args, 50)?;
        let threads = match args.get("threads") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n @ 1..=64) => Some(n as usize),
                _ => return tool_err("threads must be between 1 and 64"),
            },
        };
        let exclude = match args.get("exclude") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ToolError("exclude must be a list of strings".into()))?,
            Some(_) => return tool_err("exclude must be a list of strings"),
        };
        let explicit = ScanOptions {
            excluded_globs: exclude,
            skip_hidden_system: arg_bool(args, "skip_hidden", false)?,
            follow_junctions: arg_bool(args, "follow_junctions", false)?,
            cross_volumes: false,
            threads,
            collect_owners: false,
            skip_windows_dir: arg_bool(args, "skip_windows", false)?,
        };
        let options = crate::config::layer(&self.scan_defaults, &explicit);
        let i = self.scan(&path, &options)?;
        let s = &self.scans[i];
        let mut out = children_reply(&s.tree, &[], limit, true);
        add_meta(&mut out, &s.meta);
        Ok(out)
    }

    fn get_children(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let limit = arg_limit(args, 50)?;
        let include_files = arg_bool(args, "include_files", true)?;
        let (i, node, scanned) = self.locate(&path)?;
        let s = &self.scans[i];
        if !s.tree.get(&node).is_some_and(|n| n.is_dir()) {
            return tool_err(format!("{} is a file, not a folder", path.display()));
        }
        let mut out = children_reply(&s.tree, &node, limit, include_files);
        out["scanned_now"] = json!(scanned);
        Ok(out)
    }

    fn largest_files(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let limit = arg_limit(args, 20)?;
        let filter = mcp_filter(args)?;
        let (i, node, scanned) = self.locate(&path)?;
        let s = &self.scans[i];
        let files = s.tree.top_files_in_where(&node, limit, |node, name, ext| {
            filter.matches(name, node, ext)
        });
        Ok(json!({
            "path": s.tree.fs_path(&node).unwrap_or_default().to_string_lossy(),
            "scanned_now": scanned,
            "files": files.iter().map(|f| schema::file_entry(&s.tree, f)).collect::<Vec<_>>(),
        }))
    }

    fn search(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let limit = arg_limit(args, 50)?;
        let filter = mcp_filter(args)?;
        let kind = match arg_str(args, "type")?.as_deref() {
            None | Some("any") => None,
            Some("file") => Some(NodeKind::File),
            Some("dir") => Some(NodeKind::Dir),
            Some(other) => return tool_err(format!("type must be any, file or dir, got {other}")),
        };
        let (i, node, scanned) = self.locate(&path)?;
        let tree = &self.scans[i].tree;
        let (total, hits) = search_tree(tree, &node, &filter, kind, limit);
        Ok(json!({
            "path": tree.fs_path(&node).unwrap_or_default().to_string_lossy(),
            "scanned_now": scanned,
            "total_matches": total,
            "results": hits
                .iter()
                .map(|p| {
                    let n = tree.get(p).expect("search hit is in the tree");
                    entry_summary(
                        tree,
                        n,
                        &tree.fs_path(p).unwrap_or_default(),
                        tree.owner_at(p),
                    )
                })
                .collect::<Vec<_>>(),
        }))
    }

    fn path_info(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let (i, node_path, scanned) = self.locate(&path)?;
        let s = &self.scans[i];
        let tree = &s.tree;
        let node = tree.get(&node_path).expect("located node exists");
        let mut out = entry_summary(
            tree,
            node,
            &tree.fs_path(&node_path).unwrap_or_default(),
            tree.owner_at(&node_path),
        );
        let share = |part: u64, whole: u64| {
            if whole == 0 {
                0.0
            } else {
                (part as f64 / whole as f64 * 10_000.0).round() / 100.0
            }
        };
        if let Some((_, parent)) = node_path.split_last() {
            let parent = tree.get(parent).expect("parent exists");
            out["percent_of_parent"] = json!(share(node.size, parent.size));
        }
        out["percent_of_scan"] = json!(share(node.size, tree.root().size));
        out["scan_root"] = json!(s.meta.root.to_string_lossy());
        out["scanned_at"] = json!(s.meta.scanned_at);
        out["scanned_now"] = json!(scanned);
        if node.is_dir() {
            out["direct_children"] = json!(tree.children(node).len());
        }
        Ok(out)
    }

    fn compare_scans(&mut self, args: &Value) -> ToolResult {
        let old_arg = arg_named_path(args, "old")?;
        let new_arg = arg_named_path(args, "new")?;
        let min_change = arg_size(args, "min_change")?;
        let max_depth = match args.get("max_depth") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| ToolError("max_depth must be a whole number".into()))?
                    as usize,
            ),
        };
        let limit = arg_limit(args, 50)?;
        let old = self.load_old_side(&old_arg)?;
        let new = self.load_new_side(&new_arg)?;
        let old_tree = old.tree(&self.scans);
        let new_tree = new.tree(&self.scans);
        let diff = core_diff::build(old_tree, new_tree);
        let doc = diff_doc::document(
            &diff,
            new_tree,
            old.side(),
            new.side(),
            min_change,
            max_depth,
            Some(limit),
        );
        serde_json::to_value(&doc)
            .map_err(|e| ToolError(format!("could not serialize the comparison: {e}")))
    }

    /// `old` is a saved scan file or a cached scan root; a folder that is not
    /// cached yet is scanned once and kept, so later calls can reuse it.
    fn load_old_side(&mut self, path: &Path) -> Result<Side, ToolError> {
        let path = absolute(path)?;
        let meta =
            std::fs::metadata(&path).map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        if meta.is_file() {
            return load_saved_side(&path);
        }
        if !meta.is_dir() {
            return tool_err(format!("{}: not a file or folder", path.display()));
        }
        let i = match self
            .scans
            .iter()
            .rposition(|s| same_path(&s.meta.root, &path))
        {
            Some(i) => {
                // Keep the baseline from being evicted while `new` is loaded.
                let s = self.scans.remove(i);
                self.scans.push(s);
                self.scans.len() - 1
            }
            None => self.scan(&path, &self.scan_defaults.clone())?,
        };
        let s = &self.scans[i];
        Ok(Side::Cached {
            root: s.meta.root.clone(),
            side: diff_doc::DiffSide::new(
                s.meta.root.to_string_lossy().into_owned(),
                Some(s.meta.scanned_at.clone()),
            ),
        })
    }

    /// `new` is a saved scan file or a folder scanned now. A folder is not
    /// cached, so the baseline cached under the same root stays intact.
    fn load_new_side(&mut self, path: &Path) -> Result<Side, ToolError> {
        let path = absolute(path)?;
        let meta =
            std::fs::metadata(&path).map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        if meta.is_file() {
            return load_saved_side(&path);
        }
        if !meta.is_dir() {
            return tool_err(format!("{}: not a file or folder", path.display()));
        }
        let scanned = scan_folder(&path, &self.scan_defaults)?;
        let side = diff_doc::DiffSide::new(
            scanned.meta.root.to_string_lossy().into_owned(),
            Some(scanned.meta.scanned_at.clone()),
        );
        Ok(Side::Local {
            tree: Box::new(scanned.tree),
            side,
        })
    }

    fn find_duplicates(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let min_size = match args.get("min_size") {
            None | Some(Value::Null) => duplicates::DEFAULT_MIN_SIZE,
            Some(Value::Number(n)) => n
                .as_u64()
                .ok_or_else(|| ToolError("min_size must be a whole number of bytes".into()))?,
            Some(Value::String(s)) => parse_size(s).ok_or_else(|| {
                ToolError(format!(
                    "min_size expects a number of bytes or a size such as 500MB, got \"{s}\""
                ))
            })?,
            Some(_) => {
                return tool_err("min_size must be a number of bytes or a size string");
            }
        };
        let extensions = match args.get("extensions") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(one)) => vec![one.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ToolError("extensions must be a list of strings".into()))?,
            Some(_) => return tool_err("extensions must be a list of strings"),
        };
        let limit = arg_limit(args, 100)?;
        let (i, node, scanned) = self.locate(&path)?;
        let s = &self.scans[i];
        if !s.tree.get(&node).is_some_and(|n| n.is_dir()) {
            return tool_err(format!("{} is a file, not a folder", path.display()));
        }
        let options = DuplicateOptions {
            min_size,
            extensions,
            limit: Some(limit),
            threads: None,
        };
        let report = duplicates::find(
            &s.tree,
            &node,
            &options,
            Arc::new(DuplicateProgress::default()),
        );
        Ok(json!({
            "path": s.tree.fs_path(&node).unwrap_or_default().to_string_lossy(),
            "scanned_now": scanned,
            "min_size": min_size,
            "limit": limit,
            "candidates": report.candidates,
            "total_groups": report.total_groups,
            "total_wasted": report.total_wasted,
            "hashed_bytes": report.bytes_hashed,
            "hash_errors": report.hash_errors,
            "elapsed_ms": report.elapsed.as_millis() as u64,
            "groups": report.groups.iter().map(|group| json!({
                "size": group.size,
                "hash": group.hash,
                "wasted": group.wasted(),
                "files": group.files.iter().map(|file| json!({
                    "path": file.path.to_string_lossy(),
                    "modified": schema::iso_time(file.modified),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }))
    }

    fn export_scan(&mut self, args: &Value) -> ToolResult {
        let path = arg_path(args)?;
        let Some(output) = arg_str(args, "output")? else {
            return tool_err("missing output file");
        };
        let format = arg_str(args, "format")?.unwrap_or_else(|| "json".into());
        let max_depth = match args.get("max_depth") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| ToolError("max_depth must be a whole number".into()))?
                    as usize,
            ),
        };
        let include_files = arg_bool(args, "include_files", true)?;
        let (i, node, scanned) = self.locate(&path)?;
        let s = &self.scans[i];
        if !s.tree.get(&node).is_some_and(|n| n.is_dir()) {
            return tool_err(format!("{} is a file, not a folder", path.display()));
        }
        let output = PathBuf::from(output);
        let file = std::fs::File::create(&output)
            .map_err(|e| ToolError(format!("{}: {e}", output.display())))?;
        let mut w = io::BufWriter::new(file);
        let written = match format.as_str() {
            "json" => serde_json::to_writer(
                &mut w,
                &ScanDoc {
                    meta: &s.meta,
                    tree: &s.tree,
                    subtree: &node,
                    shape: TreeShape {
                        max_depth,
                        include_files,
                    },
                    filter: None,
                },
            )
            .map_err(io::Error::from),
            "csv" => export_csv(
                &s.tree,
                &node,
                &ExportOptions {
                    max_depth,
                    include_files,
                },
                &mut w,
            )
            .map(|_| ()),
            other => return tool_err(format!("format must be json or csv, got {other}")),
        };
        written
            .and_then(|_| w.flush())
            .map_err(|e| ToolError(format!("{}: {e}", output.display())))?;
        Ok(json!({
            "path": s.tree.fs_path(&node).unwrap_or_default().to_string_lossy(),
            "output": std::path::absolute(&output).unwrap_or(output).to_string_lossy(),
            "format": format,
            "scanned_now": scanned,
        }))
    }

    fn remove_path(&mut self, args: &Value, permanent: bool) -> ToolResult {
        let path = absolute(&arg_path(args)?)?;
        if path.parent().is_none() {
            return tool_err("refusing to remove a drive or file-system root");
        }
        std::fs::symlink_metadata(&path)
            .map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        let freed = self
            .find_cached(&path)
            .and_then(|(i, node)| self.scans[i].tree.get(&node).map(|n| n.size));
        fastdir_core::fsops::delete(&path, permanent)
            .map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        // Keep every cached scan in step: drop scans rooted at or below the
        // removed path, and remove the node from scans containing it.
        self.scans
            .retain(|s| !s.meta.root.starts_with(&path) && !same_path(&s.meta.root, &path));
        for s in &mut self.scans {
            if let Some(node) = find_in(&s.tree, &path).filter(|n| !n.is_empty()) {
                s.tree.remove(&node);
            }
        }
        Ok(json!({
            "path": path.to_string_lossy(),
            "action": if permanent { "deleted" } else { "recycled" },
            "freed_bytes": freed,
        }))
    }
}

/// One side of a `compare_scans` comparison: a tree held here, or a tree in
/// the server's scan cache (found by root when the diff is built).
enum Side {
    Local {
        tree: Box<Tree>,
        side: diff_doc::DiffSide,
    },
    Cached {
        root: PathBuf,
        side: diff_doc::DiffSide,
    },
}

impl Side {
    fn side(&self) -> &diff_doc::DiffSide {
        match self {
            Self::Local { side, .. } | Self::Cached { side, .. } => side,
        }
    }

    fn tree<'a>(&'a self, scans: &'a [Scanned]) -> &'a Tree {
        match self {
            Self::Local { tree, .. } => tree,
            Self::Cached { root, .. } => {
                let i = scans
                    .iter()
                    .rposition(|s| same_path(&s.meta.root, root))
                    .expect("the cached baseline is still present");
                &scans[i].tree
            }
        }
    }
}

fn load_saved_side(path: &Path) -> Result<Side, ToolError> {
    let shown = path.display();
    let file = std::fs::File::open(path).map_err(|e| ToolError(format!("{shown}: {e}")))?;
    let imported = import_json_with_meta(io::BufReader::new(file))
        .map_err(|e| ToolError(format!("{shown}: {e}")))?;
    let side = diff_doc::DiffSide::new(
        imported.tree.root_path.to_string_lossy().into_owned(),
        schema::iso_time(imported.scanned_at),
    );
    Ok(Side::Local {
        tree: Box::new(imported.tree),
        side,
    })
}

/// Scan `path` without touching the cache.
fn scan_folder(path: &Path, options: &ScanOptions) -> Result<Scanned, ToolError> {
    let root = resolve_root(path)?;
    let progress = Arc::new(Progress::default());
    let scanned_at = schema::now_iso();
    let result = scanner::scan(root.clone(), progress.clone(), options);
    Ok(Scanned {
        tree: result.tree,
        meta: ScanMeta {
            root,
            scanned_at,
            elapsed_ms: result.elapsed.as_millis() as u64,
            errors: progress.errors.load(Ordering::Relaxed),
        },
    })
}

/// `{ entry, children, total_children, omitted }` for the folder at `node`.
fn children_reply(tree: &Tree, node: &[u32], limit: usize, include_files: bool) -> Value {
    let dir = tree.get(node).expect("folder exists");
    let base = tree.fs_path(node).unwrap_or_default();
    let children: Vec<(usize, &Node)> = tree
        .children(dir)
        .iter()
        .enumerate()
        .filter(|(_, c)| include_files || c.is_dir())
        .collect();
    let shown: Vec<Value> = children
        .iter()
        .take(limit)
        .map(|(i, c)| entry_summary(tree, c, &base.join(tree.name(c)), tree.child_owner(dir, *i)))
        .collect();
    let omitted = &children[shown.len()..];
    json!({
        "entry": entry_summary(tree, dir, &base, tree.owner_at(node)),
        "children": shown,
        "total_children": children.len(),
        "omitted": {
            "count": omitted.len(),
            "size": omitted.iter().map(|(_, c)| c.size).sum::<u64>(),
        },
    })
}

fn add_meta(out: &mut Value, meta: &ScanMeta) {
    out["root"] = json!(meta.root.to_string_lossy());
    out["scanned_at"] = json!(meta.scanned_at);
    out["elapsed_ms"] = json!(meta.elapsed_ms);
    out["errors"] = json!(meta.errors);
}

/// Every node under `root` (excluding `root`) that passes `filter`. Returns
/// the match count and the `limit` largest matches, largest first.
fn search_tree(
    tree: &Tree,
    root: &[u32],
    filter: &Filter,
    kind: Option<NodeKind>,
    limit: usize,
) -> (u64, Vec<NodePath>) {
    let mut heap: BinaryHeap<Reverse<(u64, NodePath)>> = BinaryHeap::new();
    let mut total = 0u64;
    let Some(start) = tree.get(root) else {
        return (0, Vec::new());
    };
    let mut stack = vec![(root.to_vec(), start)];
    while let Some((path, dir)) = stack.pop() {
        for (i, child) in tree.children(dir).iter().enumerate() {
            let mut child_path = path.clone();
            child_path.push(i as u32);
            let kind_ok = kind.is_none_or(|k| child.kind() == k);
            if kind_ok && filter.matches(tree.name(child), child, tree.ext_name(child)) {
                total += 1;
                heap.push(Reverse((child.size, child_path.clone())));
                if heap.len() > limit {
                    heap.pop();
                }
            }
            if child.is_dir() {
                stack.push((child_path, child));
            }
        }
    }
    let mut hits: Vec<(u64, NodePath)> = heap.into_iter().map(|Reverse(h)| h).collect();
    hits.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    (total, hits.into_iter().map(|(_, p)| p).collect())
}

fn absolute(path: &Path) -> Result<PathBuf, ToolError> {
    std::path::absolute(path).map_err(|e| ToolError(format!("{}: {e}", path.display())))
}

/// Paths compare case-insensitively on Windows, exactly elsewhere.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str()) || a.components().eq(b.components())
    } else {
        a.components().eq(b.components())
    }
}

/// `Tree::find`, tolerating a different letter case in the root on Windows.
fn find_in(tree: &Tree, target: &Path) -> Option<NodePath> {
    if same_path(&tree.root_path, target) {
        return Some(Vec::new());
    }
    if let Some(found) = tree.find(target) {
        return Some(found);
    }
    if cfg!(windows) {
        let root = tree.root_path.to_string_lossy().to_lowercase();
        let t = target.to_string_lossy();
        if t.to_lowercase().starts_with(&root) && t.is_char_boundary(root.len()) {
            let rel = t[root.len()..].trim_start_matches(['\\', '/']);
            return tree.find(&tree.root_path.join(rel));
        }
    }
    None
}

fn arg_str(args: &Value, name: &str) -> Result<Option<String>, ToolError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => tool_err(format!("{name} must be a string")),
    }
}

fn arg_path(args: &Value) -> Result<PathBuf, ToolError> {
    match arg_str(args, "path")? {
        Some(p) if !p.trim().is_empty() => Ok(crate::args::normalize_path(&p)),
        _ => tool_err("missing path"),
    }
}

/// A required path argument named `name` (`old` / `new` for `compare_scans`).
fn arg_named_path(args: &Value, name: &str) -> Result<PathBuf, ToolError> {
    match arg_str(args, name)? {
        Some(p) if !p.trim().is_empty() => Ok(crate::args::normalize_path(&p)),
        _ => tool_err(format!("missing {name}")),
    }
}

/// A byte size given as an integer or as a string such as `20GB`.
fn arg_size(args: &Value, name: &str) -> Result<u64, ToolError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| ToolError(format!("{name} must be a whole number of bytes"))),
        Some(Value::String(s)) => parse_size(s).ok_or_else(|| {
            ToolError(format!(
                "{name} must be a size such as 500MB or 1.5GB, got {s:?}"
            ))
        }),
        Some(_) => tool_err(format!("{name} must be a number of bytes or a size string")),
    }
}

/// Build the shared name/size/extension/date filter from a tool's arguments.
fn mcp_filter(args: &Value) -> Result<Filter, ToolError> {
    let query = arg_str(args, "query")?
        .filter(|q| !q.trim().is_empty())
        .or(arg_str(args, "name")?)
        .unwrap_or_default();
    let min_size = arg_size(args, "min_size")?;
    let mut extensions = Vec::new();
    arg_string_list(args, "extension", &mut extensions)?;
    arg_string_list(args, "extensions", &mut extensions)?;
    arg_string_list(args, "ext", &mut extensions)?;
    let newer_than = arg_modified(args, "newer_than", false)?;
    let older_than = arg_modified(args, "older_than", true)?;
    if let (Some(after), Some(before)) = (newer_than, older_than)
        && after > before
    {
        return tool_err("newer_than is later than older_than");
    }
    Ok(Filter::with_extensions(
        &query,
        min_size,
        &extensions,
        newer_than,
        older_than,
    ))
}

/// A string or an array of strings, appended to `out`.
fn arg_string_list(args: &Value, name: &str, out: &mut Vec<String>) -> Result<(), ToolError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(one)) => {
            out.push(one.clone());
            Ok(())
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item.as_str() {
                    Some(item) => out.push(item.to_string()),
                    None => {
                        return tool_err(format!("{name} must be a string or a list of strings"));
                    }
                }
            }
            Ok(())
        }
        Some(_) => tool_err(format!("{name} must be a string or a list of strings")),
    }
}

/// A date or age argument (`newer_than` / `older_than`).
fn arg_modified(args: &Value, name: &str, end: bool) -> Result<Option<i64>, ToolError> {
    match arg_str(args, name)? {
        Some(raw) if !raw.trim().is_empty() => parse_modified(&raw, end).map(Some).ok_or_else(|| {
            ToolError(format!(
                "{name} must be a date such as 2026-01-01 or an age such as 30d, 6m or 1y, got {raw:?}"
            ))
        }),
        _ => Ok(None),
    }
}

fn arg_bool(args: &Value, name: &str, default: bool) -> Result<bool, ToolError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => tool_err(format!("{name} must be true or false")),
    }
}

fn arg_limit(args: &Value, default: usize) -> Result<usize, ToolError> {
    match args.get("limit") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => Ok((n as usize).min(MAX_LIMIT)),
            _ => tool_err("limit must be a whole number of at least 1"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fastdir-mcp-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("docs")).unwrap();
            std::fs::write(dir.join("docs").join("a.txt"), vec![b'a'; 100]).unwrap();
            std::fs::write(dir.join("docs").join("b.md"), vec![b'b'; 50]).unwrap();
            std::fs::write(dir.join("big.iso"), vec![0u8; 1000]).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn call(server: &mut Server, name: &str, args: Value) -> (bool, Value) {
        let reply = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": name, "arguments": args },
            }))
            .expect("reply");
        assert_eq!(reply["id"], 7);
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        let is_error = result["isError"].as_bool().unwrap();
        let value = if is_error {
            json!(text)
        } else {
            serde_json::from_str(text).unwrap()
        };
        (is_error, value)
    }

    fn ok(server: &mut Server, name: &str, args: Value) -> Value {
        let (is_error, v) = call(server, name, args);
        assert!(!is_error, "{name} failed: {v}");
        v
    }

    #[test]
    fn handshake_and_tool_list() {
        let mut s = Server::new(false);
        let init = s
            .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }))
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "fastdir");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        let unknown_version = s
            .handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }))
            .unwrap();
        assert_eq!(
            unknown_version["result"]["protocolVersion"],
            PROTOCOL_VERSIONS[0]
        );

        assert!(
            s.handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .is_none()
        );
        assert_eq!(
            s.handle(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }))
                .unwrap()["result"],
            json!({})
        );

        let list = s
            .handle(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }))
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "scan_path",
                "get_children",
                "largest_files",
                "search",
                "path_info",
                "find_duplicates",
                "compare_scans",
                "export_scan"
            ]
        );

        let missing = s
            .handle(json!({ "jsonrpc": "2.0", "id": 5, "method": "nope" }))
            .unwrap();
        assert_eq!(missing["error"]["code"], METHOD_NOT_FOUND);
        let tool = s
            .handle(json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": { "name": "recycle_path", "arguments": { "path": "x" } } }))
            .unwrap();
        assert_eq!(tool["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn destructive_tools_are_opt_in() {
        let s = Server::new(true);
        let names: Vec<String> = s
            .tool_list()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"recycle_path".to_string()));
        assert!(names.contains(&"delete_path".to_string()));
    }

    #[test]
    fn read_only_tools() {
        let dir = TempDir::new("ro");
        let root = dir.0.to_string_lossy().to_string();
        let mut s = Server::new(false);

        let scan = ok(&mut s, "scan_path", json!({ "path": root, "limit": 1 }));
        assert_eq!(scan["entry"]["size"], 1150);
        assert_eq!(scan["entry"]["files"], 3);
        assert_eq!(scan["children"].as_array().unwrap().len(), 1);
        assert_eq!(scan["children"][0]["name"], "big.iso");
        assert_eq!(scan["total_children"], 2);
        assert_eq!(scan["omitted"]["count"], 1);
        assert_eq!(scan["omitted"]["size"], 150);

        let docs = dir.0.join("docs").to_string_lossy().to_string();
        let children = ok(&mut s, "get_children", json!({ "path": docs }));
        assert_eq!(children["scanned_now"], false);
        assert_eq!(children["children"][0]["name"], "a.txt");
        assert_eq!(children["children"][0]["extension"], ".txt");

        let top = ok(&mut s, "largest_files", json!({ "path": root, "limit": 2 }));
        let names: Vec<&str> = top["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["big.iso", "a.txt"]);
        let top_docs = ok(&mut s, "largest_files", json!({ "path": docs }));
        assert_eq!(top_docs["files"].as_array().unwrap().len(), 2);
        let top_iso = ok(
            &mut s,
            "largest_files",
            json!({ "path": root, "extension": ".iso", "min_size": "500" }),
        );
        assert_eq!(top_iso["files"].as_array().unwrap().len(), 1);
        assert_eq!(top_iso["files"][0]["name"], "big.iso");
        let top_two = ok(
            &mut s,
            "largest_files",
            json!({ "path": root, "extension": [".iso", ".md"] }),
        );
        let names: Vec<&str> = top_two["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["big.iso", "b.md"]);
        let top_md = ok(
            &mut s,
            "largest_files",
            json!({ "path": root, "query": "*.md" }),
        );
        assert_eq!(top_md["files"][0]["name"], "b.md");
        let top_new = ok(
            &mut s,
            "largest_files",
            json!({ "path": root, "newer_than": "2000-01-01" }),
        );
        assert_eq!(top_new["files"].as_array().unwrap().len(), 3);
        let top_old = ok(
            &mut s,
            "largest_files",
            json!({ "path": root, "older_than": "1y" }),
        );
        assert!(top_old["files"].as_array().unwrap().is_empty());
        let (err, _) = call(
            &mut s,
            "largest_files",
            json!({ "path": root, "newer_than": "soon" }),
        );
        assert!(err);

        let found = ok(&mut s, "search", json!({ "path": root, "query": "*.md" }));
        assert_eq!(found["total_matches"], 1);
        assert_eq!(found["results"][0]["name"], "b.md");
        let dirs = ok(&mut s, "search", json!({ "path": root, "type": "dir" }));
        assert_eq!(dirs["total_matches"], 1);
        let big = ok(&mut s, "search", json!({ "path": root, "min_size": 500 }));
        assert_eq!(big["total_matches"], 1);
        let recent = ok(
            &mut s,
            "search",
            json!({ "path": root, "newer_than": "2000-01-01" }),
        );
        assert_eq!(recent["total_matches"], 4);
        let old = ok(
            &mut s,
            "search",
            json!({ "path": root, "older_than": "1y" }),
        );
        assert_eq!(old["total_matches"], 0);
        let (err, _) = call(
            &mut s,
            "search",
            json!({ "path": root, "min_size": "lots" }),
        );
        assert!(err);

        let info = ok(
            &mut s,
            "path_info",
            json!({ "path": dir.0.join("big.iso").to_string_lossy() }),
        );
        assert_eq!(info["size"], 1000);
        assert_eq!(info["type"], "file");
        assert_eq!(info["percent_of_parent"], 86.96);

        let out = dir.0.join("export.json");
        ok(
            &mut s,
            "export_scan",
            json!({ "path": docs, "output": out.to_string_lossy() }),
        );
        let doc: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(doc["schema"], "fastdir.scan");
        assert_eq!(doc["tree"]["name"], "docs");
        assert_eq!(doc["tree"]["size"], 150);
    }

    #[test]
    fn compare_scans_against_a_cached_scan_and_a_saved_file() {
        let dir = TempDir::new("cmp");
        let root = dir.0.to_string_lossy().to_string();
        let mut s = Server::new(false);
        ok(&mut s, "scan_path", json!({ "path": root }));

        // Grow a.txt from 100 to 600 bytes.
        std::fs::write(dir.0.join("docs").join("a.txt"), vec![b'a'; 600]).unwrap();
        let v = ok(&mut s, "compare_scans", json!({ "old": root, "new": root }));
        assert_eq!(v["schema"], "fastdir.diff");
        assert_eq!(v["version"], 1);
        assert_eq!(v["old"]["root"].as_str(), Some(root.as_str()));
        assert_eq!(v["total"]["delta"], 500);
        let entries = v["entries"].as_array().unwrap();
        let a = entries
            .iter()
            .find(|e| e["path"].as_str().unwrap().ends_with("a.txt"))
            .expect("a.txt changed");
        assert_eq!(a["status"], "changed");
        assert_eq!(a["type"], "file");
        assert_eq!(a["old_size"], 100);
        assert_eq!(a["new_size"], 600);
        assert_eq!(a["delta"], 500);
        // The cached baseline survives the scan of the new side.
        let after = ok(&mut s, "path_info", json!({ "path": root }));
        assert_eq!(after["size"], 1150, "the baseline was not replaced");

        // Save the scan, then compare that file with the folder now.
        let saved =
            std::env::temp_dir().join(format!("fastdir-mcp-compare-{}.json", std::process::id()));
        ok(
            &mut s,
            "export_scan",
            json!({ "path": root, "output": saved.to_string_lossy() }),
        );
        std::fs::write(dir.0.join("docs").join("b.md"), vec![b'b'; 250]).unwrap();
        let v = ok(
            &mut s,
            "compare_scans",
            json!({ "old": saved.to_string_lossy(), "new": root }),
        );
        assert_eq!(v["total"]["delta"], 700);
        let b = v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["path"].as_str().unwrap().ends_with("b.md"))
            .expect("b.md changed");
        assert_eq!(
            (b["old_size"].as_u64(), b["new_size"].as_u64()),
            (Some(50), Some(250))
        );

        // min_change hides rows; total is unchanged.
        let hidden = ok(
            &mut s,
            "compare_scans",
            json!({ "old": saved.to_string_lossy(), "new": root, "min_change": "1KB" }),
        );
        assert_eq!(hidden["total"]["delta"], 700);
        assert!(hidden["entries"].as_array().unwrap().is_empty());
        let limited = ok(
            &mut s,
            "compare_scans",
            json!({ "old": saved.to_string_lossy(), "new": root, "limit": 1 }),
        );
        assert_eq!(limited["entries"].as_array().unwrap().len(), 1);

        let (err, msg) = call(
            &mut s,
            "compare_scans",
            json!({ "old": "nope.json", "new": root }),
        );
        assert!(err);
        assert!(msg.as_str().unwrap().contains("nope.json"));
        let _ = std::fs::remove_file(&saved);
    }

    #[test]
    fn unscanned_folders_are_scanned_on_demand() {
        let dir = TempDir::new("auto");
        let mut s = Server::new(false);
        let v = ok(
            &mut s,
            "get_children",
            json!({ "path": dir.0.join("docs").to_string_lossy() }),
        );
        assert_eq!(v["scanned_now"], true);
        assert_eq!(v["entry"]["size"], 150);
        let (err, msg) = call(
            &mut s,
            "get_children",
            json!({ "path": dir.0.join("missing").to_string_lossy() }),
        );
        assert!(err);
        assert!(msg.as_str().unwrap().contains("not found"));
    }

    #[test]
    fn find_duplicates_groups_identical_files() {
        let dir = TempDir::new("dupes");
        std::fs::write(dir.0.join("one.bin"), vec![7u8; 2000]).unwrap();
        std::fs::write(dir.0.join("two.bin"), vec![7u8; 2000]).unwrap();
        std::fs::write(dir.0.join("other.bin"), vec![8u8; 2000]).unwrap();
        let root = dir.0.to_string_lossy().to_string();
        let mut s = Server::new(false);

        let found = ok(
            &mut s,
            "find_duplicates",
            json!({ "path": root, "min_size": 1 }),
        );
        assert_eq!(found["scanned_now"], true);
        assert_eq!(found["candidates"], 6);
        assert_eq!(found["total_groups"], 1);
        assert_eq!(found["total_wasted"], 2000);
        let group = &found["groups"][0];
        assert_eq!(group["size"], 2000);
        assert_eq!(group["wasted"], 2000);
        assert_eq!(group["files"].as_array().unwrap().len(), 2);
        let names: Vec<&str> = group["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                f["path"]
                    .as_str()
                    .unwrap()
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap()
            })
            .collect();
        assert_eq!(names, ["one.bin", "two.bin"]);

        // A cached scan is reused, and both filters are applied.
        let filtered = ok(
            &mut s,
            "find_duplicates",
            json!({ "path": root, "min_size": 1, "extensions": [".iso"] }),
        );
        assert_eq!(filtered["scanned_now"], false);
        assert_eq!(filtered["candidates"], 1);
        assert_eq!(filtered["total_groups"], 0);

        let (err, msg) = call(
            &mut s,
            "find_duplicates",
            json!({ "path": root, "extensions": 3 }),
        );
        assert!(err);
        assert!(msg.as_str().unwrap().contains("extensions"));
    }

    #[test]
    fn delete_updates_the_cached_scan() {
        let dir = TempDir::new("rm");
        let root = dir.0.to_string_lossy().to_string();
        let mut s = Server::new(true);
        ok(&mut s, "scan_path", json!({ "path": root }));
        let target = dir.0.join("docs").join("b.md");
        let (err, msg) = call(
            &mut s,
            "delete_path",
            json!({ "path": target.to_string_lossy() }),
        );
        assert!(err, "delete without confirm must fail");
        assert!(msg.as_str().unwrap().contains("confirm"));
        let v = ok(
            &mut s,
            "delete_path",
            json!({ "path": target.to_string_lossy(), "confirm": true }),
        );
        assert_eq!(v["freed_bytes"], 50);
        assert!(!target.exists());
        let info = ok(&mut s, "path_info", json!({ "path": root }));
        assert_eq!(info["size"], 1100);
        assert_eq!(info["scanned_now"], false);
    }
}

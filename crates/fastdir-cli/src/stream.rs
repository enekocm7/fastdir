//! `--format ndjson`: newline-delimited JSON, one record per line.
//!
//! `scan` writes each entry while the scan runs, as soon as it is final, so
//! a script can start on the first folders long before a drive scan ends
//! and never has to hold one huge document in memory. `top` cannot rank
//! before the scan ends, but writes one file per line too.
//!
//! Every stream is `start`, then the data records, then `end`. A stream
//! without an `end` line was cut short (Ctrl+C or an error). The shape is
//! versioned like the JSON documents and documented in the README; it is
//! separate from (and does not change) the `fastdir.scan` / `fastdir.top`
//! schemas.

use crate::schema::{self, SCHEMA_VERSION, ScanMeta, iso_time, kind_label};
use fastdir_core::filter::Filter;
use fastdir_core::model::{FileEntry, Node, NodeKind, Tree, extension_of};
use fastdir_core::scanner::{ObservedEntry, Progress, ScanObserver};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SCAN_STREAM_SCHEMA: &str = "fastdir.scan.ndjson";
pub const TOP_STREAM_SCHEMA: &str = "fastdir.top.ndjson";

/// How long written lines may sit in the buffer before they are flushed,
/// so a slow scan still reaches the reader promptly.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// One `entry` line: the fields of a `fastdir.scan` entry without
/// `children` / `truncated`, plus `depth` (and `extension` for files).
#[derive(Serialize)]
struct EntryRecord<'a> {
    record: &'static str,
    name: &'a str,
    path: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    depth: usize,
    size: u64,
    disk_size: u64,
    items: u64,
    files: u32,
    folders: u32,
    modified: Option<String>,
    error: bool,
    hard_link: bool,
    /// Files only (`null` when the name has none); absent for folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    extension: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<&'a str>,
}

/// Append one `entry` line for `node` to `buf`.
fn push_entry(
    buf: &mut Vec<u8>,
    name: &str,
    path: &Path,
    depth: usize,
    node: &Node,
    owner: Option<&str>,
) {
    let path = path.to_string_lossy();
    let record = EntryRecord {
        record: "entry",
        name,
        path: &path,
        kind: kind_label(node.kind()),
        depth,
        size: node.size,
        disk_size: node.disk_size,
        items: node.items(),
        files: node.files,
        folders: node.dirs,
        modified: iso_time(node.modified()),
        error: node.error(),
        hard_link: node.hard_link(),
        extension: (node.kind() == NodeKind::File).then(|| extension_of(name)),
        owner,
    };
    // Serializing plain fields into a Vec cannot fail.
    let _ = serde_json::to_writer(&mut *buf, &record);
    buf.push(b'\n');
}

struct Output {
    w: Box<dyn Write + Send>,
    last_flush: Instant,
    /// Entry lines written so far.
    entries: u64,
    /// The first write error; later writes are skipped.
    error: Option<io::Error>,
}

/// Streams a `scan` as NDJSON. It is the scan's [`ScanObserver`], so it is
/// called from the scan threads; lines are formatted outside the lock and
/// only the write itself is serialised.
pub struct ScanStream<'a> {
    root: PathBuf,
    max_depth: Option<usize>,
    include_files: bool,
    filter: Option<&'a Filter>,
    /// Cancelled when the output fails (e.g. the reader closed the pipe), so
    /// the scan stops instead of running on for nothing.
    progress: OnceLock<Arc<Progress>>,
    out: Mutex<Output>,
}

impl<'a> ScanStream<'a> {
    pub fn new(
        root: PathBuf,
        max_depth: Option<usize>,
        include_files: bool,
        filter: Option<&'a Filter>,
        w: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            root,
            max_depth,
            include_files,
            filter,
            progress: OnceLock::new(),
            out: Mutex::new(Output {
                w,
                last_flush: Instant::now(),
                entries: 0,
                error: None,
            }),
        }
    }

    /// Write the `start` line. Call once, before the scan.
    pub fn start(&self, scanned_at: &str, progress: &Arc<Progress>) {
        let _ = self.progress.set(progress.clone());
        let line = json!({
            "schema": SCAN_STREAM_SCHEMA,
            "version": SCHEMA_VERSION,
            "fastdir_version": env!("CARGO_PKG_VERSION"),
            "root": self.root.to_string_lossy(),
            "scanned_at": scanned_at,
        });
        self.write(&line_bytes("start", &line), 0, true);
    }

    /// Write the `end` line and flush. Returns the first write error.
    pub fn finish(&self, meta: &ScanMeta, tree: &Tree) -> io::Result<()> {
        let entries = self.lock().entries;
        let line = end_record(meta, tree, entries);
        self.write(&line_bytes("end", &line), 0, true);
        let mut out = self.lock();
        if out.error.is_none()
            && let Err(e) = out.w.flush()
        {
            out.error = Some(e);
        }
        out.error.take().map_or(Ok(()), Err)
    }

    /// The first write error, if any.
    pub fn take_error(&self) -> Option<io::Error> {
        self.lock().error.take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Output> {
        self.out.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Levels below the scan root (`0` for the root itself).
    fn depth(&self, path: &Path) -> usize {
        path.strip_prefix(&self.root)
            .map_or(0, |rel| rel.components().count())
    }

    fn within_depth(&self, depth: usize) -> bool {
        self.max_depth.is_none_or(|max| depth <= max)
    }

    fn wanted(&self, name: &str, node: &Node) -> bool {
        if !node.is_dir() && !self.include_files {
            return false;
        }
        self.filter.is_none_or(|filter| {
            let ext = (node.kind() == NodeKind::File)
                .then(|| extension_of(name))
                .flatten();
            filter.matches(name, node, ext.as_deref())
        })
    }

    fn write(&self, bytes: &[u8], entries: u64, flush: bool) {
        if bytes.is_empty() {
            return;
        }
        let mut out = self.lock();
        if out.error.is_some() {
            return;
        }
        let mut res = out.w.write_all(bytes);
        if res.is_ok() && (flush || out.last_flush.elapsed() >= FLUSH_INTERVAL) {
            res = out.w.flush();
            out.last_flush = Instant::now();
        }
        match res {
            Ok(()) => out.entries += entries,
            Err(e) => {
                out.error = Some(e);
                if let Some(progress) = self.progress.get() {
                    progress.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

impl ScanObserver for ScanStream<'_> {
    fn entries(&self, dir: &Path, entries: &[ObservedEntry<'_>]) {
        let depth = self.depth(dir) + 1;
        if !self.within_depth(depth) {
            return;
        }
        let mut buf = Vec::new();
        let mut count = 0;
        for entry in entries {
            if !self.wanted(entry.name, entry.node) {
                continue;
            }
            push_entry(
                &mut buf,
                entry.name,
                &dir.join(entry.name),
                depth,
                entry.node,
                entry.owner,
            );
            count += 1;
        }
        self.write(&buf, count, false);
    }

    fn dir_done(&self, path: &Path, node: &Node, owner: Option<&str>) {
        let depth = self.depth(path);
        if !self.within_depth(depth) {
            return;
        }
        // The root is named by its full path, as in `fastdir.scan`.
        let name = if depth == 0 {
            path.to_string_lossy()
        } else {
            path.file_name()
                .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        };
        if !self.wanted(&name, node) {
            return;
        }
        let mut buf = Vec::new();
        push_entry(&mut buf, &name, path, depth, node, owner);
        self.write(&buf, 1, false);
    }
}

/// A line other than `entry`: `record` first, then the fields of the
/// object `fields`.
#[derive(Serialize)]
struct Tagged<'a> {
    record: &'static str,
    #[serde(flatten)]
    fields: &'a Value,
}

fn line_bytes(record: &'static str, fields: &Value) -> Vec<u8> {
    let mut buf = serde_json::to_vec(&Tagged { record, fields }).unwrap_or_default();
    buf.push(b'\n');
    buf
}

fn total(tree: &Tree) -> Value {
    let root = tree.root();
    json!({
        "size": root.size,
        "disk_size": root.disk_size,
        "items": root.items(),
        "files": root.files,
        "folders": root.dirs,
    })
}

/// The `end` line of a `scan` stream.
fn end_record(meta: &ScanMeta, tree: &Tree, entries: u64) -> Value {
    let unreadable: Vec<String> = tree
        .unreadable_dirs()
        .iter()
        .filter_map(|p| tree.fs_path(p))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    json!({
        "elapsed_ms": meta.elapsed_ms,
        "errors": meta.errors,
        "unreadable": unreadable,
        "entries": entries,
        "total": total(tree),
    })
}

/// `fastdir top --format ndjson`: `start`, one `file` line per file
/// (largest first, the fields of a `fastdir.top` file), then `end`.
pub fn write_top(
    w: &mut dyn Write,
    meta: &ScanMeta,
    tree: &Tree,
    files: &[FileEntry],
    limit: usize,
) -> io::Result<()> {
    let start = json!({
        "schema": TOP_STREAM_SCHEMA,
        "version": SCHEMA_VERSION,
        "fastdir_version": env!("CARGO_PKG_VERSION"),
        "root": meta.root.to_string_lossy(),
        "scanned_at": meta.scanned_at,
    });
    w.write_all(&line_bytes("start", &start))?;
    for f in files {
        w.write_all(&line_bytes("file", &schema::file_entry(tree, f)))?;
    }
    let end = json!({
        "elapsed_ms": meta.elapsed_ms,
        "errors": meta.errors,
        "limit": limit,
        "count": files.len(),
        "total": total(tree),
    });
    w.write_all(&line_bytes("end", &end))?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample_tree;
    use fastdir_core::scanner::replay;

    /// A `Write` that hands its bytes back to the test.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn meta() -> ScanMeta {
        ScanMeta {
            root: PathBuf::from("root"),
            scanned_at: "2026-01-01T00:00:00Z".into(),
            elapsed_ms: 7,
            errors: 0,
        }
    }

    fn lines(shared: &Shared) -> Vec<Value> {
        let bytes = shared.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn stream(
        max_depth: Option<usize>,
        include_files: bool,
        filter: Option<&Filter>,
    ) -> Vec<Value> {
        let tree = sample_tree();
        let shared = Shared::default();
        let s = ScanStream::new(
            PathBuf::from("root"),
            max_depth,
            include_files,
            filter,
            Box::new(shared.clone()),
        );
        s.start("2026-01-01T00:00:00Z", &Arc::new(Progress::default()));
        replay(&tree, &[], &s);
        s.finish(&meta(), &tree).unwrap();
        lines(&shared)
    }

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    }

    #[test]
    fn scan_stream_shape() {
        let lines = stream(None, true, None);
        assert_eq!(lines[0]["record"], "start");
        assert_eq!(lines[0]["schema"], "fastdir.scan.ndjson");
        assert_eq!(lines[0]["version"], 1);
        assert_eq!(lines[0]["root"], "root");
        let end = lines.last().unwrap();
        assert_eq!(end["record"], "end");
        assert_eq!(end["total"]["size"], 1150);
        assert_eq!(end["total"]["files"], 3);
        let entries = &lines[1..lines.len() - 1];
        // root, docs, big.iso, a.txt, b.md
        assert_eq!(entries.len(), 5);
        assert_eq!(end["entries"], 5);
        let file = entries.iter().find(|e| e["name"] == "big.iso").unwrap();
        assert_eq!(
            keys(file),
            [
                "depth",
                "disk_size",
                "error",
                "extension",
                "files",
                "folders",
                "hard_link",
                "items",
                "modified",
                "name",
                "path",
                "record",
                "size",
                "type"
            ]
        );
        assert_eq!(file["extension"], ".iso");
        assert_eq!(file["depth"], 1);
        let sep = std::path::MAIN_SEPARATOR;
        let a = entries.iter().find(|e| e["name"] == "a.txt").unwrap();
        assert_eq!(a["path"], format!("root{sep}docs{sep}a.txt"));
        assert_eq!(a["depth"], 2);
        // A folder comes after everything below it, the root last.
        let pos = |name: &str| entries.iter().position(|e| e["name"] == name).unwrap();
        assert!(pos("docs") > pos("a.txt") && pos("docs") > pos("b.md"));
        let root = entries.last().unwrap();
        assert_eq!(root["name"], "root");
        assert_eq!(root["depth"], 0);
        assert_eq!(root["size"], 1150);
        assert!(root.get("extension").is_none());
    }

    #[test]
    fn scan_stream_depth_files_and_filter() {
        let names = |lines: &[Value]| -> Vec<String> {
            lines
                .iter()
                .filter(|l| l["record"] == "entry")
                .map(|l| l["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(names(&stream(Some(0), true, None)), ["root"]);
        let mut shallow = names(&stream(Some(1), true, None));
        shallow.sort();
        assert_eq!(shallow, ["big.iso", "docs", "root"]);
        let mut folders = names(&stream(None, false, None));
        folders.sort();
        assert_eq!(folders, ["docs", "root"]);
        let filter = Filter::parse("*.md", "", "", "", "").unwrap();
        assert_eq!(names(&stream(None, true, Some(&filter))), ["b.md"]);
    }

    #[test]
    fn a_failed_write_cancels_the_scan() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let s = ScanStream::new(PathBuf::from("root"), None, true, None, Box::new(Broken));
        let progress = Arc::new(Progress::default());
        s.start("2026-01-01T00:00:00Z", &progress);
        assert!(progress.cancelled());
        assert_eq!(
            s.take_error().map(|e| e.kind()),
            Some(io::ErrorKind::BrokenPipe)
        );
    }

    #[test]
    fn top_stream_shape() {
        let tree = sample_tree();
        let files = tree.top_files(2);
        let mut out = Vec::new();
        write_top(&mut out, &meta(), &tree, &files, 2).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["schema"], "fastdir.top.ndjson");
        assert_eq!(lines[1]["record"], "file");
        assert_eq!(lines[1]["name"], "big.iso");
        assert_eq!(lines[1]["extension"], ".iso");
        assert_eq!(lines[2]["name"], "a.txt");
        assert_eq!(lines[3]["record"], "end");
        assert_eq!(lines[3]["count"], 2);
        assert_eq!(lines[3]["limit"], 2);
        assert_eq!(lines[3]["total"]["size"], 1150);
    }
}

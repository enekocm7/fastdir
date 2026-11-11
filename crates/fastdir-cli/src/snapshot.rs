//! `fastdir snapshot` (scan and save a timestamped snapshot, then apply
//! retention) and `fastdir history` (a folder's size across snapshots).
//!
//! The snapshot files themselves are written by `fastdir_core::history`, so
//! the GUI and the command line share one layout.

use crate::args::{ConfigSource, Format, HistoryCommand, SnapshotCommand};
use crate::schema::{SCHEMA_VERSION, csv_field, iso_time};
use crate::{CliError, exit, io_error, open_output, scan_with_progress, write_json};
use fastdir_core::export::ExportOptions;
use fastdir_core::history::{self, GrowthPoint, Retention};
use fastdir_settings::SnapshotSettings;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

pub const SNAPSHOT_SCHEMA: &str = "fastdir.snapshot";
pub const HISTORY_SCHEMA: &str = "fastdir.history";

/// The `[snapshots]` section named by `--config`, or `None` without it.
fn snapshot_settings(config: Option<&ConfigSource>) -> Result<Option<SnapshotSettings>, CliError> {
    let Some(source) = config else {
        return Ok(None);
    };
    let path = match source {
        ConfigSource::Default => None,
        ConfigSource::File(p) => Some(p.as_path()),
    };
    fastdir_settings::load_snapshot_settings(path)
        .map(Some)
        .map_err(|e| CliError::new(exit::USAGE, e))
}

/// `--dir`, else the configured folder, else the default one.
fn snapshot_dir(
    flag: Option<&PathBuf>,
    settings: Option<&SnapshotSettings>,
) -> Result<PathBuf, CliError> {
    flag.cloned()
        .or_else(|| settings.and_then(SnapshotSettings::effective_dir))
        .or_else(fastdir_settings::default_snapshot_dir)
        .ok_or_else(|| {
            CliError::new(
                exit::USAGE,
                "cannot locate the default snapshot folder; pass --dir",
            )
        })
}

pub fn run_snapshot_command(c: &SnapshotCommand) -> Result<(), CliError> {
    let settings = snapshot_settings(c.scan.config.as_ref())?;
    let dir = snapshot_dir(c.dir.as_ref(), settings.as_ref())?;
    // Flags win over the config; without either nothing is pruned.
    let keep = c
        .keep
        .or(settings.as_ref().map(|s| s.keep_count))
        .unwrap_or(0);
    let max_age = c
        .max_age_days
        .or(settings.as_ref().map(|s| s.max_age_days))
        .unwrap_or(0);
    let retention = Retention::from_limits(keep, max_age);

    let scanned_at = history::now_unix();
    let scanned = scan_with_progress(&c.scan)?;
    let options = ExportOptions {
        max_depth: c.max_depth,
        include_files: c.include_files,
    };
    let file = history::save(&scanned.tree, scanned_at, &dir, &options).map_err(|e| {
        CliError::new(
            exit::ERROR,
            format!("cannot save the snapshot in {}: {e}", dir.display()),
        )
    })?;
    let pruned = history::prune(&dir, &scanned.meta.root, &retention, history::now_unix())
        .map_err(|e| CliError::new(exit::ERROR, format!("cannot apply retention: {e}")))?;
    let count = history::list(&dir, &scanned.meta.root)
        .map(|l| l.len())
        .unwrap_or(0);
    if !c.scan.quiet {
        let mut line = format!("Saved snapshot {}", file.display());
        if !pruned.is_empty() {
            line.push_str(&format!("; removed {} old snapshot(s)", pruned.len()));
        }
        eprintln!("{line}");
    }
    let root = scanned.tree.root();
    let doc = json!({
        "schema": SNAPSHOT_SCHEMA,
        "version": SCHEMA_VERSION,
        "fastdir_version": env!("CARGO_PKG_VERSION"),
        "root": scanned.meta.root.to_string_lossy(),
        "scanned_at": iso_time(scanned_at),
        "elapsed_ms": scanned.meta.elapsed_ms,
        "errors": scanned.meta.errors,
        "file": file.to_string_lossy(),
        "snapshots_dir": dir.to_string_lossy(),
        "size": root.size,
        "disk_size": root.disk_size,
        "files": root.files,
        "folders": root.dirs,
        "snapshots": count,
        "pruned": pruned.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
    });
    write_json(
        &crate::args::OutputArgs {
            format: Format::Json,
            output: None,
            pretty: c.pretty,
        },
        &doc,
    )
}

/// The change since the previous point that has a size.
fn deltas(points: &[GrowthPoint]) -> Vec<Option<i64>> {
    let mut last: Option<u64> = None;
    points
        .iter()
        .map(|p| {
            let size = p.size.map(|s| s.size)?;
            let delta = last.map(|prev| size as i64 - prev as i64);
            last = Some(size);
            delta
        })
        .collect()
}

/// The `fastdir.history` document.
pub fn history_doc(path: &std::path::Path, dir: &std::path::Path, points: &[GrowthPoint]) -> Value {
    let deltas = deltas(points);
    let entries: Vec<Value> = points
        .iter()
        .zip(&deltas)
        .map(|(p, delta)| {
            json!({
                "scanned_at": iso_time(p.snapshot.scanned_at),
                "file": p.snapshot.path.to_string_lossy(),
                "root": p.snapshot.root.to_string_lossy(),
                "found": p.size.is_some(),
                "size": p.size.map(|s| s.size),
                "disk_size": p.size.map(|s| s.disk_size),
                "files": p.size.map(|s| s.files),
                "folders": p.size.map(|s| s.dirs),
                "delta": delta,
            })
        })
        .collect();
    let sized: Vec<u64> = points
        .iter()
        .filter_map(|p| p.size.map(|s| s.size))
        .collect();
    let change = match (sized.first(), sized.last()) {
        (Some(first), Some(last)) if sized.len() > 1 => Some(*last as i64 - *first as i64),
        _ => None,
    };
    json!({
        "schema": HISTORY_SCHEMA,
        "version": SCHEMA_VERSION,
        "fastdir_version": env!("CARGO_PKG_VERSION"),
        "path": path.to_string_lossy(),
        "snapshots_dir": dir.to_string_lossy(),
        "change": change,
        "points": entries,
    })
}

fn history_csv(points: &[GrowthPoint]) -> String {
    let mut out = String::from("scanned_at,size,disk_size,files,folders,delta,root,file\n");
    for (p, delta) in points.iter().zip(deltas(points)) {
        let num = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            iso_time(p.snapshot.scanned_at).unwrap_or_default(),
            num(p.size.map(|s| s.size)),
            num(p.size.map(|s| s.disk_size)),
            num(p.size.map(|s| s.files)),
            num(p.size.map(|s| s.dirs)),
            delta.map(|d| d.to_string()).unwrap_or_default(),
            csv_field(&p.snapshot.root.to_string_lossy()),
            csv_field(&p.snapshot.path.to_string_lossy()),
        ));
    }
    out
}

pub fn run_history_command(c: &HistoryCommand) -> Result<(), CliError> {
    let settings = snapshot_settings(c.config.as_ref())?;
    let dir = snapshot_dir(c.dir.as_ref(), settings.as_ref())?;
    // The folder may no longer exist; only make the path absolute.
    let path = std::path::absolute(&c.path)
        .map_err(|e| CliError::new(exit::NOT_FOUND, format!("{}: {e}", c.path.display())))?;
    let points = history::growth(&dir, &path).map_err(|e| {
        CliError::new(
            exit::ERROR,
            format!("cannot read snapshots in {}: {e}", dir.display()),
        )
    })?;
    match c.out.format {
        Format::Json => write_json(&c.out, &history_doc(&path, &dir, &points)),
        Format::Csv => {
            let mut w = open_output(&c.out)?;
            w.write_all(history_csv(&points).as_bytes())
                .and_then(|_| w.flush())
                .map_err(io_error)
        }
        Format::Html => Err(crate::html_not_supported()),
        // The parser only accepts these for `scan` / `top`.
        Format::Binary | Format::Ndjson => Err(crate::format_unsupported(c.out.format)),
    }
}

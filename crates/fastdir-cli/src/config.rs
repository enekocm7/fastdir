//! Opt-in use of the `[scan]` section of `config.toml` (`--config`).
//!
//! Without `--config` the command line ignores the file entirely. With it,
//! the configured scan options form the base; explicit flags then override
//! them (`--threads`), add to them (`--exclude`) or switch things on
//! (`--follow-junctions`, ...).

use crate::args::{ConfigSource, ScanArgs};
use fastdir_core::scanner::ScanOptions;
use fastdir_settings::ScanSettings;

/// Loads the `[scan]` settings named by `source`. Missing or invalid files
/// are an error, never a silent fall back.
pub fn load(source: &ConfigSource) -> Result<ScanSettings, String> {
    match source {
        ConfigSource::Default => fastdir_settings::load_scan_settings(None),
        ConfigSource::File(p) => fastdir_settings::load_scan_settings(Some(p)),
    }
}

/// The scan options a config alone describes.
pub fn options_from(s: &ScanSettings) -> ScanOptions {
    ScanOptions {
        excluded_globs: s.excluded.clone(),
        skip_hidden_system: s.skip_hidden_system,
        follow_junctions: s.follow_junctions,
        cross_volumes: s.cross_volumes,
        threads: s.thread_count.map(usize::from),
        collect_owners: s.collect_owners,
        skip_windows_dir: s.skip_windows_dir,
    }
}

/// Layers explicit `over` options on top of `base`: exclusions are added,
/// a thread count wins, and switches turn on if either side has them on.
pub fn layer(base: &ScanOptions, over: &ScanOptions) -> ScanOptions {
    let mut excluded_globs = base.excluded_globs.clone();
    for g in &over.excluded_globs {
        if !excluded_globs.contains(g) {
            excluded_globs.push(g.clone());
        }
    }
    ScanOptions {
        excluded_globs,
        skip_hidden_system: base.skip_hidden_system || over.skip_hidden_system,
        follow_junctions: base.follow_junctions || over.follow_junctions,
        cross_volumes: base.cross_volumes || over.cross_volumes,
        threads: over.threads.or(base.threads),
        collect_owners: base.collect_owners || over.collect_owners,
        skip_windows_dir: base.skip_windows_dir || over.skip_windows_dir,
    }
}

/// Folds the configured scan settings into `args` (flags win).
pub fn apply(args: &mut ScanArgs) -> Result<(), String> {
    let Some(source) = args.config.clone() else {
        return Ok(());
    };
    let merged = layer(&options_from(&load(&source)?), &args.scan_options());
    args.exclude = merged.excluded_globs;
    args.skip_hidden_system = merged.skip_hidden_system;
    args.follow_junctions = merged.follow_junctions;
    args.cross_volumes = merged.cross_volumes;
    args.threads = merged.threads;
    args.collect_owners = merged.collect_owners;
    args.skip_windows_dir = merged.skip_windows_dir;
    Ok(())
}

/// The effective options as JSON, for `fastdir config`.
pub fn describe(opts: &ScanOptions, source: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": "fastdir.config",
        "version": 1,
        "source": source,
        "scan": {
            "exclude": opts.excluded_globs,
            "skip_hidden_system": opts.skip_hidden_system,
            "follow_junctions": opts.follow_junctions,
            "cross_volumes": opts.cross_volumes,
            "threads": opts.threads,
            "collect_owners": opts.collect_owners,
            "skip_windows_dir": opts.skip_windows_dir,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fastdir-cli-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn args(config: Option<ConfigSource>) -> ScanArgs {
        ScanArgs {
            path: ".".into(),
            threads: None,
            exclude: vec!["flag".into()],
            follow_junctions: false,
            cross_volumes: false,
            skip_hidden_system: false,
            skip_windows_dir: false,
            collect_owners: false,
            quiet: true,
            config,
        }
    }

    #[test]
    fn layering_rules() {
        let base = ScanOptions {
            excluded_globs: vec!["*.tmp".into()],
            follow_junctions: true,
            threads: Some(8),
            ..Default::default()
        };
        let over = ScanOptions {
            excluded_globs: vec!["node_modules".into(), "*.tmp".into()],
            skip_hidden_system: true,
            threads: Some(2),
            ..Default::default()
        };
        let m = layer(&base, &over);
        assert_eq!(m.excluded_globs, vec!["*.tmp", "node_modules"]);
        assert!(m.follow_junctions && m.skip_hidden_system);
        assert_eq!(m.threads, Some(2));
        assert_eq!(layer(&base, &ScanOptions::default()).threads, Some(8));
    }

    #[test]
    fn apply_uses_config_and_flags_override() {
        let p = temp_config(
            "apply.toml",
            "[scan]\nexcluded = [\"*.tmp\"]\nskip_hidden_system = true\nskip_windows_dir = true\nthread_count = 3\n",
        );
        let mut a = args(Some(ConfigSource::File(p)));
        apply(&mut a).unwrap();
        assert_eq!(a.exclude, vec!["*.tmp", "flag"]);
        assert!(a.skip_hidden_system);
        assert!(a.skip_windows_dir);
        assert_eq!(a.threads, Some(3));

        let p = temp_config("apply2.toml", "[scan]\nthread_count = 3\n");
        let mut a = args(Some(ConfigSource::File(p)));
        a.threads = Some(5);
        apply(&mut a).unwrap();
        assert_eq!(a.threads, Some(5));
    }

    #[test]
    fn without_config_nothing_changes() {
        let mut a = args(None);
        let before = a.clone();
        apply(&mut a).unwrap();
        assert_eq!(a, before);
    }

    #[test]
    fn missing_or_invalid_config_is_an_error() {
        let mut a = args(Some(ConfigSource::File("does-not-exist.toml".into())));
        assert!(apply(&mut a).unwrap_err().contains("cannot read"));
        let p = temp_config("bad.toml", "[scan\n");
        let mut a = args(Some(ConfigSource::File(p)));
        assert!(apply(&mut a).unwrap_err().contains("invalid"));
    }
}

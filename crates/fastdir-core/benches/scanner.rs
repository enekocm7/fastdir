//! Scanner benchmarks (criterion).
//!
//! ```text
//! cargo bench -p fastdir-core
//! cargo bench -p fastdir-core -- --save-baseline before
//! cargo bench -p fastdir-core -- --baseline before
//! ```
//!
//! The on-disk tree is generated once from a fixed seed under
//! `target/fastdir-bench-tree` and reused between runs (delete the folder to
//! regenerate it). Runs are warm-cache: see the README for cold-cache runs.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use fastdir_core::model::{Arena, DirBuilder, Subtree, Tree};
use fastdir_core::scanner::{Progress, ScanOptions, scan};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FILES: usize = 200_000;
const DIRS: usize = 20_000;
/// Bump when the generator changes so stale trees are rebuilt.
const VERSION: u32 = 1;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Directory layout: entry `i - 1` is the parent of dir `i` (dir 0 is the
/// root). Most parents are recent dirs (deep chains), some are random (fan-out).
fn layout(rng: &mut Rng) -> Vec<usize> {
    (1..DIRS)
        .map(|i| {
            if rng.below(3) == 0 {
                rng.below(i)
            } else {
                i - 1 - rng.below(i.min(8))
            }
        })
        .collect()
}

/// Every 10th directory is `cache_N`, so an exclusion has work to do.
fn dir_name(i: usize) -> String {
    if i % 10 == 0 {
        format!("cache_{i}")
    } else {
        format!("dir_{i}")
    }
}

fn bench_root() -> PathBuf {
    // Workspace root = two levels above this crate (no `..`: the scanner rejects it).
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");
    let root = ws.join("target").join("fastdir-bench-tree");
    let marker = root.join(".complete");
    let want = format!("{VERSION} {FILES} {DIRS}");
    if fs::read_to_string(&marker).is_ok_and(|s| s == want) {
        return root;
    }
    let _ = fs::remove_dir_all(&root);
    eprintln!("generating benchmark tree in {} ...", root.display());
    let mut rng = Rng(0x00C0_FFEE_D1CE_5EED);
    let parents = layout(&mut rng);
    let mut paths = vec![root.clone()];
    for (i, &p) in parents.iter().enumerate() {
        let path = paths[p].join(dir_name(i + 1));
        fs::create_dir_all(&path).expect("create dir");
        paths.push(path);
    }
    // A few huge dirs take 30% of the files, the rest is spread out.
    let huge = [0usize, 1, 2, 3];
    let payload = vec![b'x'; 4096];
    for f in 0..FILES {
        let d = if f % 10 < 3 {
            huge[rng.below(huge.len())]
        } else {
            rng.below(DIRS)
        };
        let len = if rng.below(50) == 0 {
            4096
        } else {
            rng.below(1024)
        };
        let ext = ["txt", "rs", "png", "dll", "log"][rng.below(5)];
        fs::write(paths[d].join(format!("file_{f}.{ext}")), &payload[..len]).expect("write file");
    }
    fs::write(&marker, want).expect("marker");
    root
}

fn run_scan(root: &Path, options: &ScanOptions) -> Tree {
    scan(root.to_path_buf(), Arc::new(Progress::default()), options).tree
}

fn scans(c: &mut Criterion) {
    let root = bench_root();
    let sanity = run_scan(&root, &ScanOptions::default());
    assert!(
        sanity.root().files as usize >= FILES,
        "benchmark scan found no files"
    );
    let mut g = c.benchmark_group("scan");
    g.sample_size(10);
    g.throughput(Throughput::Elements((FILES + DIRS) as u64));

    let default = ScanOptions::default();
    g.bench_function("default", |b| {
        b.iter(|| black_box(run_scan(&root, &default)))
    });

    let excluded = ScanOptions {
        excluded_globs: vec!["cache_*".into(), "*.log".into()],
        ..ScanOptions::default()
    };
    g.bench_function("exclusions", |b| {
        b.iter(|| black_box(run_scan(&root, &excluded)))
    });

    let one = ScanOptions {
        threads: Some(1),
        ..ScanOptions::default()
    };
    g.bench_function("1_thread", |b| b.iter(|| black_box(run_scan(&root, &one))));
    let all = ScanOptions {
        threads: Some(fastdir_core::scanner::default_thread_count()),
        ..ScanOptions::default()
    };
    g.bench_function("all_threads", |b| {
        b.iter(|| black_box(run_scan(&root, &all)))
    });
    g.finish();
}

fn fill(b: &mut DirBuilder, rng: &mut Rng, files: usize) {
    for f in 0..files {
        let size = rng.next() % 1_000_000;
        b.add_file(
            &format!("file_{f}.dat"),
            size,
            size.next_multiple_of(4096),
            1_700_000_000,
        );
    }
}

/// An in-memory tree of `dirs` directories with `per_dir` files each.
fn build_tree(dirs: usize, per_dir: usize) -> Tree {
    let mut rng = Rng(42);
    let mut arena = Arena::default();
    let mut top = DirBuilder::new(0);
    for d in 0..dirs {
        let mut b = DirBuilder::new(0);
        fill(&mut b, &mut rng, per_dir);
        let node = b.commit(&mut arena);
        top.add_dir(&format!("dir_{d}"), node);
    }
    let root = top.commit(&mut arena);
    Tree::new(Subtree::new(arena, "root", root), PathBuf::from("root"))
}

fn tree_build(c: &mut Criterion) {
    let (dirs, per_dir) = (2_000, 100);
    let mut g = c.benchmark_group("tree");
    g.throughput(Throughput::Elements((dirs * per_dir) as u64));
    g.bench_function("build_arena", |b| {
        b.iter(|| black_box(build_tree(dirs, per_dir)))
    });

    // Sorting and totals of a single huge directory.
    let huge = 100_000;
    g.throughput(Throughput::Elements(huge as u64));
    g.bench_function("sort_huge_dir", |b| {
        b.iter_batched(
            || {
                let mut d = DirBuilder::new(0);
                fill(&mut d, &mut Rng(7), huge);
                d
            },
            |mut d| {
                d.finish();
                black_box(d)
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

criterion_group!(benches, scans, tree_build);
criterion_main!(benches);

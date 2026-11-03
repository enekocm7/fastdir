//! Treemap layout benchmarks (criterion). Run with `cargo bench -p fastdir-treemap`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fastdir_core::model::{Arena, DirBuilder, Subtree, Tree};
use fastdir_treemap::{Rect, RenderOptions, render, squarify};
use std::hint::black_box;
use std::path::PathBuf;

/// A root with `n` files of pseudo-random sizes (fixed seed).
fn flat_tree(n: usize) -> Tree {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut arena = Arena::default();
    let mut b = DirBuilder::new(0);
    for i in 0..n {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        b.add_file(&format!("f{i}.dat"), s % 10_000_000 + 1, 0, 1_700_000_000);
    }
    let root = b.commit(&mut arena);
    Tree::new(Subtree::new(arena, "root", root), PathBuf::from("root"))
}

fn layout(c: &mut Criterion) {
    let mut g = c.benchmark_group("treemap");
    let rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 1920.0,
        y1: 1080.0,
    };
    for n in [1_000usize, 10_000, 100_000] {
        let tree = flat_tree(n);
        let root = tree.root();
        g.throughput(Throughput::Elements(n as u64));
        g.bench_with_input(BenchmarkId::new("squarify", n), &n, |b, _| {
            b.iter(|| {
                let mut acc = 0.0;
                squarify(tree.children(root), root.size, rc, |_, r| acc += r.w());
                black_box(acc)
            })
        });
    }
    let tree = flat_tree(10_000);
    let opts = RenderOptions {
        now: 1_700_000_000,
        ..RenderOptions::default()
    };
    g.sample_size(20);
    g.bench_function("render_1280x720_10k", |b| {
        b.iter(|| black_box(render(&tree, tree.root(), 1280, 720, [0, 0, 0], &opts)))
    });
    g.finish();
}

criterion_group!(benches, layout);
criterion_main!(benches);

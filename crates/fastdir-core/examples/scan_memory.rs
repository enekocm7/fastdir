//! Measure how much heap a scanned tree keeps alive.
//!
//! ```text
//! cargo run --release -p fastdir-core --example scan_memory -- C:\
//! ```
//!
//! Prints the live heap after the scan (the tree only; scan scratch space has
//! been freed by then), the peak heap during the scan, and bytes per item.

use fastdir_core::scanner::{Progress, scan};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let now = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed)
                    + (new_size - layout.size());
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn mib(b: usize) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let progress = Arc::new(Progress::default());
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let result = scan(root.clone(), progress.clone());
    let live = LIVE.load(Ordering::Relaxed) - before;
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let items = progress.files.load(Ordering::Relaxed) + progress.dirs.load(Ordering::Relaxed) + 1;
    println!("root:     {}", root.display());
    println!("items:    {items}");
    println!("elapsed:  {:.2?}", result.elapsed);
    println!(
        "tree:     {:.1} MiB ({:.1} bytes/item)",
        mib(live),
        live as f64 / items as f64
    );
    println!("peak:     {:.1} MiB", mib(peak));
    drop(result);
}

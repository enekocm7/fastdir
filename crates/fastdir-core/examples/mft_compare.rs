//! Scans a folder twice, once by walking directories and once through the
//! NTFS MFT, and compares the results. Run it elevated on a real volume:
//!
//! ```text
//! cargo run --release -p fastdir-core --example mft_compare -- C:\
//! ```

use fastdir_core::model::Tree;
use fastdir_core::scanner::{Progress, ScanOptions, scan};
use std::path::PathBuf;
use std::sync::Arc;

fn totals(tree: &Tree) -> (u32, u32, u64, u64) {
    let r = tree.root();
    (r.files, r.dirs, r.size, r.disk_size)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "C:\\".into()));
    let walk = scan(
        root.clone(),
        Arc::new(Progress::default()),
        &ScanOptions::default(),
    );
    let mft = scan(
        root.clone(),
        Arc::new(Progress::default()),
        &ScanOptions {
            use_mft: true,
            ..ScanOptions::default()
        },
    );
    if !mft.used_mft {
        eprintln!("The MFT could not be read (not elevated, or not NTFS).");
        std::process::exit(1);
    }
    let (w, m) = (totals(&walk.tree), totals(&mft.tree));
    println!("             files       dirs          bytes        on disk     time");
    println!(
        "walk  {:>12} {:>10} {:>14} {:>14} {:>8.2?}",
        w.0, w.1, w.2, w.3, walk.elapsed
    );
    println!(
        "mft   {:>12} {:>10} {:>14} {:>14} {:>8.2?}",
        m.0, m.1, m.2, m.3, mft.elapsed
    );
    // Folders the walk could not read are the expected difference.
    println!(
        "unreadable folders in the walk: {}",
        walk.tree.unreadable_dirs().len()
    );
}

//! In-app updater for FastDir.
//!
//! All items are re-exported at the crate root, so callers use
//! `fastdir_updater::fetch_latest()` and friends.

mod updater;

pub use updater::*;

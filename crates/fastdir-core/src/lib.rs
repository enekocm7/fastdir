//! GPU-free core of FastDir: the scanned directory tree, the parallel
//! scanner, drive enumeration, file-system operations and formatting helpers.
//!
//! Nothing in this crate may depend on a GUI toolkit, so it can be tested and
//! benchmarked on its own.

pub mod actions;
pub mod categories;
pub mod cleanup;
pub mod compress;
pub mod diff;
pub mod drives;
pub mod duplicates;
pub mod elevate;
pub mod export;
pub mod filter;
pub mod format;
pub mod fsops;
pub mod history;
pub mod integration;
pub mod mft;
pub mod model;
pub mod scanfile;
pub mod scanner;
pub mod sysprefs;
pub mod windows_apps;

//! GPU-free core of FastDir: the scanned directory tree, the parallel
//! scanner, drive enumeration, file-system operations and formatting helpers.
//!
//! Nothing in this crate may depend on a GUI toolkit, so it can be tested and
//! benchmarked on its own.

pub mod drives;
pub mod format;
pub mod fsops;
pub mod integration;
pub mod model;
pub mod scanner;

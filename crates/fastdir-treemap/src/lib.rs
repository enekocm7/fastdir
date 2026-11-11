//! Squarified cushion treemap layout and rendering, plus the standalone
//! HTML report that embeds it.
//!
//! The public API is re-exported at the crate root: [`Rect`], [`squarify`],
//! [`ext_color`], [`render`], [`hit_test`], [`rect_of`] and the
//! [`report`] functions.

mod report;
mod treemap;

pub use report::{ReportOptions, export_html, export_html_to_file};
pub use treemap::*;

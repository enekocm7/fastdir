//! Squarified cushion treemap layout and rendering, plus the standalone
//! HTML report that embeds it, and the [`sunburst`] layout that draws the
//! same tree as rings.
//!
//! The public API is re-exported at the crate root: [`Rect`], [`squarify`],
//! [`ext_color`], [`render`], [`hit_test`], [`rect_of`] and the
//! [`report`] functions.

mod report;
pub mod sunburst;
mod treemap;

pub use report::{ReportOptions, export_html, export_html_to_file};
pub use sunburst::Layout;
pub use treemap::*;

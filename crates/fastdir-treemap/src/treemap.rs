//! Squarified cushion treemap, rendered on the CPU in parallel.
//!
//! This follows the WinDirStat/SequoiaView approach: every rectangle adds a
//! parabolic "ridge" to a height surface, and each leaf is shaded by the
//! surface normal against a light source. The result is the familiar field of
//! little pillow-shaped squares, one per file, coloured by extension.
//!
//! Layout is a pure function of the tree and the pixel size, so hit-testing
//! and highlighting re-run the layout only along a single path instead of
//! keeping millions of rectangles in memory.

use fastdir_core::model::{NO_EXT, NO_OWNER, Node, NodeKind, NodePath, Tree};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    #[inline]
    pub fn w(&self) -> f64 {
        self.x1 - self.x0
    }
    #[inline]
    pub fn h(&self) -> f64 {
        self.y1 - self.y0
    }
    #[inline]
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

// WinDirStat's default cushion parameters.
const HEIGHT: f64 = 0.38;
const SCALE: f64 = 0.91;
const AMBIENT: f64 = 0.15;
const BRIGHTNESS: f64 = 0.88;
const PALETTE_BRIGHTNESS: f64 = 0.6;
const LIGHT: (f64, f64, f64) = (-1.0, -1.0, 10.0);

/// Base colours for the top extensions (by bytes). Everything else is grey.
const PALETTE: [(u8, u8, u8); 18] = [
    (0, 0, 255),
    (255, 0, 0),
    (0, 255, 0),
    (0, 255, 255),
    (255, 0, 255),
    (255, 255, 0),
    (150, 150, 255),
    (255, 150, 150),
    (150, 255, 150),
    (150, 255, 255),
    (255, 150, 255),
    (255, 255, 150),
    (255, 128, 0),
    (0, 160, 255),
    (160, 0, 255),
    (0, 200, 120),
    (200, 120, 60),
    (255, 0, 128),
];
/// Colour-blind friendlier alternative to [`PALETTE`] (Okabe-Ito inspired,
/// extended with lighter / darker variants past the first eight).
const COLORBLIND_PALETTE: [(u8, u8, u8); 18] = [
    (0, 114, 178),
    (230, 159, 0),
    (0, 158, 115),
    (204, 121, 167),
    (86, 180, 233),
    (213, 94, 0),
    (240, 228, 66),
    (0, 0, 0),
    (119, 172, 212),
    (255, 190, 93),
    (115, 209, 173),
    (232, 167, 198),
    (141, 211, 244),
    (244, 162, 97),
    (247, 243, 158),
    (120, 120, 120),
    (70, 70, 178),
    (178, 120, 60),
];
/// WinDirStat colours for the drive pseudo-items.
pub const FREE_SPACE_COLOR: (u8, u8, u8) = (128, 128, 128);
pub const UNKNOWN_COLOR: (u8, u8, u8) = (255, 255, 0);
const OTHER: (u8, u8, u8) = (200, 200, 200);

/// One colour per file-type category, in `FileCategory::ALL` order.
const CATEGORY_PALETTE: [(u8, u8, u8); 10] = [
    (0, 114, 178),   // video
    (0, 158, 115),   // images
    (204, 121, 167), // audio
    (230, 159, 0),   // archives
    (213, 94, 0),    // documents
    (86, 180, 233),  // code
    (117, 112, 179), // executables
    (240, 228, 66),  // disk images
    (150, 150, 150), // system
    (200, 200, 200), // other
];

/// The display colour for a file-type category index
/// ([`fastdir_core::categories::FileCategory::index`]).
pub fn category_color(category: usize) -> (u8, u8, u8) {
    CATEGORY_PALETTE.get(category).copied().unwrap_or(OTHER)
}

/// Normalise so channel overflow spills into the others (keeps hue, avoids clipping).
fn normalize(r: &mut f64, g: &mut f64, b: &mut f64) {
    if *r > 255.0 {
        let over = *r - 255.0;
        *r = 255.0;
        *g += over / 2.0;
        *b += over / 2.0;
    }
    if *g > 255.0 {
        let over = *g - 255.0;
        *g = 255.0;
        *r += over / 2.0;
        *b += over / 2.0;
    }
    if *b > 255.0 {
        let over = *b - 255.0;
        *b = 255.0;
        *r += over / 2.0;
        *g += over / 2.0;
    }
    *r = r.min(255.0);
    *g = g.min(255.0);
    *b = b.min(255.0);
}

/// Equalise a colour to the palette brightness (so all hues look equally bright).
fn bright_color((r, g, b): (u8, u8, u8)) -> [f64; 3] {
    let (mut r, mut g, mut b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let sum = r + g + b;
    if sum > 0.0 {
        let f = 3.0 * PALETTE_BRIGHTNESS / sum;
        r *= f;
        g *= f;
        b *= f;
    }
    let (mut r, mut g, mut b) = (r * 255.0, g * 255.0, b * 255.0);
    normalize(&mut r, &mut g, &mut b);
    [r, g, b]
}

/// The display colour (sRGB 0-255) for an extension rank, as shown in the legend.
pub fn ext_color(ext: u16) -> (u8, u8, u8) {
    ext_color_with(Palette::Default, ext)
}

/// [`ext_color`] for an explicit [`Palette`].
pub fn ext_color_with(palette: Palette, ext: u16) -> (u8, u8, u8) {
    if ext == NO_EXT {
        return OTHER;
    }
    match palette {
        Palette::Default => PALETTE.get(ext as usize).copied().unwrap_or(OTHER),
        Palette::ColorBlind => COLORBLIND_PALETTE
            .get(ext as usize)
            .copied()
            .unwrap_or(OTHER),
        Palette::Mono => {
            // Greyscale ramp by rank; past the ramp everything is mid-grey.
            const STEPS: usize = 18;
            if (ext as usize) < STEPS {
                let v = 235u8.saturating_sub((ext as u16 * 150 / STEPS as u16) as u8);
                (v, v, v)
            } else {
                OTHER
            }
        }
    }
}

/// Display colour for a node: pseudo-items get their WinDirStat colours,
// otherwise the extension colour.
pub fn node_color(node: &Node) -> (u8, u8, u8) {
    node_color_with(Palette::Default, node)
}

/// [`node_color`] for an explicit [`Palette`].
pub fn node_color_with(palette: Palette, node: &Node) -> (u8, u8, u8) {
    match node.kind() {
        NodeKind::FreeSpace => FREE_SPACE_COLOR,
        NodeKind::Unknown => UNKNOWN_COLOR,
        _ => ext_color_with(palette, node.ext),
    }
}

const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
const MONTH: i64 = 30 * DAY;
const HALF_YEAR: i64 = 182 * DAY;
const YEAR: i64 = 365 * DAY;
const THREE_YEARS: i64 = 3 * YEAR;

/// Fixed age buckets used by [`ColorMode::Age`] and the legend. The order is
/// newest to oldest so the colour scale and legend read left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgeBucket {
    /// Less than one week old.
    Week,
    /// One week to under one month.
    Month,
    /// One month to under six months.
    SixMonths,
    /// Six months to under one year.
    Year,
    /// One year to under three years.
    ThreeYears,
    /// Three years or older.
    Older,
    /// No usable modification time.
    Unknown,
}

impl AgeBucket {
    /// Every bucket, newest to oldest. Suitable for legends.
    pub const ALL: [AgeBucket; 7] = [
        AgeBucket::Week,
        AgeBucket::Month,
        AgeBucket::SixMonths,
        AgeBucket::Year,
        AgeBucket::ThreeYears,
        AgeBucket::Older,
        AgeBucket::Unknown,
    ];

    fn index(self) -> usize {
        match self {
            AgeBucket::Week => 0,
            AgeBucket::Month => 1,
            AgeBucket::SixMonths => 2,
            AgeBucket::Year => 3,
            AgeBucket::ThreeYears => 4,
            AgeBucket::Older => 5,
            AgeBucket::Unknown => 6,
        }
    }
}

/// The age bucket for a modification time. `modified` and `now` are Unix
/// seconds; non-positive timestamps are [`AgeBucket::Unknown`]. Future
/// timestamps count as brand new.
pub fn age_bucket(modified: i64, now: i64) -> AgeBucket {
    if modified <= 0 {
        return AgeBucket::Unknown;
    }
    let age = now.saturating_sub(modified).max(0);
    if age < WEEK {
        AgeBucket::Week
    } else if age < MONTH {
        AgeBucket::Month
    } else if age < HALF_YEAR {
        AgeBucket::SixMonths
    } else if age < YEAR {
        AgeBucket::Year
    } else if age < THREE_YEARS {
        AgeBucket::ThreeYears
    } else {
        AgeBucket::Older
    }
}

/// Warm-to-cold ramp, one colour per bucket (new to old, then unknown).
const AGE_PALETTE: [(u8, u8, u8); 7] = [
    (231, 76, 60),
    (230, 126, 34),
    (241, 196, 15),
    (46, 204, 113),
    (52, 152, 219),
    (142, 68, 173),
    (128, 128, 128),
];
/// Colour-blind-safe variant (Okabe-Ito based), same order.
const AGE_PALETTE_COLORBLIND: [(u8, u8, u8); 7] = [
    (213, 94, 0),
    (230, 159, 0),
    (240, 228, 66),
    (0, 158, 115),
    (86, 180, 233),
    (0, 114, 178),
    (128, 128, 128),
];
/// Greyscale ramp for the mono palette (old is darker).
const AGE_PALETTE_MONO: [(u8, u8, u8); 7] = [
    (225, 225, 225),
    (200, 200, 200),
    (175, 175, 175),
    (150, 150, 150),
    (120, 120, 120),
    (90, 90, 90),
    (128, 128, 128),
];

/// Display colour for an age bucket under `palette`.
pub fn age_bucket_color(bucket: AgeBucket, palette: Palette) -> (u8, u8, u8) {
    let ix = bucket.index();
    match palette {
        Palette::Default => AGE_PALETTE[ix],
        Palette::ColorBlind => AGE_PALETTE_COLORBLIND[ix],
        Palette::Mono => AGE_PALETTE_MONO[ix],
    }
}

/// Warm-to-cold colour for a file modified at `modified` ("now" is `now`,
/// both Unix seconds), using fixed buckets so it matches the legend.
pub fn age_color(modified: i64, now: i64, palette: Palette) -> (u8, u8, u8) {
    age_bucket_color(age_bucket(modified, now), palette)
}

/// Bytes per age bucket for every real file under `root`, matching the
/// treemap's size metric. Directories are traversed but not counted, so the
/// totals add up to the files' bytes without double counting.
pub fn age_bucket_totals(
    tree: &Tree,
    root: &Node,
    now: i64,
    use_disk: bool,
) -> [(AgeBucket, u64); 7] {
    let mut totals = [0u64; 7];
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_dir() {
            stack.extend(tree.children(node));
        } else if !node.is_special() {
            let bucket = age_bucket(node.modified(), now);
            totals[bucket.index()] += node_size(node, use_disk);
        }
    }
    std::array::from_fn(|i| (AgeBucket::ALL[i], totals[i]))
}

fn shade_node(tree: &Tree, node: &Node, opts: &RenderOptions) -> [f64; 3] {
    let rgb = match node.kind() {
        NodeKind::FreeSpace => FREE_SPACE_COLOR,
        NodeKind::Unknown => UNKNOWN_COLOR,
        _ => match opts.color_mode {
            ColorMode::Extension => ext_color_with(opts.palette, node.ext),
            ColorMode::Age => age_color(node.modified(), opts.now, opts.palette),
            ColorMode::Category => category_color(tree.ext_category(node.ext).index()),
        },
    };
    bright_color(rgb)
}

/// Desaturate and darken a colour so non-highlighted leaves fade into the
/// background without disappearing completely.
fn dim_color(color: [f64; 3]) -> [f64; 3] {
    const DIM: f64 = 38.0;
    const KEEP: f64 = 0.22;
    [
        DIM + (color[0] - DIM) * KEEP,
        DIM + (color[1] - DIM) * KEEP,
        DIM + (color[2] - DIM) * KEEP,
    ]
}

/// Squarify `children` (sorted by size, descending) into `rc`. Calls `f`
/// with each child index and its rectangle. Adjacent rectangles share exact
/// edge values so pixel rounding never overlaps.
pub fn squarify(children: &[Node], total: u64, rc: Rect, mut f: impl FnMut(usize, Rect)) {
    let n = children.iter().take_while(|c| c.size > 0).count();
    if n == 0 || total == 0 {
        return;
    }
    let mut rc = rc;
    let mut remaining = total as f64;
    let mut start = 0;
    while start < n {
        let w = rc.w();
        let h = rc.h();
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // Wide rectangle: lay a column on the left; tall: a row on the top.
        let horizontal = w >= h;
        let side = if horizontal { h } else { w };
        let area_per_byte = w * h / remaining;

        let max_size = children[start].size as f64;
        let mut end = start;
        let mut row_sum = 0.0;
        let mut worst = f64::INFINITY;
        while end < n {
            let s = children[end].size as f64;
            let new_sum = row_sum + s;
            let thickness = new_sum * area_per_byte / side;
            let ratio = |sz: f64| {
                let len = sz * area_per_byte / thickness;
                if len > thickness {
                    len / thickness
                } else {
                    thickness / len
                }
            };
            let new_worst = ratio(max_size).max(ratio(s));
            if end > start && new_worst > worst {
                break;
            }
            worst = new_worst;
            row_sum = new_sum;
            end += 1;
        }

        let last_row = end == n;
        let frac = row_sum / remaining;
        if horizontal {
            let split = if last_row { rc.x1 } else { rc.x0 + w * frac };
            let mut y = rc.y0;
            for i in start..end {
                let y_next = if i + 1 == end {
                    rc.y1
                } else {
                    y + h * (children[i].size as f64 / row_sum)
                };
                f(
                    i,
                    Rect {
                        x0: rc.x0,
                        y0: y,
                        x1: split,
                        y1: y_next,
                    },
                );
                y = y_next;
            }
            rc.x0 = split;
        } else {
            let split = if last_row { rc.y1 } else { rc.y0 + h * frac };
            let mut x = rc.x0;
            for i in start..end {
                let x_next = if i + 1 == end {
                    rc.x1
                } else {
                    x + w * (children[i].size as f64 / row_sum)
                };
                f(
                    i,
                    Rect {
                        x0: x,
                        y0: rc.y0,
                        x1: x_next,
                        y1: split,
                    },
                );
                x = x_next;
            }
            rc.y0 = split;
        }
        remaining -= row_sum;
        start = end;
    }
}

/// Which palette maps extension ranks to colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Palette {
    /// WinDirStat-style rainbow.
    #[default]
    Default,
    /// Okabe-Ito inspired, friendlier to red-green colour blindness.
    ColorBlind,
    /// Greyscale ramp (prints well, calm).
    Mono,
}

/// What decides a leaf's colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// Colour by file extension (the extension list doubles as a legend).
    #[default]
    Extension,
    /// Colour by age: new files are warm, old files are cold.
    Age,
    /// Colour by file-type category (video, images, archives, ...).
    Category,
}

/// Everything about how a treemap looks. Passed as one struct so new display
/// settings stay a settings-only change.
#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Extension rank in full colour, everything else dimmed. Ignored when
    /// [`ColorMode::Age`] or [`ColorMode::Category`] is active.
    pub highlight: Option<u16>,
    /// Category index in full colour, everything else dimmed. Takes
    /// precedence over `highlight` when both are set.
    pub highlight_category: Option<u8>,
    /// Owner index in full colour, everything else dimmed. Takes precedence
    /// over the category and extension highlights.
    pub highlight_owner: Option<u16>,
    /// Owner of `root`, so a root leaf can be highlighted too.
    pub root_owner: u16,
    /// Lay out the `<Free space>` pseudo-item.
    pub show_free: bool,
    /// Lay out the `<Unknown>` pseudo-item.
    pub show_unknown: bool,
    /// Size cells by allocated (on-disk) bytes instead of logical bytes.
    pub use_disk: bool,
    pub palette: Palette,
    pub color_mode: ColorMode,
    /// Cushion (SequoiaView pillow) shading, or flat rectangles.
    pub cushion: bool,
    /// Ridge height multiplier for the cushion shading (0.0..=2.0).
    pub cushion_strength: f32,
    /// Leave a 1px gap between cells.
    pub borders: bool,
    /// Stop subdividing once a cell is smaller than this (device px).
    pub min_cell: f32,
    /// Unix timestamp used as "now" for age colours.
    pub now: i64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            highlight: None,
            highlight_category: None,
            highlight_owner: None,
            root_owner: NO_OWNER,
            show_free: false,
            show_unknown: false,
            use_disk: false,
            palette: Palette::Default,
            color_mode: ColorMode::Extension,
            cushion: true,
            cushion_strength: 1.0,
            borders: false,
            min_cell: 1.0,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }
}
/// The metric used to size a cell: logical bytes or allocated bytes.
#[inline]
fn node_size(node: &Node, use_disk: bool) -> u64 {
    if use_disk { node.disk_size } else { node.size }
}

/// Whether `node` is laid out given the pseudo-item flags.
#[inline]
fn is_visible(node: &Node, show_free: bool, show_unknown: bool) -> bool {
    match node.kind() {
        NodeKind::FreeSpace => show_free,
        NodeKind::Unknown => show_unknown,
        _ => true,
    }
}

#[inline]
fn has_hidden_special(children: &[Node], show_free: bool, show_unknown: bool) -> bool {
    children
        .iter()
        .any(|c| c.is_special() && !is_visible(c, show_free, show_unknown))
}

/// Squarify `sizes` (already sorted descending, all > 0) into `rc`. `sizes[i]`
/// corresponds to the original child `indices[i]`.
fn squarify_sizes(
    sizes: &[u64],
    indices: &[usize],
    total: u64,
    rc: Rect,
    mut f: impl FnMut(usize, Rect),
) {
    let n = sizes.len();
    if n == 0 || total == 0 {
        return;
    }
    let mut rc = rc;
    let mut remaining = total as f64;
    let mut start = 0;
    while start < n {
        let w = rc.w();
        let h = rc.h();
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let horizontal = w >= h;
        let side = if horizontal { h } else { w };
        let area_per_byte = w * h / remaining;

        let max_size = sizes[start] as f64;
        let mut end = start;
        let mut row_sum = 0.0;
        let mut worst = f64::INFINITY;
        while end < n {
            let s = sizes[end] as f64;
            let new_sum = row_sum + s;
            let thickness = new_sum * area_per_byte / side;
            let ratio = |sz: f64| {
                let len = sz * area_per_byte / thickness;
                if len > thickness {
                    len / thickness
                } else {
                    thickness / len
                }
            };
            let new_worst = ratio(max_size).max(ratio(s));
            if end > start && new_worst > worst {
                break;
            }
            worst = new_worst;
            row_sum = new_sum;
            end += 1;
        }

        let last_row = end == n;
        let frac = row_sum / remaining;
        if horizontal {
            let split = if last_row { rc.x1 } else { rc.x0 + w * frac };
            let mut y = rc.y0;
            for i in start..end {
                let y_next = if i + 1 == end {
                    rc.y1
                } else {
                    y + h * (sizes[i] as f64 / row_sum)
                };
                f(
                    indices[i],
                    Rect {
                        x0: rc.x0,
                        y0: y,
                        x1: split,
                        y1: y_next,
                    },
                );
                y = y_next;
            }
            rc.x0 = split;
        } else {
            let split = if last_row { rc.y1 } else { rc.y0 + h * frac };
            let mut x = rc.x0;
            for i in start..end {
                let x_next = if i + 1 == end {
                    rc.x1
                } else {
                    x + w * (sizes[i] as f64 / row_sum)
                };
                f(
                    indices[i],
                    Rect {
                        x0: x,
                        y0: rc.y0,
                        x1: x_next,
                        y1: split,
                    },
                );
                x = x_next;
            }
            rc.y0 = split;
        }
        remaining -= row_sum;
        start = end;
    }
}

/// Like [`squarify`], but skips hidden pseudo-items and can size cells by
/// their allocated (on-disk) bytes instead of their logical bytes. The
/// callback still gets the original child indices.
///
/// Children are stored sorted by logical size (as [`Tree`] keeps them), which
/// [`squarify`] relies on. Disk-size order can differ, so that path re-sorts
/// the visible children.
fn squarify_filtered(
    children: &[Node],
    total: u64,
    rc: Rect,
    show_free: bool,
    show_unknown: bool,
    use_disk: bool,
    f: impl FnMut(usize, Rect),
) {
    if !use_disk && !has_hidden_special(children, show_free, show_unknown) {
        squarify(children, total, rc, f);
        return;
    }

    let mut entries: Vec<(u64, usize)> = children
        .iter()
        .enumerate()
        .filter(|(_, c)| is_visible(c, show_free, show_unknown))
        .map(|(i, c)| (node_size(c, use_disk), i))
        .filter(|&(size, _)| size > 0)
        .collect();
    if entries.is_empty() {
        return;
    }
    if use_disk {
        entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    }
    let total: u64 = entries.iter().map(|&(size, _)| size).sum();
    let sizes: Vec<u64> = entries.iter().map(|&(size, _)| size).collect();
    let indices: Vec<usize> = entries.iter().map(|&(_, i)| i).collect();
    squarify_sizes(&sizes, &indices, total, rc, f);
}

#[derive(Clone, Copy)]
struct Surface([f64; 4]);

impl Surface {
    fn add_ridge(&mut self, rc: &Rect, h: f64) {
        let h4 = 4.0 * h;
        let w = rc.w();
        let hh = rc.h();
        if w > 0.0 {
            let wf = h4 / w;
            self.0[2] += wf * (rc.x1 + rc.x0);
            self.0[0] -= wf;
        }
        if hh > 0.0 {
            let hf = h4 / hh;
            self.0[3] += hf * (rc.y1 + rc.y0);
            self.0[1] -= hf;
        }
    }
}

/// Raw BGRA target shared between worker threads. Each leaf writes only the
/// pixels of its own rectangle, and rectangles never overlap.
#[derive(Clone, Copy)]
struct Target {
    ptr: *mut u8,
    w: usize,
    h: usize,
}
unsafe impl Send for Target {}
unsafe impl Sync for Target {}

#[inline]
fn px_range(a: f64, b: f64, max: usize) -> (usize, usize) {
    let lo = (a.round().max(0.0) as usize).min(max);
    let hi = (b.round().max(0.0) as usize).min(max);
    (lo, hi)
}

struct Light {
    lx: f64,
    ly: f64,
    lz: f64,
}

fn render_leaf(t: Target, rc: &Rect, s: &Surface, color: [f64; 3], light: &Light, cushion: bool) {
    let (x0, x1) = px_range(rc.x0, rc.x1, t.w);
    let (y0, y1) = px_range(rc.y0, rc.y1, t.h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Flat rectangles: one fill, no per-pixel lighting (easier to read for
    // some people, and cheaper to render).
    if !cushion {
        let r = color[0].clamp(0.0, 255.0) as u8;
        let g = color[1].clamp(0.0, 255.0) as u8;
        let b = color[2].clamp(0.0, 255.0) as u8;
        for iy in y0..y1 {
            let row = unsafe { t.ptr.add((iy * t.w + x0) * 4) };
            for k in 0..(x1 - x0) {
                unsafe {
                    let p = row.add(k * 4);
                    *p = b;
                    *p.add(1) = g;
                    *p.add(2) = r;
                    *p.add(3) = 255;
                }
            }
        }
        return;
    }
    let ia = AMBIENT;
    let is = 1.0 - ia;
    let contrast = BRIGHTNESS / PALETTE_BRIGHTNESS;
    let [s0, s1, s2, s3] = s.0;
    for iy in y0..y1 {
        let ny = -(2.0 * s1 * (iy as f64 + 0.5) + s3);
        let row = unsafe { t.ptr.add((iy * t.w + x0) * 4) };
        for (k, ix) in (x0..x1).enumerate() {
            let nx = -(2.0 * s0 * (ix as f64 + 0.5) + s2);
            let mut cosa =
                (nx * light.lx + ny * light.ly + light.lz) / (nx * nx + ny * ny + 1.0).sqrt();
            if cosa > 1.0 {
                cosa = 1.0;
            }
            let mut pixel = is * cosa;
            if pixel < 0.0 {
                pixel = 0.0;
            }
            pixel += ia;
            pixel *= contrast;
            let mut r = color[0] * pixel;
            let mut g = color[1] * pixel;
            let mut b = color[2] * pixel;
            normalize(&mut r, &mut g, &mut b);
            unsafe {
                let p = row.add(k * 4);
                *p = b as u8;
                *p.add(1) = g as u8;
                *p.add(2) = r as u8;
                *p.add(3) = 255;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    tree: &Tree,
    node: &Node,
    // Owner index of `node` ([`NO_OWNER`] when unknown or not collected).
    owner: u16,
    rc: Rect,
    surface: Surface,
    h: f64,
    as_root: bool,
    depth: u32,
    t: Target,
    light: &Light,
    opts: &RenderOptions,
) {
    if rc.w() <= 0.0 || rc.h() <= 0.0 {
        return;
    }
    let mut surface = surface;
    if !as_root {
        surface.add_ridge(&rc, h);
    }
    // Stop subdividing once a cell is under the minimum size in either
    // direction (1px by default; files below it merge into their parent).
    let min = opts.min_cell.max(0.5) as f64;
    let size = node_size(node, opts.use_disk);
    let children = tree.children(node);
    let subdivide =
        node.is_dir() && size > 0 && !children.is_empty() && rc.w() >= min && rc.h() >= min;
    if !subdivide {
        let ext = tree.dominant_ext(node);
        let mut color = shade_node(tree, node, opts);
        // A category highlight works in every colour mode. Otherwise
        // extension highlighting only applies when colouring by extension;
        // in age and category modes every leaf keeps its own colour.
        if let Some(want) = opts.highlight_owner {
            if want != owner {
                color = dim_color(color);
            }
        } else if let Some(want) = opts.highlight_category {
            if want as usize != tree.ext_category(ext).index() {
                color = dim_color(color);
            }
        } else if opts.color_mode == ColorMode::Extension
            && opts.highlight.is_some_and(|hl| hl != ext)
        {
            color = dim_color(color);
        }
        // Cell borders: shrink the leaf so a hairline of background shows.
        let rc = if opts.borders && rc.w() > 2.0 && rc.h() > 2.0 {
            Rect {
                x0: rc.x0 + 0.5,
                y0: rc.y0 + 0.5,
                x1: rc.x1 - 0.5,
                y1: rc.y1 - 0.5,
            }
        } else {
            rc
        };
        render_leaf(t, &rc, &surface, color, light, opts.cushion);
        return;
    }
    let h_child = h * SCALE;
    if depth < 4 && children.len() > 1 {
        let mut cells = Vec::with_capacity(children.len().min(4096));
        squarify_filtered(
            children,
            size,
            rc,
            opts.show_free,
            opts.show_unknown,
            opts.use_disk,
            |i, r| cells.push((i, r)),
        );
        cells.par_iter().with_min_len(8).for_each(|&(i, r)| {
            draw(
                tree,
                &children[i],
                tree.child_owner(node, i),
                r,
                surface,
                h_child,
                false,
                depth + 1,
                t,
                light,
                opts,
            );
        });
    } else {
        squarify_filtered(
            children,
            size,
            rc,
            opts.show_free,
            opts.show_unknown,
            opts.use_disk,
            |i, r| {
                draw(
                    tree,
                    &children[i],
                    tree.child_owner(node, i),
                    r,
                    surface,
                    h_child,
                    false,
                    depth + 1,
                    t,
                    light,
                    opts,
                );
            },
        );
    }
}

/// Render the subtree under `root` (the scan root, or the zoomed-in folder
/// of `tree`) into a `w`×`h` BGRA buffer. All display choices (pseudo-items,
/// size metric, palette, cushion shading, borders, …) come from `opts`.
/// When `highlight` is `Some(ext)`, leaves of that extension stay in full
/// colour and everything else is dimmed.
pub fn render(
    tree: &Tree,
    root: &Node,
    w: u32,
    h: u32,
    background: [u8; 3],
    opts: &RenderOptions,
) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut buf = vec![0u8; w * h * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = background[2];
        px[1] = background[1];
        px[2] = background[0];
        px[3] = 255;
    }
    let len = (LIGHT.0 * LIGHT.0 + LIGHT.1 * LIGHT.1 + LIGHT.2 * LIGHT.2).sqrt();
    let light = Light {
        lx: LIGHT.0 / len,
        ly: LIGHT.1 / len,
        lz: LIGHT.2 / len,
    };
    let t = Target {
        ptr: buf.as_mut_ptr(),
        w,
        h,
    };
    let rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w as f64,
        y1: h as f64,
    };
    draw(
        tree,
        root,
        opts.root_owner,
        rc,
        Surface([0.0; 4]),
        HEIGHT * opts.cushion_strength.clamp(0.0, 2.0) as f64,
        true,
        0,
        t,
        &light,
        opts,
    );
    buf
}

/// The node under pixel (`x`, `y`), descending as far as the layout goes.
/// The returned path is relative to `root`, a node of `tree`. Hidden pseudo-items are never
/// returned.
pub fn hit_test(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    x: f64,
    y: f64,
    opts: &RenderOptions,
) -> Option<NodePath> {
    let mut rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    };
    if !rc.contains(x, y) {
        return None;
    }
    let min = opts.min_cell.max(0.5) as f64;
    let mut node = root;
    let mut path = Vec::new();
    loop {
        let children = tree.children(node);
        if !node.is_dir() || children.is_empty() || rc.w() < min || rc.h() < min {
            return Some(path);
        }
        let mut found = None;
        squarify_filtered(
            children,
            node_size(node, opts.use_disk),
            rc,
            opts.show_free,
            opts.show_unknown,
            opts.use_disk,
            |i, r| {
                if found.is_none() && r.contains(x, y) {
                    found = Some((i, r));
                }
            },
        );
        let (i, r) = found?;
        path.push(i as u32);
        node = &children[i];
        rc = r;
    }
}

/// The rectangle of the node at `path` (relative to `root`, a node of
/// `tree`), if it is laid out at this size. Returns `None` for hidden pseudo-items.
pub fn rect_of(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    path: &[u32],
    opts: &RenderOptions,
) -> Option<Rect> {
    let mut rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    };
    let mut node = root;
    let min = opts.min_cell.max(0.5) as f64;
    for &want in path {
        if rc.w() < min || rc.h() < min {
            return Some(rc);
        }
        let children = tree.children(node);
        // Hidden pseudo-items have no rectangle.
        if let Some(next) = children.get(want as usize)
            && !is_visible(next, opts.show_free, opts.show_unknown)
        {
            return None;
        }
        let mut found = None;
        squarify_filtered(
            children,
            node_size(node, opts.use_disk),
            rc,
            opts.show_free,
            opts.show_unknown,
            opts.use_disk,
            |i, r| {
                if i == want as usize {
                    found = Some(r);
                }
            },
        );
        rc = found?;
        node = children.get(want as usize)?;
    }
    Some(rc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastdir_core::model::{Arena, DirBuilder, Subtree};
    use std::path::PathBuf;

    /// A one-level tree of files with the given `(name, size, disk_size)`.
    fn files_tree(files: &[(&str, u64, u64)]) -> Tree {
        let named: Vec<(&str, u64, u64, i64)> =
            files.iter().map(|&(n, s, d)| (n, s, d, 0)).collect();
        files_tree_at(&named)
    }

    /// Like [`files_tree`], with an explicit modification time per file.
    fn files_tree_at(files: &[(&str, u64, u64, i64)]) -> Tree {
        let mut arena = Arena::default();
        let mut root = DirBuilder::new(0);
        for &(name, size, disk, modified) in files {
            root.add_file(name, size, disk, modified);
        }
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn two_file_root() -> Tree {
        files_tree(&[("large.bin", 6, 6), ("small.txt", 4, 4)])
    }

    fn children_of(tree: &Tree) -> &[Node] {
        tree.children(tree.root())
    }

    #[test]
    fn squarify_tiles_the_whole_rect_without_gaps() {
        let tree = files_tree(&[("a", 6, 6), ("b", 3, 3), ("c", 1, 1)]);
        let rc = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 80.0,
        };
        let mut cells = Vec::new();
        squarify(children_of(&tree), 10, rc, |i, r| cells.push((i, r)));

        assert_eq!(cells.len(), 3);
        let area: f64 = cells.iter().map(|(_, r)| r.w() * r.h()).sum();
        assert!((area - 8000.0).abs() < 0.01, "area was {area}");
        for (_, r) in cells {
            assert!(r.w() > 0.0 && r.h() > 0.0);
            assert!(r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 100.0 && r.y1 <= 80.0);
        }
    }

    #[test]
    fn squarify_ignores_zero_sized_children() {
        let tree = files_tree(&[("a", 5, 5), ("b", 5, 5), ("empty", 0, 0)]);
        let rc = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        };
        let mut cells = Vec::new();
        squarify(children_of(&tree), 10, rc, |i, r| cells.push((i, r)));
        assert_eq!(
            cells.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn rect_of_and_hit_test_agree() {
        let tree = two_file_root();
        let root = tree.root();
        let opts = RenderOptions::default();
        let r = rect_of(&tree, root, 100.0, 100.0, &[0], &opts).unwrap();
        let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        assert_eq!(
            hit_test(&tree, root, 100.0, 100.0, x, y, &opts),
            Some(vec![0])
        );

        assert_eq!(hit_test(&tree, root, 100.0, 100.0, -1.0, 0.0, &opts), None);
        assert_eq!(rect_of(&tree, root, 100.0, 100.0, &[99], &opts), None);
    }

    #[test]
    fn disk_mode_sizes_cells_by_allocated_bytes() {
        // Logical order: compressed (100) then small (10).
        let tree = files_tree(&[("compressed.bin", 100, 512), ("small.bin", 10, 8192)]);
        let root = tree.root();
        let position = |name: &str| {
            children_of(&tree)
                .iter()
                .position(|n| tree.name(n) == name)
                .unwrap() as u32
        };
        let compressed = position("compressed.bin");
        let small = position("small.bin");

        let logical = rect_of(
            &tree,
            root,
            100.0,
            100.0,
            &[compressed],
            &RenderOptions::default(),
        )
        .unwrap();
        let disk_opts = RenderOptions {
            use_disk: true,
            ..RenderOptions::default()
        };
        let disk = rect_of(&tree, root, 100.0, 100.0, &[small], &disk_opts).unwrap();
        assert!(
            disk.w() * disk.h() > logical.w() * logical.h(),
            "disk-sized small file should be the larger cell"
        );
    }

    #[test]
    fn colours_follow_the_extension_rank() {
        assert_eq!(ext_color(NO_EXT), (200, 200, 200));
        assert_eq!(ext_color(0), (0, 0, 255));
        assert_eq!(ext_color(1), (255, 0, 0));
        assert_eq!(ext_color(u16::MAX - 1), (200, 200, 200));
    }

    #[test]
    fn category_colours_are_distinct_and_indexed() {
        use fastdir_core::categories::FileCategory;
        let mut seen = Vec::new();
        for category in FileCategory::ALL {
            let colour = category_color(category.index());
            assert!(!seen.contains(&colour), "duplicate {colour:?}");
            seen.push(colour);
        }
        assert_eq!(category_color(usize::MAX), OTHER);
    }

    #[test]
    fn category_mode_and_highlight_render_opaque_buffers() {
        let tree = files_tree(&[("video.mp4", 6, 6), ("notes.txt", 4, 4)]);
        let opts = RenderOptions {
            color_mode: ColorMode::Category,
            highlight_category: Some(
                fastdir_core::categories::builtin_category("txt").index() as u8
            ),
            ..RenderOptions::default()
        };
        let buf = render(&tree, tree.root(), 16, 16, [24, 24, 27], &opts);
        assert_eq!(buf.len(), 16 * 16 * 4);
        assert!(buf.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn palettes_stay_distinct_per_rank() {
        // Colour-blind palette differs from the default but keeps ranks apart.
        let a: Vec<_> = (0..8)
            .map(|r| ext_color_with(Palette::ColorBlind, r))
            .collect();
        let b: Vec<_> = (0..8).map(ext_color).collect();
        assert_ne!(a, b);
        let mut distinct = a.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), 8);
        // Mono is a grey ramp: channels equal, brighter for lower ranks.
        let m0 = ext_color_with(Palette::Mono, 0);
        let m1 = ext_color_with(Palette::Mono, 5);
        assert_eq!(m0.0, m0.1);
        assert_eq!(m0.1, m0.2);
        assert!(m0.0 > m1.0);
        assert_eq!(ext_color_with(Palette::Mono, u16::MAX - 1), (200, 200, 200));
    }

    #[test]
    fn age_bucket_mapping_uses_fixed_boundaries() {
        let now = 1_800_000_000;
        assert_eq!(age_bucket(0, now), AgeBucket::Unknown);
        assert_eq!(age_bucket(-5, now), AgeBucket::Unknown);
        assert_eq!(age_bucket(now + 100, now), AgeBucket::Week);
        assert_eq!(age_bucket(now, now), AgeBucket::Week);
        assert_eq!(age_bucket(now - (7 * DAY - 1), now), AgeBucket::Week);
        assert_eq!(age_bucket(now - 7 * DAY, now), AgeBucket::Month);
        assert_eq!(age_bucket(now - (30 * DAY - 1), now), AgeBucket::Month);
        assert_eq!(age_bucket(now - 30 * DAY, now), AgeBucket::SixMonths);
        assert_eq!(age_bucket(now - (HALF_YEAR - 1), now), AgeBucket::SixMonths);
        assert_eq!(age_bucket(now - HALF_YEAR, now), AgeBucket::Year);
        assert_eq!(age_bucket(now - (YEAR - 1), now), AgeBucket::Year);
        assert_eq!(age_bucket(now - YEAR, now), AgeBucket::ThreeYears);
        assert_eq!(
            age_bucket(now - (THREE_YEARS - 1), now),
            AgeBucket::ThreeYears
        );
        assert_eq!(age_bucket(now - THREE_YEARS, now), AgeBucket::Older);
    }

    #[test]
    fn age_colours_are_distinct_per_bucket_and_palette() {
        let all: Vec<_> = AgeBucket::ALL
            .iter()
            .map(|&b| age_bucket_color(b, Palette::Default))
            .collect();
        let mut distinct = all.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), all.len());

        let colorblind: Vec<_> = AgeBucket::ALL
            .iter()
            .map(|&b| age_bucket_color(b, Palette::ColorBlind))
            .collect();
        assert_ne!(all, colorblind);
        let mut distinct = colorblind.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), colorblind.len());

        // Mono stays grey and still separates every bucket.
        let mono: Vec<_> = AgeBucket::ALL
            .iter()
            .map(|&b| age_bucket_color(b, Palette::Mono))
            .collect();
        assert!(mono.iter().all(|(r, g, b)| r == g && g == b));
        let mut distinct = mono.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), mono.len());
    }

    #[test]
    fn age_colour_uses_buckets_for_known_and_unknown_times() {
        let now = 1_800_000_000;
        assert_eq!(age_color(now, now, Palette::Default), (231, 76, 60));
        assert_eq!(
            age_color(now - 4 * YEAR, now, Palette::Default),
            (142, 68, 173)
        );
        assert_eq!(age_color(0, now, Palette::Default), (128, 128, 128));
    }

    #[test]
    fn age_totals_sum_files_per_bucket() {
        let now = 1_800_000_000;
        let tree = files_tree_at(&[
            ("new.bin", 10, 10, now - 2 * DAY),
            ("month.bin", 20, 20, now - 20 * DAY),
            ("old.bin", 30, 30, now - 4 * YEAR),
            ("unknown.bin", 5, 5, 0),
        ]);
        let totals = age_bucket_totals(&tree, tree.root(), now, false);
        let by = |b: AgeBucket| totals.iter().find(|(x, _)| *x == b).unwrap().1;
        assert_eq!(by(AgeBucket::Week), 10);
        assert_eq!(by(AgeBucket::Month), 20);
        assert_eq!(by(AgeBucket::SixMonths), 0);
        assert_eq!(by(AgeBucket::Year), 0);
        assert_eq!(by(AgeBucket::ThreeYears), 0);
        assert_eq!(by(AgeBucket::Older), 30);
        assert_eq!(by(AgeBucket::Unknown), 5);
        assert_eq!(totals.iter().map(|(_, n)| n).sum::<u64>(), 65);
    }

    #[test]
    fn pseudo_item_colours_match_windirstat() {
        let mut tree = files_tree(&[("a.txt", 1, 1)]);
        tree.attach_volume_info(100, 50);
        let colour = |kind: NodeKind| {
            node_color(
                children_of(&tree)
                    .iter()
                    .find(|n| n.kind() == kind)
                    .unwrap(),
            )
        };
        assert_eq!(colour(NodeKind::FreeSpace), FREE_SPACE_COLOR);
        assert_eq!(colour(NodeKind::Unknown), UNKNOWN_COLOR);
        assert_eq!(colour(NodeKind::File), ext_color(0));
        assert_eq!(node_color(Subtree::file("x", 1, 0).root()), OTHER);
    }

    #[test]
    fn render_writes_an_opaque_bgra_buffer() {
        let tree = two_file_root();
        let buf = render(
            &tree,
            tree.root(),
            16,
            16,
            [24, 24, 27],
            &RenderOptions::default(),
        );
        assert_eq!(buf.len(), 16 * 16 * 4);
        assert!(buf.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn flat_and_bordered_renders_stay_opaque() {
        let tree = two_file_root();
        for opts in [
            RenderOptions {
                cushion: false,
                ..RenderOptions::default()
            },
            RenderOptions {
                borders: true,
                ..RenderOptions::default()
            },
            RenderOptions {
                color_mode: ColorMode::Age,
                palette: Palette::ColorBlind,
                ..RenderOptions::default()
            },
        ] {
            let buf = render(&tree, tree.root(), 16, 16, [24, 24, 27], &opts);
            assert_eq!(buf.len(), 16 * 16 * 4);
            assert!(buf.chunks_exact(4).all(|px| px[3] == 255));
        }
        // Flat mode fills each leaf with exactly its base colour.
        let flat = RenderOptions {
            cushion: false,
            min_cell: 100.0,
            ..RenderOptions::default()
        };
        let buf = render(&tree, tree.root(), 8, 8, [0, 0, 0], &flat);
        assert!(buf.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn hidden_pseudo_items_are_skipped() {
        let mut tree = files_tree(&[("real", 50, 50)]);
        // scanned 50, used 70 -> unknown 20, free 30.
        tree.attach_volume_info(100, 30);
        let root = tree.root();
        assert_eq!(root.size, 100);
        assert_eq!(tree.name(&children_of(&tree)[0]), "real");

        // Hidden: only the real file is laid out over the whole rect.
        let mut cells = Vec::new();
        squarify_filtered(
            children_of(&tree),
            root.size,
            test_rect(),
            false,
            false,
            false,
            |i, r| cells.push((i, r)),
        );
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].0, 0);
        assert!((cells[0].1.w() * cells[0].1.h() - 8000.0).abs() < 0.01);

        // Shown: all three take part.
        let mut cells = Vec::new();
        squarify_filtered(
            children_of(&tree),
            root.size,
            test_rect(),
            true,
            true,
            false,
            |i, r| cells.push((i, r)),
        );
        assert_eq!(cells.len(), 3);

        // hit_test never returns hidden pseudo-items.
        let hidden_opts = RenderOptions::default();
        let hit = hit_test(&tree, root, 100.0, 80.0, 50.0, 40.0, &hidden_opts).unwrap();
        assert_eq!(hit, vec![0]);
        // rect_of returns None for hidden pseudo-items.
        assert_eq!(rect_of(&tree, root, 100.0, 80.0, &[1], &hidden_opts), None);
        let shown_opts = RenderOptions {
            show_free: true,
            ..RenderOptions::default()
        };
        assert!(rect_of(&tree, root, 100.0, 80.0, &[1], &shown_opts).is_some());
    }

    /// Deterministic xorshift64: no external dependency, same sequence on
    /// every platform.
    fn rng_next(state: &mut u64) -> u64 {
        let mut x = *state;
        if x == 0 {
            x = 0x9E37_79B9_7F4A_7C15;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    fn check_cover_no_overlap_proportional(
        children: &[Node],
        total: u64,
        rc: Rect,
        cells: &[(usize, Rect)],
    ) {
        let parent_area = rc.w() * rc.h();
        // Covers the parent exactly.
        let area: f64 = cells.iter().map(|(_, r)| r.w() * r.h()).sum();
        assert!(
            (area - parent_area).abs() <= 1e-6 * parent_area.max(1.0),
            "area {area} != parent {parent_area} for {rc:?}"
        );
        // Within the parent bounds.
        for (_, r) in cells {
            assert!(
                r.x0 >= rc.x0 - 1e-9 && r.y0 >= rc.y0 - 1e-9,
                "{r:?} in {rc:?}"
            );
            assert!(
                r.x1 <= rc.x1 + 1e-9 && r.y1 <= rc.y1 + 1e-9,
                "{r:?} in {rc:?}"
            );
            assert!(r.w() > 0.0 && r.h() > 0.0, "{r:?}");
        }
        // Areas proportional to sizes.
        for (i, r) in cells {
            let expected = children[*i].size as f64 / total as f64 * parent_area;
            let got = r.w() * r.h();
            let tol = 1e-6 * expected.max(1.0);
            assert!(
                (got - expected).abs() <= tol,
                "cell {i}: area {got} != expected {expected}"
            );
        }
        // No two interiors overlap (edges may touch exactly).
        for a in 0..cells.len() {
            for b in (a + 1)..cells.len() {
                let (_, ra) = cells[a];
                let (_, rb) = cells[b];
                let x_overlap = ra.x0.min(rb.x0) < ra.x1.max(rb.x1) - 1e-9
                    && ra.x1.max(rb.x1) > ra.x0.min(rb.x0) + 1e-9
                    && ra.x0 < rb.x1 - 1e-9
                    && rb.x0 < ra.x1 - 1e-9;
                let y_overlap = ra.y0 < rb.y1 - 1e-9 && rb.y0 < ra.y1 - 1e-9;
                assert!(!(x_overlap && y_overlap), "cells overlap: {ra:?} vs {rb:?}");
            }
        }
        // Bounding box of the cells is the parent.
        let x0 = cells
            .iter()
            .map(|(_, r)| r.x0)
            .fold(f64::INFINITY, f64::min);
        let x1 = cells
            .iter()
            .map(|(_, r)| r.x1)
            .fold(f64::NEG_INFINITY, f64::max);
        let y0 = cells
            .iter()
            .map(|(_, r)| r.y0)
            .fold(f64::INFINITY, f64::min);
        let y1 = cells
            .iter()
            .map(|(_, r)| r.y1)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (x0 - rc.x0).abs() <= 1e-9 && (x1 - rc.x1).abs() <= 1e-9,
            "{cells:?}"
        );
        assert!(
            (y0 - rc.y0).abs() <= 1e-9 && (y1 - rc.y1).abs() <= 1e-9,
            "{cells:?}"
        );
    }

    #[test]
    fn squarify_covers_parent_without_overlap_and_proportional_areas() {
        // A mix of skewed, even and many-item distributions on several shapes,
        // including an offset rect.
        let cases: Vec<Vec<(&str, u64, u64)>> = vec![
            vec![("a", 6, 6), ("b", 3, 3), ("c", 1, 1)],
            vec![("big", 1000, 1000), ("mid", 100, 100), ("tiny", 1, 1)],
            vec![("a", 5, 5), ("b", 5, 5), ("c", 5, 5), ("d", 5, 5)],
            vec![("only", 42, 42)],
            (0..25)
                .map(|i| {
                    (
                        Box::leak(format!("f{i}").into_boxed_str()) as &str,
                        10 + i as u64 * 37,
                        10 + i as u64 * 37,
                    )
                })
                .collect(),
        ];
        let rects = [
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: 80.0,
            },
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: 100.0,
            },
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 50.0,
            },
            Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 50.0,
                y1: 200.0,
            },
            Rect {
                x0: 10.0,
                y0: 20.0,
                x1: 110.0,
                y1: 100.0,
            },
        ];
        for files in &cases {
            let owned: Vec<(String, u64, u64)> = files
                .iter()
                .map(|(n, s, d)| (n.to_string(), *s, *d))
                .collect();
            let refs: Vec<(&str, u64, u64)> =
                owned.iter().map(|(n, s, d)| (n.as_str(), *s, *d)).collect();
            let tree = files_tree(&refs);
            let children = children_of(&tree);
            let total = tree.root().size;
            for rc in rects {
                let mut cells = Vec::new();
                squarify(children, total, rc, |i, r| cells.push((i, r)));
                let expected = children.iter().filter(|c| c.size > 0).count();
                assert_eq!(cells.len(), expected, "files {files:?} in {rc:?}");
                if expected > 0 {
                    check_cover_no_overlap_proportional(children, total, rc, &cells);
                }
            }
        }
    }

    /// Build a two-level tree with deterministic pseudo-random sizes.
    fn random_nested_tree() -> Tree {
        let mut state = 0x1234_5678_9ABC_DEF1u64;
        let mut arena = Arena::default();
        let mut top = DirBuilder::new(0);
        // 3 sub-directories with 2..5 files each, plus top-level files.
        for d in 0..3 {
            let mut inner = DirBuilder::new(0);
            let n_files = 2 + (rng_next(&mut state) % 4) as usize;
            for f in 0..n_files {
                let size = 5 + rng_next(&mut state) % 900;
                inner.add_file(&format!("d{d}f{f}.bin"), size, size, 0);
            }
            top.add_dir(&format!("dir{d}"), inner.commit(&mut arena));
        }
        for f in 0..4 {
            let size = 5 + rng_next(&mut state) % 900;
            // Mix extensions so ext ranks vary.
            let name = if f % 2 == 0 {
                format!("top{f}.txt")
            } else {
                format!("top{f}.log")
            };
            top.add_file(&name, size, size, 0);
        }
        let root = top.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    fn collect_paths(tree: &Tree) -> Vec<NodePath> {
        let mut out = Vec::new();
        let mut stack = vec![(Vec::new(), tree.root())];
        while let Some((path, node)) = stack.pop() {
            out.push(path.clone());
            for (i, child) in tree.children(node).iter().enumerate() {
                let mut p = path.clone();
                p.push(i as u32);
                stack.push((p, child));
            }
        }
        out
    }

    #[test]
    fn hit_test_and_rect_of_agree_on_random_nested_trees() {
        let tree = random_nested_tree();
        // Large canvas so every leaf stays well above min_cell.
        let (w, h) = (1000.0, 800.0);
        let opts = RenderOptions::default();
        let root_rect = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: w,
            y1: h,
        };

        // Every laid-out node has a positive rect inside the canvas.
        let paths = collect_paths(&tree);
        assert!(paths.len() > 10);
        for path in &paths {
            let r = rect_of(&tree, tree.root(), w, h, path, &opts)
                .unwrap_or_else(|| panic!("no rect for {path:?}"));
            assert!(r.w() > 0.0 && r.h() > 0.0, "{path:?} -> {r:?}");
            assert!(
                r.x0 >= root_rect.x0 - 1e-9
                    && r.y0 >= root_rect.y0 - 1e-9
                    && r.x1 <= root_rect.x1 + 1e-9
                    && r.y1 <= root_rect.y1 + 1e-9,
                "{path:?} -> {r:?}"
            );
        }

        // Sibling rects at each level tile their parent without overlap.
        for path in &paths {
            let node = tree.get(path).unwrap();
            if !node.is_dir() || tree.children(node).is_empty() {
                continue;
            }
            let parent_rect = rect_of(&tree, tree.root(), w, h, path, &opts).unwrap();
            let parent_size = node.size;
            let mut cells: Vec<(usize, Rect)> = Vec::new();
            for (i, _) in tree.children(node).iter().enumerate() {
                let mut child_path = path.clone();
                child_path.push(i as u32);
                let r = rect_of(&tree, tree.root(), w, h, &child_path, &opts).unwrap();
                cells.push((i, r));
            }
            // Areas proportional to the child's share of the parent.
            let parent_area = parent_rect.w() * parent_rect.h();
            for (i, r) in &cells {
                let expected =
                    tree.children(node)[*i].size as f64 / parent_size as f64 * parent_area;
                let got = r.w() * r.h();
                assert!(
                    (got - expected).abs() <= 1e-6 * expected.max(1.0),
                    "{path:?} child {i}: {got} != {expected}"
                );
            }
            for a in 0..cells.len() {
                for b in (a + 1)..cells.len() {
                    let ra = cells[a].1;
                    let rb = cells[b].1;
                    let overlap = ra.x0 < rb.x1 - 1e-9
                        && rb.x0 < ra.x1 - 1e-9
                        && ra.y0 < rb.y1 - 1e-9
                        && rb.y0 < ra.y1 - 1e-9;
                    assert!(!overlap, "siblings overlap: {ra:?} vs {rb:?}");
                }
            }
        }

        // Leaf centres hit exactly; directory centres hit inside the directory.
        for path in &paths {
            let node = tree.get(path).unwrap();
            let r = rect_of(&tree, tree.root(), w, h, path, &opts).unwrap();
            let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            let hit = hit_test(&tree, tree.root(), w, h, x, y, &opts)
                .unwrap_or_else(|| panic!("no hit at ({x},{y}) for {path:?}"));
            if node.is_dir() && !tree.children(node).is_empty() {
                assert!(hit.starts_with(path), "dir {path:?} centre hit {hit:?}");
            } else {
                assert_eq!(hit, *path, "leaf {path:?} centre hit {hit:?}");
            }
            // The hit node's rect contains the sampled point.
            let hr = rect_of(&tree, tree.root(), w, h, &hit, &opts).unwrap();
            assert!(
                x >= hr.x0 - 1e-9 && x < hr.x1 + 1e-9 && y >= hr.y0 - 1e-9 && y < hr.y1 + 1e-9,
                "point ({x},{y}) not in {hr:?} for hit {hit:?}"
            );
        }

        // A grid of sample points round-trips through hit_test -> rect_of.
        for gx in 0..20 {
            for gy in 0..16 {
                let x = (gx as f64 + 0.5) / 20.0 * w;
                let y = (gy as f64 + 0.5) / 16.0 * h;
                let hit = hit_test(&tree, tree.root(), w, h, x, y, &opts).unwrap();
                let r = rect_of(&tree, tree.root(), w, h, &hit, &opts).unwrap();
                assert!(
                    x >= r.x0 - 1e-9 && x < r.x1 + 1e-9 && y >= r.y0 - 1e-9 && y < r.y1 + 1e-9,
                    "grid ({x},{y}) -> {hit:?} -> {r:?}"
                );
            }
        }
    }

    fn test_rect() -> Rect {
        Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 80.0,
        }
    }
}

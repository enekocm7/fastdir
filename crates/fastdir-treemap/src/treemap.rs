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

use fastdir_core::model::{NO_EXT, Node, NodeKind, NodePath};
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
const OTHER: (u8, u8, u8) = (200, 200, 200);
/// WinDirStat colours for the drive pseudo-items.
pub const FREE_SPACE_COLOR: (u8, u8, u8) = (128, 128, 128);
pub const UNKNOWN_COLOR: (u8, u8, u8) = (255, 255, 0);

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
    if ext == NO_EXT {
        return OTHER;
    }
    PALETTE.get(ext as usize).copied().unwrap_or(OTHER)
}

/// Display colour for a node: pseudo-items get their WinDirStat colours,
// otherwise the extension colour.
pub fn node_color(node: &Node) -> (u8, u8, u8) {
    match node.kind {
        NodeKind::FreeSpace => FREE_SPACE_COLOR,
        NodeKind::Unknown => UNKNOWN_COLOR,
        _ => ext_color(node.ext),
    }
}

fn shade_node(node: &Node) -> [f64; 3] {
    bright_color(node_color(node))
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

/// The metric used to size a cell: logical bytes or allocated bytes.
#[inline]
fn node_size(node: &Node, use_disk: bool) -> u64 {
    if use_disk { node.disk_size } else { node.size }
}

/// Whether `node` is laid out given the pseudo-item flags.
#[inline]
fn is_visible(node: &Node, show_free: bool, show_unknown: bool) -> bool {
    match node.kind {
        NodeKind::FreeSpace => show_free,
        NodeKind::Unknown => show_unknown,
        _ => true,
    }
}

#[inline]
fn has_hidden_special(children: &[Node], show_free: bool, show_unknown: bool) -> bool {
    children.iter().any(|c| {
        matches!(c.kind, NodeKind::FreeSpace | NodeKind::Unknown)
            && !is_visible(c, show_free, show_unknown)
    })
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
/// Children are stored sorted by logical size (as [`Node::finalize`] keeps
/// them), which [`squarify`] relies on. Disk-size order can differ, so that
/// path re-sorts the visible children.
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

fn render_leaf(t: Target, rc: &Rect, s: &Surface, color: [f64; 3], light: &Light) {
    let (x0, x1) = px_range(rc.x0, rc.x1, t.w);
    let (y0, y1) = px_range(rc.y0, rc.y1, t.h);
    if x0 >= x1 || y0 >= y1 {
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
    node: &Node,
    rc: Rect,
    surface: Surface,
    h: f64,
    as_root: bool,
    depth: u32,
    t: Target,
    light: &Light,
    highlight: Option<u16>,
    show_free: bool,
    show_unknown: bool,
    use_disk: bool,
) {
    if rc.w() <= 0.0 || rc.h() <= 0.0 {
        return;
    }
    let mut surface = surface;
    if !as_root {
        surface.add_ridge(&rc, h);
    }
    // Stop subdividing once a cell is under a pixel in either direction.
    let size = node_size(node, use_disk);
    let subdivide =
        node.is_dir() && size > 0 && !node.children.is_empty() && rc.w() >= 1.0 && rc.h() >= 1.0;
    if !subdivide {
        let ext = node.dominant_ext();
        let mut color = shade_node(node);
        if highlight.is_some_and(|hl| hl != ext) {
            color = dim_color(color);
        }
        render_leaf(t, &rc, &surface, color, light);
        return;
    }
    let h_child = h * SCALE;
    if depth < 4 && node.children.len() > 1 {
        let mut cells = Vec::with_capacity(node.children.len().min(4096));
        squarify_filtered(
            &node.children,
            size,
            rc,
            show_free,
            show_unknown,
            use_disk,
            |i, r| cells.push((i, r)),
        );
        cells.par_iter().with_min_len(8).for_each(|&(i, r)| {
            draw(
                &node.children[i],
                r,
                surface,
                h_child,
                false,
                depth + 1,
                t,
                light,
                highlight,
                show_free,
                show_unknown,
                use_disk,
            );
        });
    } else {
        squarify_filtered(
            &node.children,
            size,
            rc,
            show_free,
            show_unknown,
            use_disk,
            |i, r| {
                draw(
                    &node.children[i],
                    r,
                    surface,
                    h_child,
                    false,
                    depth + 1,
                    t,
                    light,
                    highlight,
                    show_free,
                    show_unknown,
                    use_disk,
                );
            },
        );
    }
}

/// Render the subtree under `root` (the scan root, or the zoomed-in folder)
/// into a `w`×`h` BGRA buffer. When `highlight` is `Some(ext)`, leaves of that
/// extension stay in full colour and everything else is dimmed. Drive
/// pseudo-items are only laid out when `show_free` / `show_unknown` are set
/// (both off by default, like WinDirStat). `use_disk` sizes cells by their
/// allocated bytes instead of their logical bytes.
#[allow(clippy::too_many_arguments)]
pub fn render(
    root: &Node,
    w: u32,
    h: u32,
    background: [u8; 3],
    highlight: Option<u16>,
    show_free: bool,
    show_unknown: bool,
    use_disk: bool,
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
        root,
        rc,
        Surface([0.0; 4]),
        HEIGHT,
        true,
        0,
        t,
        &light,
        highlight,
        show_free,
        show_unknown,
        use_disk,
    );
    buf
}

/// The node under pixel (`x`, `y`), descending as far as the layout goes.
/// The returned path is relative to `root`. Hidden pseudo-items are never
/// returned.
#[allow(clippy::too_many_arguments)]
pub fn hit_test(
    root: &Node,
    w: f64,
    h: f64,
    x: f64,
    y: f64,
    show_free: bool,
    show_unknown: bool,
    use_disk: bool,
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
    let mut node = root;
    let mut path = Vec::new();
    loop {
        if !node.is_dir() || node.children.is_empty() || rc.w() < 1.0 || rc.h() < 1.0 {
            return Some(path);
        }
        let mut found = None;
        squarify_filtered(
            &node.children,
            node_size(node, use_disk),
            rc,
            show_free,
            show_unknown,
            use_disk,
            |i, r| {
                if found.is_none() && r.contains(x, y) {
                    found = Some((i, r));
                }
            },
        );
        let (i, r) = found?;
        path.push(i as u32);
        node = &node.children[i];
        rc = r;
    }
}

/// The rectangle of the node at `path` (relative to `root`), if it is laid
/// out at this size. Returns `None` for hidden pseudo-items.
#[allow(clippy::too_many_arguments)]
pub fn rect_of(
    root: &Node,
    w: f64,
    h: f64,
    path: &[u32],
    show_free: bool,
    show_unknown: bool,
    use_disk: bool,
) -> Option<Rect> {
    let mut rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    };
    let mut node = root;
    for &want in path {
        if rc.w() < 1.0 || rc.h() < 1.0 {
            return Some(rc);
        }
        // Hidden pseudo-items have no rectangle.
        if let Some(next) = node.children.get(want as usize)
            && !is_visible(next, show_free, show_unknown)
        {
            return None;
        }
        let mut found = None;
        squarify_filtered(
            &node.children,
            node_size(node, use_disk),
            rc,
            show_free,
            show_unknown,
            use_disk,
            |i, r| {
                if i == want as usize {
                    found = Some(r);
                }
            },
        );
        rc = found?;
        node = node.children.get(want as usize)?;
    }
    Some(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Node {
        Node::file(name.into(), size, 0)
    }

    fn two_file_root() -> Node {
        let mut root = Node::dir("root".into(), 0);
        root.children.push(file("large.bin", 6));
        root.children.push(file("small.txt", 4));
        root.finalize();
        root
    }

    #[test]
    fn squarify_tiles_the_whole_rect_without_gaps() {
        let children = vec![file("a", 6), file("b", 3), file("c", 1)];
        let rc = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 80.0,
        };
        let mut cells = Vec::new();
        squarify(&children, 10, rc, |i, r| cells.push((i, r)));

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
        let children = vec![file("a", 5), file("b", 5), file("empty", 0)];
        let rc = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        };
        let mut cells = Vec::new();
        squarify(&children, 10, rc, |i, r| cells.push((i, r)));
        assert_eq!(
            cells.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn rect_of_and_hit_test_agree() {
        let root = two_file_root();
        let r = rect_of(&root, 100.0, 100.0, &[0], false, false, false).unwrap();
        let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        assert_eq!(
            hit_test(&root, 100.0, 100.0, x, y, false, false, false),
            Some(vec![0])
        );

        assert_eq!(
            hit_test(&root, 100.0, 100.0, -1.0, 0.0, false, false, false),
            None
        );
        assert_eq!(
            rect_of(&root, 100.0, 100.0, &[99], false, false, false),
            None
        );
    }

    #[test]
    fn disk_mode_sizes_cells_by_allocated_bytes() {
        let mut root = Node::dir("root".into(), 0);
        // Logical order: compressed (100) then small (10).
        root.children
            .push(Node::file_on_disk("compressed.bin".into(), 100, 512, 0));
        root.children
            .push(Node::file_on_disk("small.bin".into(), 10, 8192, 0));
        root.finalize();
        let compressed = root
            .children
            .iter()
            .position(|n| &*n.name == "compressed.bin")
            .unwrap() as u32;
        let small = root
            .children
            .iter()
            .position(|n| &*n.name == "small.bin")
            .unwrap() as u32;

        let logical = rect_of(&root, 100.0, 100.0, &[compressed], false, false, false).unwrap();
        let disk = rect_of(&root, 100.0, 100.0, &[small], false, false, true).unwrap();
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
    fn pseudo_item_colours_match_windirstat() {
        assert_eq!(node_color(&Node::free_space(1)), FREE_SPACE_COLOR);
        assert_eq!(node_color(&Node::unknown(1)), UNKNOWN_COLOR);
        assert_eq!(node_color(&file("a.txt", 1)), OTHER);
    }

    #[test]
    fn render_writes_an_opaque_bgra_buffer() {
        let root = two_file_root();
        let buf = render(&root, 16, 16, [24, 24, 27], None, false, false, false);
        assert_eq!(buf.len(), 16 * 16 * 4);
        assert!(buf.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn hidden_pseudo_items_are_skipped() {
        let mut root = Node::dir("root".into(), 0);
        root.children.push(file("real", 50));
        root.children.push(Node::free_space(30));
        root.children.push(Node::unknown(20));
        root.finalize();
        assert_eq!(root.size, 100);

        // Hidden: only the real file is laid out over the whole rect.
        let mut cells = Vec::new();
        squarify_filtered(
            &root.children,
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
            &root.children,
            root.size,
            test_rect(),
            true,
            true,
            false,
            |i, r| cells.push((i, r)),
        );
        assert_eq!(cells.len(), 3);

        // hit_test never returns hidden pseudo-items.
        let hit = hit_test(&root, 100.0, 80.0, 50.0, 40.0, false, false, false).unwrap();
        assert_eq!(hit, vec![0]);
        // rect_of returns None for hidden pseudo-items.
        assert_eq!(rect_of(&root, 100.0, 80.0, &[1], false, false, false), None);
        assert!(rect_of(&root, 100.0, 80.0, &[1], true, false, false).is_some());
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

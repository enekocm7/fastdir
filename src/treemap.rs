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

use crate::model::{NO_EXT, Node, NodePath, Tree};
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

fn shade_color(ext: u16) -> [f64; 3] {
    bright_color(ext_color(ext))
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
                if len > thickness { len / thickness } else { thickness / len }
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
            let mut cosa = (nx * light.lx + ny * light.ly + light.lz) / (nx * nx + ny * ny + 1.0).sqrt();
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

fn draw(node: &Node, rc: Rect, surface: Surface, h: f64, as_root: bool, depth: u32, t: Target, light: &Light) {
    if rc.w() <= 0.0 || rc.h() <= 0.0 {
        return;
    }
    let mut surface = surface;
    if !as_root {
        surface.add_ridge(&rc, h);
    }
    // Stop subdividing once a cell is under a pixel in either direction.
    let subdivide = node.is_dir() && node.size > 0 && !node.children.is_empty() && rc.w() >= 1.0 && rc.h() >= 1.0;
    if !subdivide {
        render_leaf(t, &rc, &surface, shade_color(node.dominant_ext()), light);
        return;
    }
    let h_child = h * SCALE;
    if depth < 4 && node.children.len() > 1 {
        let mut cells = Vec::with_capacity(node.children.len().min(4096));
        squarify(&node.children, node.size, rc, |i, r| cells.push((i, r)));
        cells.par_iter().with_min_len(8).for_each(|&(i, r)| {
            draw(&node.children[i], r, surface, h_child, false, depth + 1, t, light);
        });
    } else {
        squarify(&node.children, node.size, rc, |i, r| {
            draw(&node.children[i], r, surface, h_child, false, depth + 1, t, light);
        });
    }
}

/// Render the whole tree into a `w`×`h` BGRA buffer.
pub fn render(tree: &Tree, w: u32, h: u32, background: [u8; 3]) -> Vec<u8> {
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
    draw(&tree.root, rc, Surface([0.0; 4]), HEIGHT, true, 0, t, &light);
    buf
}

/// The node under pixel (`x`, `y`), descending as far as the layout goes.
pub fn hit_test(tree: &Tree, w: f64, h: f64, x: f64, y: f64) -> Option<NodePath> {
    let mut rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    };
    if !rc.contains(x, y) {
        return None;
    }
    let mut node = &tree.root;
    let mut path = Vec::new();
    loop {
        if !node.is_dir() || node.children.is_empty() || rc.w() < 1.0 || rc.h() < 1.0 {
            return Some(path);
        }
        let mut found = None;
        squarify(&node.children, node.size, rc, |i, r| {
            if found.is_none() && r.contains(x, y) {
                found = Some((i, r));
            }
        });
        let (i, r) = found?;
        path.push(i as u32);
        node = &node.children[i];
        rc = r;
    }
}

/// The rectangle of the node at `path`, if it is laid out at this size.
pub fn rect_of(tree: &Tree, w: f64, h: f64, path: &[u32]) -> Option<Rect> {
    let mut rc = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: w,
        y1: h,
    };
    let mut node = &tree.root;
    for &want in path {
        if rc.w() < 1.0 || rc.h() < 1.0 {
            return Some(rc);
        }
        let mut found = None;
        squarify(&node.children, node.size, rc, |i, r| {
            if i == want as usize {
                found = Some(r);
            }
        });
        rc = found?;
        node = node.children.get(want as usize)?;
    }
    Some(rc)
}

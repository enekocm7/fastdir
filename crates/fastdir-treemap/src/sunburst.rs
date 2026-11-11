//! Sunburst (radial) layout, rendered on the CPU in parallel.
//!
//! The folder being shown is the disc in the middle. Its children form the
//! first ring around it, each one an arc whose angle is its share of the
//! folder; their children form the next ring inside their parent's arc, and
//! so on outwards. Angles start at 12 o'clock and grow clockwise, largest
//! item first.
//!
//! Like the treemap, the layout is a pure function of the tree and the pixel
//! size: hit-testing and outlines re-run it along a single path instead of
//! keeping every arc in memory. It shares the treemap's [`RenderOptions`], so
//! pseudo-items, the size metric, palettes, colour modes and highlights all
//! behave the same in both views.

use crate::treemap::{
    Direction, RenderOptions, dim_color, has_hidden_special, is_dimmed, is_visible, node_size,
    normalize, shade_node,
};
use fastdir_core::diff::ScanDiff;
use fastdir_core::model::{Node, NodePath, Tree};
use rayon::prelude::*;
use std::f64::consts::TAU;

/// Which picture the map panel draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Squarified cushion treemap.
    #[default]
    Treemap,
    /// Radial sunburst.
    Sunburst,
}

/// At most this many levels are drawn around the centre; zoom in to see
/// deeper ones.
pub const MAX_RINGS: u32 = 8;
/// Background left around the disc (device px).
const MARGIN: f64 = 6.0;
/// Below this outer radius nothing is laid out.
const MIN_RADIUS: f64 = 12.0;
/// Half-width of the separator drawn along every arc edge (device px).
const LINE: f64 = 0.8;
/// How strongly the separator covers the arc colour.
const LINE_ALPHA: f64 = 0.85;

/// Where the rings are for a given pixel size and tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Centre of the disc (device px).
    pub cx: f64,
    pub cy: f64,
    /// Radius of the centre disc, which stands for the root itself.
    pub hole: f64,
    /// Thickness of one ring.
    pub ring: f64,
    /// Number of rings drawn (1..=[`MAX_RINGS`]).
    pub rings: u32,
    /// Outer radius of the last ring.
    pub radius: f64,
}

impl Geometry {
    fn new(w: f64, h: f64, rings: u32) -> Option<Self> {
        let radius = outer_radius(w, h)?;
        let rings = rings.clamp(1, MAX_RINGS);
        // Keep the centre big enough for a label even with many rings.
        let hole = (radius / (rings as f64 + 1.0)).max(radius * 0.16);
        Some(Self {
            cx: w / 2.0,
            cy: h / 2.0,
            hole,
            ring: (radius - hole) / rings as f64,
            rings,
            radius,
        })
    }

    /// Inner and outer radius of ring `depth` (1 is the innermost ring; 0 is
    /// the centre disc).
    pub fn ring_bounds(&self, depth: u32) -> (f64, f64) {
        if depth == 0 {
            return (0.0, self.hole);
        }
        let r0 = self.hole + (depth - 1) as f64 * self.ring;
        (r0, r0 + self.ring)
    }
}

/// One arc of the sunburst (or the centre disc, at depth 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    /// Ring number: 0 for the centre disc, 1 for the root's children, ...
    pub depth: u32,
    /// Start and end angle in radians, clockwise from 12 o'clock.
    pub a0: f64,
    pub a1: f64,
    /// Inner and outer radius (device px).
    pub r0: f64,
    pub r1: f64,
    /// Centre of the disc (device px).
    pub cx: f64,
    pub cy: f64,
}

impl Segment {
    /// The pixel at `angle` and distance `r` from the centre.
    pub fn point(&self, angle: f64, r: f64) -> (f64, f64) {
        (self.cx + r * angle.sin(), self.cy - r * angle.cos())
    }

    /// Whether the arc goes all the way round.
    pub fn is_full(&self) -> bool {
        self.a1 - self.a0 >= TAU - 1e-9
    }

    /// Whether pixel (`x`, `y`) lies inside the segment.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (dx, dy) = (x - self.cx, y - self.cy);
        let r = (dx * dx + dy * dy).sqrt();
        if r < self.r0 || r >= self.r1 {
            return false;
        }
        let a = angle_of(dx, dy);
        self.is_full() || (a >= self.a0 && a < self.a1)
    }

    /// The outline as closed polygons (device px), for drawing a selection
    /// frame: one loop for an arc or the centre disc, two (outer and inner
    /// circle) for a ring that goes all the way round.
    pub fn outline(&self) -> Vec<Vec<(f64, f64)>> {
        let arc = |r: f64, from: f64, to: f64| {
            // One vertex about every 3 px of arc.
            let n = (((to - from).abs() * r / 3.0).ceil() as usize).clamp(1, 720);
            (0..=n).map(move |i| self.point(from + (to - from) * (i as f64 / n as f64), r))
        };
        if self.is_full() || self.r0 <= 0.0 {
            let mut loops = vec![arc(self.r1, 0.0, TAU).collect::<Vec<_>>()];
            if self.r0 > 0.0 {
                loops.push(arc(self.r0, 0.0, TAU).collect());
            }
            return loops;
        }
        let mut points: Vec<(f64, f64)> = arc(self.r1, self.a0, self.a1).collect();
        points.extend(arc(self.r0, self.a1, self.a0));
        vec![points]
    }
}

fn outer_radius(w: f64, h: f64) -> Option<f64> {
    let radius = w.min(h) / 2.0 - MARGIN;
    (radius >= MIN_RADIUS).then_some(radius)
}

/// Arcs narrower than this are not laid out: `min_cell` pixels of arc on the
/// outermost ring.
fn min_angle(radius: f64, opts: &RenderOptions) -> f64 {
    opts.min_cell.max(0.5) as f64 / radius
}

/// Angle of the offset (`dx`, `dy`) from the centre: 0 at 12 o'clock,
/// clockwise, in `0..TAU`.
#[inline]
fn angle_of(dx: f64, dy: f64) -> f64 {
    let a = dx.atan2(-dy);
    let a = if a < 0.0 { a + TAU } else { a };
    if a >= TAU { 0.0 } else { a }
}

/// Split the arc `a0..a1` of `node` between its visible children, largest
/// first. `f` gets each child index with its start and end angle and returns
/// whether to go on. Children whose arc would be under `min` are not
/// reported (they are sorted by size, so the walk stops at the first one).
fn child_spans(
    tree: &Tree,
    node: &Node,
    a0: f64,
    a1: f64,
    min: f64,
    opts: &RenderOptions,
    mut f: impl FnMut(usize, f64, f64) -> bool,
) {
    let children = tree.children(node);
    let span = a1 - a0;
    let mut walk = |total: u64, sizes: &mut dyn Iterator<Item = (usize, u64)>| {
        if total == 0 {
            return;
        }
        let mut acc = 0u64;
        let mut start = a0;
        for (i, size) in sizes {
            if span * (size as f64 / total as f64) < min {
                return;
            }
            acc = acc.saturating_add(size);
            // Snap the last arc to the parent's end so siblings share exact
            // edges and nothing spills over.
            let end = if acc >= total {
                a1
            } else {
                a0 + span * (acc as f64 / total as f64)
            };
            if end > start && !f(i, start, end) {
                return;
            }
            if acc >= total {
                return;
            }
            start = end;
        }
    };

    // Children are stored sorted by logical size, so the common case needs
    // no allocation.
    if !opts.use_disk && !has_hidden_special(children, opts.show_free, opts.show_unknown) {
        let total = node_size(node, false);
        walk(
            total,
            &mut children
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.size))
                .take_while(|&(_, size)| size > 0),
        );
        return;
    }
    let mut entries: Vec<(usize, u64)> = children
        .iter()
        .enumerate()
        .filter(|(_, c)| is_visible(c, opts.show_free, opts.show_unknown))
        .map(|(i, c)| (i, node_size(c, opts.use_disk)))
        .filter(|&(_, size)| size > 0)
        .collect();
    if opts.use_disk {
        // Disk-size order can differ from the stored logical-size order.
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
    }
    let total = entries.iter().map(|&(_, size)| size).sum();
    walk(total, &mut entries.into_iter());
}

/// The laid-out children of `node` as `(child index, start, end)`.
fn visible_children(
    tree: &Tree,
    node: &Node,
    a0: f64,
    a1: f64,
    min: f64,
    opts: &RenderOptions,
) -> Vec<(usize, f64, f64)> {
    let mut out = Vec::new();
    child_spans(tree, node, a0, a1, min, opts, |i, c0, c1| {
        out.push((i, c0, c1));
        true
    });
    out
}

/// How many rings the tree under `root` fills, up to [`MAX_RINGS`].
fn visible_rings(tree: &Tree, root: &Node, min: f64, opts: &RenderOptions) -> u32 {
    #[allow(clippy::too_many_arguments)]
    fn go(
        tree: &Tree,
        node: &Node,
        a0: f64,
        a1: f64,
        depth: u32,
        min: f64,
        opts: &RenderOptions,
        deepest: &mut u32,
    ) {
        let children = tree.children(node);
        child_spans(tree, node, a0, a1, min, opts, |i, c0, c1| {
            *deepest = (*deepest).max(depth);
            if depth < MAX_RINGS && children[i].is_dir() {
                go(tree, &children[i], c0, c1, depth + 1, min, opts, deepest);
            }
            *deepest < MAX_RINGS
        });
    }
    let mut deepest = 0;
    go(tree, root, 0.0, TAU, 1, min, opts, &mut deepest);
    deepest.max(1)
}

/// The ring geometry for the subtree under `root` drawn into `w`×`h`
/// pixels, or `None` when the area is too small to draw anything.
pub fn geometry(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    opts: &RenderOptions,
) -> Option<Geometry> {
    let radius = outer_radius(w, h)?;
    let rings = visible_rings(tree, root, min_angle(radius, opts), opts);
    Geometry::new(w, h, rings)
}

/// Colour of the arc for `node`. A folder takes the colour of its largest
/// file, like a treemap cell too small to subdivide.
fn segment_color(
    tree: &Tree,
    node: &Node,
    owner: u16,
    opts: &RenderOptions,
    change: Option<&ScanDiff>,
) -> [f64; 3] {
    let ext = tree.dominant_ext(node);
    let color = shade_node(tree, node, ext, opts, change);
    if is_dimmed(tree, ext, owner, opts) {
        dim_color(color)
    } else {
        color
    }
}

/// One drawn arc: its angles and base colour.
struct Arc {
    a0: f64,
    a1: f64,
    color: [f64; 3],
}

struct Collect<'a> {
    tree: &'a Tree,
    min: f64,
    opts: &'a RenderOptions,
    change: Option<&'a ScanDiff>,
    /// Arcs per ring, in increasing angle order.
    rings: Vec<Vec<Arc>>,
}

fn collect(ctx: &mut Collect, node: &Node, a0: f64, a1: f64, depth: u32) {
    let (tree, opts, change) = (ctx.tree, ctx.opts, ctx.change);
    let children = tree.children(node);
    child_spans(tree, node, a0, a1, ctx.min, opts, |i, c0, c1| {
        let child = &children[i];
        let owner = tree.child_owner(node, i);
        ctx.rings[depth as usize - 1].push(Arc {
            a0: c0,
            a1: c1,
            color: segment_color(tree, child, owner, opts, change),
        });
        if depth < MAX_RINGS && child.is_dir() {
            collect(ctx, child, c0, c1, depth + 1);
        }
        true
    });
}

/// Render the subtree under `root` as a sunburst into a `w`×`h` BGRA buffer.
/// `change` is the comparison used by [`crate::ColorMode::Change`]. With
/// `opts.cushion` each ring is shaded from its inner to its outer edge;
/// `opts.borders` has no effect because arcs are always separated by a
/// hairline (the rings are unreadable without one).
pub fn render(
    tree: &Tree,
    root: &Node,
    w: u32,
    h: u32,
    background: [u8; 3],
    opts: &RenderOptions,
    change: Option<&ScanDiff>,
) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut buf = vec![0u8; w * h * 4];
    for px in buf.as_chunks_mut::<4>().0 {
        px[0] = background[2];
        px[1] = background[1];
        px[2] = background[0];
        px[3] = 255;
    }
    let Some(radius) = outer_radius(w as f64, h as f64) else {
        return buf;
    };
    let mut ctx = Collect {
        tree,
        min: min_angle(radius, opts),
        opts,
        change,
        rings: (0..MAX_RINGS).map(|_| Vec::new()).collect(),
    };
    collect(&mut ctx, root, 0.0, TAU, 1);
    let mut rings = ctx.rings;
    while rings.len() > 1 && rings.last().is_some_and(Vec::is_empty) {
        rings.pop();
    }
    let Some(g) = Geometry::new(w as f64, h as f64, rings.len() as u32) else {
        return buf;
    };

    let bg = [
        background[0] as f64,
        background[1] as f64,
        background[2] as f64,
    ];
    // The centre disc is the root: a muted tone that works on both themes.
    let centre = bg.map(|c| c + (128.0 - c) * 0.25);
    let shade = if opts.cushion {
        opts.cushion_strength.clamp(0.0, 2.0) as f64
    } else {
        0.0
    };

    buf.par_chunks_mut(w * 4).enumerate().for_each(|(iy, row)| {
        let dy = iy as f64 + 0.5 - g.cy;
        if dy.abs() >= g.radius {
            return;
        }
        // Only the pixels of this row that fall inside the disc.
        let half = (g.radius * g.radius - dy * dy).sqrt();
        let x_lo = (g.cx - half).floor().max(0.0) as usize;
        let x_hi = ((g.cx + half).ceil().max(0.0) as usize).min(w);
        for ix in x_lo..x_hi {
            let dx = ix as f64 + 0.5 - g.cx;
            let r = (dx * dx + dy * dy).sqrt();
            if r >= g.radius {
                continue;
            }
            // `line` is how much of the separator covers this pixel: 1 on
            // the boundary of the shape, 0 from `LINE` px inside it.
            let (color, line) = if r < g.hole {
                (centre, 1.0 - (g.hole - r) / LINE)
            } else {
                let ring = (((r - g.hole) / g.ring) as usize).min(rings.len() - 1);
                let theta = angle_of(dx, dy);
                let arcs = &rings[ring];
                let k = arcs.partition_point(|a| a.a1 <= theta);
                let Some(arc) = arcs.get(k).filter(|a| a.a0 <= theta) else {
                    continue;
                };
                let r0 = g.hole + ring as f64 * g.ring;
                let mut line = 1.0 - (r - r0).min(r0 + g.ring - r) / LINE;
                if arc.a1 - arc.a0 < TAU - 1e-9 {
                    // Thin arcs get a thinner separator, so a fan of slivers
                    // keeps its colours instead of turning into lines.
                    let width = (arc.a1 - arc.a0) * r;
                    let side = (theta - arc.a0).min(arc.a1 - theta) * r;
                    line = line.max(1.0 - side / LINE.min(width * 0.25));
                }
                // Lit from the centre: bright inner edge, darker outer edge.
                let t = (r - r0) / g.ring;
                let light = 1.0 + shade * (0.18 - 0.36 * t);
                (arc.color.map(|c| c * light), line)
            };
            // Blend towards the background near an edge: this draws the
            // separators and anti-aliases them in one go.
            let line = line.clamp(0.0, 1.0) * LINE_ALPHA;
            let [mut r8, mut g8, mut b8] = color;
            normalize(&mut r8, &mut g8, &mut b8);
            let mix = |c: f64, b: f64| (c + (b - c) * line).clamp(0.0, 255.0) as u8;
            let p = &mut row[ix * 4..ix * 4 + 4];
            p[0] = mix(b8, bg[2]);
            p[1] = mix(g8, bg[1]);
            p[2] = mix(r8, bg[0]);
            p[3] = 255;
        }
    });
    buf
}

/// The node under pixel (`x`, `y`): the arc there, or an empty path (the
/// root) on the centre disc. The path is relative to `root`. Returns `None`
/// outside the disc and on gaps where nothing is drawn; hidden pseudo-items
/// are never returned.
pub fn hit_test(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    x: f64,
    y: f64,
    opts: &RenderOptions,
) -> Option<NodePath> {
    let g = geometry(tree, root, w, h, opts)?;
    let (dx, dy) = (x - g.cx, y - g.cy);
    let r = (dx * dx + dy * dy).sqrt();
    if r >= g.radius {
        return None;
    }
    if r < g.hole {
        return Some(Vec::new());
    }
    let ring = (((r - g.hole) / g.ring) as u32 + 1).min(g.rings);
    let theta = angle_of(dx, dy);
    let min = min_angle(g.radius, opts);
    let mut node = root;
    let (mut a0, mut a1) = (0.0, TAU);
    let mut path = Vec::with_capacity(ring as usize);
    for _ in 0..ring {
        if !node.is_dir() {
            return None;
        }
        let mut found = None;
        child_spans(tree, node, a0, a1, min, opts, |i, c0, c1| {
            if theta >= c0 && theta < c1 {
                found = Some((i, c0, c1));
            }
            // Arcs come in angle order: stop once past the pointer.
            c1 <= theta
        });
        let (i, c0, c1) = found?;
        path.push(i as u32);
        node = &tree.children(node)[i];
        (a0, a1) = (c0, c1);
    }
    Some(path)
}

/// The segment of the node at `path` (relative to `root`): the centre disc
/// for an empty path. A node that is too deep or too small to have an arc of
/// its own gets the arc of its closest drawn ancestor, so a selection always
/// has something to outline. Returns `None` for hidden pseudo-items.
pub fn segment_of(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    path: &[u32],
    opts: &RenderOptions,
) -> Option<Segment> {
    let g = geometry(tree, root, w, h, opts)?;
    let min = min_angle(g.radius, opts);
    let mut node = root;
    let (mut a0, mut a1) = (0.0, TAU);
    let mut depth = 0;
    for &want in path {
        let child = tree.children(node).get(want as usize)?;
        if !is_visible(child, opts.show_free, opts.show_unknown) {
            return None;
        }
        if depth == g.rings {
            break;
        }
        let mut found = None;
        child_spans(tree, node, a0, a1, min, opts, |i, c0, c1| {
            if i == want as usize {
                found = Some((c0, c1));
            }
            found.is_none()
        });
        let Some((c0, c1)) = found else { break };
        (a0, a1) = (c0, c1);
        node = child;
        depth += 1;
    }
    let (r0, r1) = g.ring_bounds(depth);
    Some(Segment {
        depth,
        a0,
        a1,
        r0,
        r1,
        cx: g.cx,
        cy: g.cy,
    })
}

/// The arc to move to from the one at `from` (relative to `root`), for
/// keyboard navigation. Left and right walk round the ring (anticlockwise /
/// clockwise) to the next sibling, carrying on into the parent's neighbour
/// at the end of a folder and wrapping round on the first ring. Down steps
/// outwards into the largest child, up steps back in to the parent. An empty
/// `from` (or one that is not drawn) yields the largest arc of the first
/// ring. Returns `None` when there is nowhere to go.
pub fn neighbor(
    tree: &Tree,
    root: &Node,
    w: f64,
    h: f64,
    from: &[u32],
    dir: Direction,
    opts: &RenderOptions,
) -> Option<NodePath> {
    let g = geometry(tree, root, w, h, opts)?;
    let min = min_angle(g.radius, opts);
    // `levels[k]` holds the drawn children of the node at `path[..k]`, i.e.
    // the siblings of `path[k]`; `path` is the drawn prefix of `from`.
    let mut levels: Vec<Vec<(usize, f64, f64)>> = Vec::new();
    let mut path: NodePath = Vec::new();
    let mut node = root;
    let (mut a0, mut a1) = (0.0, TAU);
    while (path.len() as u32) < g.rings && node.is_dir() {
        let cells = visible_children(tree, node, a0, a1, min, opts);
        let next = from
            .get(path.len())
            .and_then(|&want| cells.iter().find(|c| c.0 == want as usize).copied());
        levels.push(cells);
        let Some((i, c0, c1)) = next else { break };
        path.push(i as u32);
        node = &tree.children(node)[i];
        (a0, a1) = (c0, c1);
    }
    let with = |prefix: &[u32], i: usize| {
        let mut out = prefix.to_vec();
        out.push(i as u32);
        out
    };
    if path.is_empty() {
        return levels.first()?.first().map(|c| with(&[], c.0));
    }
    match dir {
        Direction::Down => levels.get(path.len())?.first().map(|c| with(&path, c.0)),
        Direction::Up => (path.len() > 1).then(|| path[..path.len() - 1].to_vec()),
        Direction::Left | Direction::Right => {
            for level in (0..path.len()).rev() {
                let siblings = &levels[level];
                let pos = siblings.iter().position(|c| c.0 == path[level] as usize)?;
                let n = siblings.len();
                let next = if dir == Direction::Right {
                    (pos + 1 < n).then_some(pos + 1)
                } else {
                    pos.checked_sub(1)
                };
                // The first ring is a full circle, so it wraps round.
                let next = next.or_else(|| {
                    (level == 0 && n > 1).then(|| if dir == Direction::Right { 0 } else { n - 1 })
                });
                if let Some(next) = next {
                    return Some(with(&path[..level], siblings[next].0));
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treemap::ColorMode;
    use fastdir_core::model::{Arena, DirBuilder, NodeKind, Subtree};
    use std::path::PathBuf;

    const W: f64 = 400.0;
    const H: f64 = 300.0;

    fn opts() -> RenderOptions {
        RenderOptions {
            now: 1_700_000_000,
            ..RenderOptions::default()
        }
    }

    fn tree_of(root: DirBuilder, arena: Arena) -> Tree {
        let mut arena = arena;
        let root = root.commit(&mut arena);
        Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"))
    }

    /// `/root/{big.bin 6, small.txt 4}`.
    fn flat() -> Tree {
        let mut root = DirBuilder::new(0);
        root.add_file("big.bin", 6, 6, 0);
        root.add_file("small.txt", 4, 4, 0);
        tree_of(root, Arena::default())
    }

    /// `/root/{sub/{a.bin 30, b.txt 20, deep/{c.bin 10}}, top.bin 40}`.
    fn nested() -> Tree {
        let mut arena = Arena::default();
        let mut deep = DirBuilder::new(0);
        deep.add_file("c.bin", 10, 10, 0);
        let mut sub = DirBuilder::new(0);
        sub.add_file("a.bin", 30, 30, 0);
        sub.add_file("b.txt", 20, 20, 0);
        sub.add_dir("deep", deep.commit(&mut arena));
        let mut root = DirBuilder::new(0);
        root.add_dir("sub", sub.commit(&mut arena));
        root.add_file("top.bin", 40, 40, 0);
        tree_of(root, arena)
    }

    fn seg(tree: &Tree, path: &[u32]) -> Segment {
        segment_of(tree, tree.root(), W, H, path, &opts()).unwrap()
    }

    /// A pixel in the middle of `s`.
    fn middle(s: &Segment) -> (f64, f64) {
        s.point((s.a0 + s.a1) / 2.0, (s.r0 + s.r1) / 2.0)
    }

    fn pixel(buf: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * W as usize + x) * 4;
        [buf[i + 2], buf[i + 1], buf[i]]
    }

    #[test]
    fn angles_are_proportional_to_size_and_fill_the_circle() {
        let tree = flat();
        let (big, small) = (seg(&tree, &[0]), seg(&tree, &[1]));
        assert_eq!(big.a0, 0.0);
        assert!((big.a1 - TAU * 0.6).abs() < 1e-9);
        assert_eq!(small.a0, big.a1, "siblings share an exact edge");
        assert_eq!(small.a1, TAU, "the last arc closes the circle");
        assert_eq!((big.depth, small.depth), (1, 1));
    }

    #[test]
    fn rings_follow_the_tree_depth() {
        let g = geometry(&flat(), flat().root(), W, H, &opts()).unwrap();
        assert_eq!(g.rings, 1);
        let tree = nested();
        let g = geometry(&tree, tree.root(), W, H, &opts()).unwrap();
        assert_eq!(g.rings, 3);
        assert_eq!(g.radius, 150.0 - MARGIN);
        assert!((g.hole + g.ring * 3.0 - g.radius).abs() < 1e-9);
        // Rings are stacked without gaps.
        assert_eq!(g.ring_bounds(0), (0.0, g.hole));
        assert_eq!(g.ring_bounds(1).0, g.hole);
        assert_eq!(g.ring_bounds(2).0, g.ring_bounds(1).1);
    }

    #[test]
    fn children_stay_inside_their_parents_arc() {
        let tree = nested();
        let sub = seg(&tree, &[0]);
        let a = seg(&tree, &[0, 0]);
        let deep = seg(&tree, &[0, 2]);
        let c = seg(&tree, &[0, 2, 0]);
        assert_eq!(a.a0, sub.a0);
        assert_eq!(deep.a1, sub.a1);
        assert_eq!(
            (c.a0, c.a1),
            (deep.a0, deep.a1),
            "an only child fills its parent"
        );
        assert_eq!((a.depth, deep.depth, c.depth), (2, 2, 3));
        assert_eq!(c.r0, deep.r1);
    }

    #[test]
    fn hit_test_finds_every_drawn_segment() {
        let tree = nested();
        for path in [
            vec![],
            vec![0],
            vec![1],
            vec![0, 0],
            vec![0, 1],
            vec![0, 2],
            vec![0, 2, 0],
        ] {
            let s = seg(&tree, &path);
            let (x, y) = middle(&s);
            assert!(s.contains(x, y));
            assert_eq!(
                hit_test(&tree, tree.root(), W, H, x, y, &opts()),
                Some(path.clone()),
                "path {path:?}"
            );
        }
    }

    #[test]
    fn hit_test_misses_outside_the_disc_and_on_gaps() {
        let tree = nested();
        let o = opts();
        assert_eq!(hit_test(&tree, tree.root(), W, H, 1.0, 1.0, &o), None);
        assert_eq!(hit_test(&tree, tree.root(), W, H, W / 2.0, 1.0, &o), None);
        // Ring 2 over `top.bin` (a file, so nothing lies beyond it).
        let top = seg(&tree, &[1]);
        let (x, y) = top.point((top.a0 + top.a1) / 2.0, top.r1 + 5.0);
        assert_eq!(hit_test(&tree, tree.root(), W, H, x, y, &o), None);
        // Too small to lay out at all.
        assert_eq!(
            hit_test(&tree, tree.root(), 20.0, 20.0, 10.0, 10.0, &o),
            None
        );
        assert!(segment_of(&tree, tree.root(), 20.0, 20.0, &[], &o).is_none());
    }

    #[test]
    fn hidden_pseudo_items_take_no_angle() {
        let mut root = DirBuilder::new(0);
        root.add_special(NodeKind::FreeSpace, "<Free space>", 60, 60, 0);
        root.add_file("a.bin", 30, 30, 0);
        root.add_file("b.bin", 10, 10, 0);
        let tree = tree_of(root, Arena::default());
        let free = tree
            .children(tree.root())
            .iter()
            .position(|c| c.kind() == NodeKind::FreeSpace)
            .unwrap() as u32;
        let file = tree
            .children(tree.root())
            .iter()
            .position(|c| c.size == 30)
            .unwrap() as u32;

        let hidden = opts();
        assert!(segment_of(&tree, tree.root(), W, H, &[free], &hidden).is_none());
        let s = segment_of(&tree, tree.root(), W, H, &[file], &hidden).unwrap();
        assert!((s.a1 - s.a0 - TAU * 0.75).abs() < 1e-9);

        let shown = RenderOptions {
            show_free: true,
            ..opts()
        };
        let s = segment_of(&tree, tree.root(), W, H, &[free], &shown).unwrap();
        assert!((s.a1 - s.a0 - TAU * 0.6).abs() < 1e-9);
        let s = segment_of(&tree, tree.root(), W, H, &[file], &shown).unwrap();
        assert!((s.a1 - s.a0 - TAU * 0.3).abs() < 1e-9);
    }

    #[test]
    fn disk_size_metric_reorders_and_resizes() {
        let mut root = DirBuilder::new(0);
        root.add_file("sparse.bin", 90, 10, 0);
        root.add_file("dense.bin", 10, 30, 0);
        let tree = tree_of(root, Arena::default());
        let disk = RenderOptions {
            use_disk: true,
            ..opts()
        };
        // `dense.bin` (index 1 by logical size) is the largest on disk, so
        // it comes first and takes three quarters.
        let dense = segment_of(&tree, tree.root(), W, H, &[1], &disk).unwrap();
        assert_eq!(dense.a0, 0.0);
        assert!((dense.a1 - TAU * 0.75).abs() < 1e-9);
        let (x, y) = middle(&dense);
        assert_eq!(
            hit_test(&tree, tree.root(), W, H, x, y, &disk),
            Some(vec![1])
        );
    }

    #[test]
    fn tiny_arcs_are_dropped_and_fall_back_to_the_parent() {
        let mut root = DirBuilder::new(0);
        root.add_file("huge.bin", 1_000_000, 1_000_000, 0);
        root.add_file("speck.txt", 1, 1, 0);
        let tree = tree_of(root, Arena::default());
        let centre = seg(&tree, &[]);
        assert_eq!(seg(&tree, &[1]), centre, "no arc of its own");
        let huge = seg(&tree, &[0]);
        // Just before 12 o'clock is where the speck would have been.
        let (x, y) = huge.point(TAU - 1e-7, (huge.r0 + huge.r1) / 2.0);
        assert_eq!(hit_test(&tree, tree.root(), W, H, x, y, &opts()), None);
    }

    #[test]
    fn deeper_levels_than_max_rings_use_the_last_ring() {
        let mut arena = Arena::default();
        let mut dir = DirBuilder::new(0);
        dir.add_file("leaf.bin", 10, 10, 0);
        for i in 0..12 {
            let mut parent = DirBuilder::new(0);
            parent.add_dir(&format!("d{i}"), dir.commit(&mut arena));
            dir = parent;
        }
        let tree = tree_of(dir, arena);
        let g = geometry(&tree, tree.root(), W, H, &opts()).unwrap();
        assert_eq!(g.rings, MAX_RINGS);
        let deep = seg(&tree, &[0; 13]);
        assert_eq!(deep.depth, MAX_RINGS);
        assert!((deep.r1 - g.radius).abs() < 1e-9);
        // The outermost ring resolves to the folder drawn there.
        let (x, y) = middle(&deep);
        assert_eq!(
            hit_test(&tree, tree.root(), W, H, x, y, &opts()),
            Some(vec![0; MAX_RINGS as usize])
        );
    }

    #[test]
    fn render_paints_arcs_centre_and_leaves_the_rest() {
        let tree = nested();
        let bg = [10, 20, 30];
        let flat_opts = RenderOptions {
            cushion: false,
            ..opts()
        };
        let buf = render(&tree, tree.root(), W as u32, H as u32, bg, &flat_opts, None);
        assert_eq!(buf.len(), W as usize * H as usize * 4);
        assert!(buf.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        assert_eq!(pixel(&buf, 0, 0), bg, "corners stay background");
        assert_ne!(pixel(&buf, 200, 150), bg, "the centre disc is drawn");

        // `top.bin` and `a.bin` share an extension, `b.txt` has another.
        let at = |path: &[u32]| {
            let (x, y) = middle(&seg(&tree, path));
            pixel(&buf, x as usize, y as usize)
        };
        assert_ne!(at(&[1]), bg);
        assert_eq!(at(&[1]), at(&[0, 0]));
        assert_ne!(at(&[0, 0]), at(&[0, 1]));
        // A folder takes its largest file's colour.
        assert_eq!(at(&[0]), at(&[0, 0]));

        // Beyond a file there is nothing: background.
        let top = seg(&tree, &[1]);
        let (x, y) = top.point((top.a0 + top.a1) / 2.0, top.r1 + top.r1 - top.r0);
        assert_eq!(pixel(&buf, x as usize, y as usize), bg);
    }

    #[test]
    fn render_matches_hit_test_everywhere() {
        // Every pixel well inside a shape is painted, and every pixel the
        // hit test rejects keeps the background.
        let tree = nested();
        let bg = [1, 2, 3];
        let o = opts();
        let buf = render(&tree, tree.root(), W as u32, H as u32, bg, &o, None);
        for y in (0..H as usize).step_by(3) {
            for x in (0..W as usize).step_by(3) {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                match hit_test(&tree, tree.root(), W, H, fx, fy, &o) {
                    None => assert_eq!(pixel(&buf, x, y), bg, "({x}, {y})"),
                    Some(path) => {
                        let s = segment_of(&tree, tree.root(), W, H, &path, &o).unwrap();
                        assert!(s.contains(fx, fy), "({x}, {y}) {path:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn highlight_dims_the_other_segments() {
        let tree = nested();
        let bg = [0, 0, 0];
        let base = RenderOptions {
            cushion: false,
            ..opts()
        };
        let bin = tree.children(tree.root())[1].ext;
        let lit = RenderOptions {
            highlight: Some(bin),
            ..base
        };
        let plain = render(&tree, tree.root(), W as u32, H as u32, bg, &base, None);
        let dimmed = render(&tree, tree.root(), W as u32, H as u32, bg, &lit, None);
        let at = |buf: &[u8], path: &[u32]| {
            let (x, y) = middle(&seg(&tree, path));
            pixel(buf, x as usize, y as usize)
        };
        assert_eq!(at(&plain, &[1]), at(&dimmed, &[1]), "highlighted stays");
        assert_ne!(at(&plain, &[0, 1]), at(&dimmed, &[0, 1]), "others fade");
        // Age colours ignore the extension highlight, as in the treemap.
        let age = RenderOptions {
            color_mode: ColorMode::Age,
            ..base
        };
        let age_lit = RenderOptions {
            highlight: Some(bin),
            ..age
        };
        assert_eq!(
            render(&tree, tree.root(), W as u32, H as u32, bg, &age, None),
            render(&tree, tree.root(), W as u32, H as u32, bg, &age_lit, None)
        );
    }

    #[test]
    fn cushion_shades_each_ring_from_bright_to_dark() {
        let tree = flat();
        let buf = render(
            &tree,
            tree.root(),
            W as u32,
            H as u32,
            [0, 0, 0],
            &opts(),
            None,
        );
        let s = seg(&tree, &[0]);
        let mid = (s.a0 + s.a1) / 2.0;
        let sum = |r: f64| {
            let (x, y) = s.point(mid, r);
            pixel(&buf, x as usize, y as usize)
                .iter()
                .map(|&c| c as u32)
                .sum::<u32>()
        };
        assert!(sum(s.r0 + 4.0) > sum(s.r1 - 4.0));
    }

    #[test]
    fn neighbor_walks_round_the_ring_and_between_rings() {
        let tree = nested();
        let o = opts();
        let go = |from: &[u32], dir| neighbor(&tree, tree.root(), W, H, from, dir, &o);
        // Nothing selected: the largest arc of the first ring.
        assert_eq!(go(&[], Direction::Down), Some(vec![0]));
        // Round the first ring, wrapping.
        assert_eq!(go(&[0], Direction::Right), Some(vec![1]));
        assert_eq!(go(&[1], Direction::Right), Some(vec![0]));
        assert_eq!(go(&[0], Direction::Left), Some(vec![1]));
        // Outwards and back in.
        assert_eq!(go(&[0], Direction::Down), Some(vec![0, 0]));
        assert_eq!(go(&[0, 0], Direction::Up), Some(vec![0]));
        assert_eq!(go(&[0], Direction::Up), None);
        assert_eq!(go(&[1], Direction::Down), None, "a file has no ring beyond");
        // Siblings, then on to the parent's neighbour at the end.
        assert_eq!(go(&[0, 0], Direction::Right), Some(vec![0, 1]));
        assert_eq!(go(&[0, 2], Direction::Right), Some(vec![1]));
        assert_eq!(go(&[0, 0], Direction::Left), Some(vec![1]));
        assert_eq!(go(&[0, 2, 0], Direction::Left), Some(vec![0, 1]));
    }

    #[test]
    fn outline_closes_around_the_segment() {
        let tree = nested();
        let arc = seg(&tree, &[0]);
        let loops = arc.outline();
        assert_eq!(loops.len(), 1);
        let dist = |&(x, y): &(f64, f64)| ((x - arc.cx).powi(2) + (y - arc.cy).powi(2)).sqrt();
        assert!(loops[0].iter().all(|p| {
            let d = dist(p);
            (d - arc.r0).abs() < 1e-6 || (d - arc.r1).abs() < 1e-6
        }));
        assert_eq!(loops[0][0], arc.point(arc.a0, arc.r1));

        // The centre disc is one circle; a full ring is two.
        assert_eq!(seg(&tree, &[]).outline().len(), 1);
        let mut root = DirBuilder::new(0);
        root.add_file("only.bin", 5, 5, 0);
        let single = tree_of(root, Arena::default());
        let ring = seg(&single, &[0]);
        assert!(ring.is_full());
        assert_eq!(ring.outline().len(), 2);
    }

    #[test]
    fn empty_folder_still_has_a_centre() {
        let tree = tree_of(DirBuilder::new(0), Arena::default());
        let o = opts();
        assert_eq!(geometry(&tree, tree.root(), W, H, &o).unwrap().rings, 1);
        assert_eq!(
            hit_test(&tree, tree.root(), W, H, W / 2.0, H / 2.0, &o),
            Some(vec![])
        );
        assert_eq!(
            neighbor(&tree, tree.root(), W, H, &[], Direction::Down, &o),
            None
        );
        let buf = render(&tree, tree.root(), W as u32, H as u32, [0, 0, 0], &o, None);
        assert_ne!(pixel(&buf, 200, 150), [0, 0, 0]);
    }
}

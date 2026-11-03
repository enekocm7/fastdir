//! The treemap panel: shows the cushion image rendered off the main thread
//! and turns clicks into selections. It can be zoomed into any folder; a
//! breadcrumb above the image leads back up.

use fastdir_core::diff::ScanDiff;
use fastdir_core::format;
use fastdir_core::model::{Node, NodePath, Tree};
use fastdir_treemap::{self as treemap, AgeBucket, ColorMode, Palette, RenderOptions};
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use image::{Frame, RgbaImage};
use parking_lot::RwLock;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// While a scan is running, redraw the preview at most this often, so a tree
/// that keeps growing does not keep the renderer busy.
const PARTIAL_REFRESH: Duration = Duration::from_millis(1000);

/// Ctrl+click (Cmd+click on macOS) adds to or removes from the selection.
pub fn is_toggle_click(m: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform
    } else {
        m.control
    }
}

pub enum TreemapEvent {
    /// The user clicked a cell. `toggle` is set for Ctrl+click (Cmd+click on
    /// macOS) to add to or remove from the selection, `context` for a right click.
    Select {
        path: NodePath,
        toggle: bool,
        context: bool,
    },
    /// Double-click: open the item.
    Activate(NodePath),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    generation: u64,
    w: u32,
    h: u32,
    dark: bool,
    highlight: Option<u16>,
    highlight_category: Option<u8>,
    highlight_owner: Option<u16>,
    /// Bumped whenever the zoom root changes.
    zoom: u64,
    /// Bumped whenever the comparison overlay changes.
    change: u64,
    show_free: bool,
    show_unknown: bool,
    use_disk_size: bool,
    palette: Palette,
    color_mode: ColorMode,
    cushion: bool,
    cushion_strength: u32,
    borders: bool,
    min_cell: u32,
}

impl Key {
    /// Floats are stored as bits so the key stays `Eq` (no NaN is ever stored).
    #[allow(clippy::too_many_arguments)]
    fn new(
        generation: u64,
        w: u32,
        h: u32,
        dark: bool,
        highlight: Option<u16>,
        highlight_category: Option<u8>,
        highlight_owner: Option<u16>,
        zoom: u64,
        change: u64,
        opts: RenderOptions,
    ) -> Self {
        Self {
            generation,
            w,
            h,
            dark,
            highlight,
            highlight_category,
            highlight_owner,
            zoom,
            change,
            show_free: opts.show_free,
            show_unknown: opts.show_unknown,
            use_disk_size: opts.use_disk,
            palette: opts.palette,
            color_mode: opts.color_mode,
            cushion: opts.cushion,
            cushion_strength: opts.cushion_strength.to_bits(),
            borders: opts.borders,
            min_cell: opts.min_cell.to_bits(),
        }
    }

    /// The render options this key was built from (`highlight` included).
    /// `now` is refreshed so age colours never go stale.
    fn opts(&self) -> RenderOptions {
        RenderOptions {
            highlight: self.highlight,
            highlight_category: self.highlight_category,
            highlight_owner: self.highlight_owner,
            root_owner: fastdir_core::model::NO_OWNER,
            show_free: self.show_free,
            show_unknown: self.show_unknown,
            use_disk: self.use_disk_size,
            palette: self.palette,
            color_mode: self.color_mode,
            cushion: self.cushion,
            cushion_strength: f32::from_bits(self.cushion_strength),
            borders: self.borders,
            min_cell: f32::from_bits(self.min_cell),
            now: RenderOptions::default().now,
        }
    }
}

pub struct TreemapView {
    tree: Option<Arc<RwLock<Tree>>>,
    image: Option<Arc<RenderImage>>,
    image_key: Option<Key>,
    pending: Option<Key>,
    /// Bytes per age bucket for the current render, used by the age legend.
    age_totals: Option<[(AgeBucket, u64); 7]>,
    /// Comparison overlay used by [`ColorMode::Change`].
    change: Option<Arc<ScanDiff>>,
    change_gen: u64,
    stale: Rc<std::cell::RefCell<Vec<Arc<RenderImage>>>>,
    /// Every selected node; each one gets an outline.
    selection: Vec<NodePath>,
    highlight: Option<u16>,
    highlight_category: Option<u8>,
    highlight_owner: Option<u16>,
    /// Folder the treemap is rooted at (`[]` is the scan root).
    zoom: NodePath,
    /// File-system path of `zoom`, used to find it again after the tree
    /// changes (indices shift when sizes change or items are removed).
    zoom_fs: Option<PathBuf>,
    zoom_gen: u64,
    hover: Option<(NodePath, SharedString)>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scale: Rc<Cell<f32>>,
    rendering: bool,
    show_free: bool,
    show_unknown: bool,
    use_disk_size: bool,
    palette: Palette,
    color_mode: ColorMode,
    cushion: bool,
    cushion_strength: f32,
    borders: bool,
    min_cell: f32,
    /// Format hover sizes with decimal (SI) units instead of binary ones.
    decimal_sizes: bool,
    /// Resolved UI language code for hover labels and hints.
    lang: String,
    /// The tree is still being scanned, so previews are throttled.
    partial: bool,
    last_render: Option<Instant>,
    _task: Option<Task<()>>,
}

impl EventEmitter<TreemapEvent> for TreemapView {}

impl TreemapView {
    pub fn new() -> Self {
        Self {
            tree: None,
            image: None,
            image_key: None,
            pending: None,
            age_totals: None,
            change: None,
            change_gen: 0,
            stale: Default::default(),
            selection: Vec::new(),
            highlight: None,
            highlight_category: None,
            highlight_owner: None,
            zoom: Vec::new(),
            zoom_fs: None,
            zoom_gen: 0,
            hover: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            scale: Rc::new(Cell::new(1.0)),
            rendering: false,
            show_free: false,
            show_unknown: false,
            use_disk_size: false,
            palette: Palette::Default,
            color_mode: ColorMode::Extension,
            cushion: true,
            cushion_strength: 1.0,
            borders: false,
            min_cell: 1.0,
            decimal_sizes: false,
            lang: "en".to_string(),
            partial: false,
            last_render: None,
            _task: None,
        }
    }

    /// Push all `[treemap]` display settings at once (`highlight` is managed
    /// separately through [`Self::set_highlight`]). Anything that changed
    /// redraws the image.
    pub fn set_render(
        &mut self,
        opts: &RenderOptions,
        decimal_sizes: bool,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        changed |= set_if_diff(&mut self.show_free, opts.show_free);
        changed |= set_if_diff(&mut self.show_unknown, opts.show_unknown);
        changed |= set_if_diff(&mut self.use_disk_size, opts.use_disk);
        changed |= set_if_diff(&mut self.palette, opts.palette);
        changed |= set_if_diff(&mut self.color_mode, opts.color_mode);
        changed |= set_if_diff(&mut self.cushion, opts.cushion);
        changed |= set_if_diff(&mut self.cushion_strength, opts.cushion_strength);
        changed |= set_if_diff(&mut self.borders, opts.borders);
        changed |= set_if_diff(&mut self.min_cell, opts.min_cell);
        changed |= set_if_diff(&mut self.decimal_sizes, decimal_sizes);
        if changed {
            // The legend (and its totals) belongs to the previous render.
            self.age_totals = None;
            cx.notify();
        }
    }

    /// Switch the language of hover labels and hints. Cached hover text is
    /// dropped so it is rebuilt in the new language on the next mouse move.
    pub fn set_language(&mut self, lang: &str, cx: &mut Context<Self>) {
        if self.lang != lang {
            self.lang = lang.to_string();
            self.hover = None;
            cx.notify();
        }
    }

    /// The full render options for the current view state.
    fn render_opts(&self) -> RenderOptions {
        RenderOptions {
            highlight: self.highlight,
            highlight_category: self.highlight_category,
            highlight_owner: self.highlight_owner,
            root_owner: fastdir_core::model::NO_OWNER,
            show_free: self.show_free,
            show_unknown: self.show_unknown,
            use_disk: self.use_disk_size,
            palette: self.palette,
            color_mode: self.color_mode,
            cushion: self.cushion,
            cushion_strength: self.cushion_strength,
            borders: self.borders,
            min_cell: self.min_cell,
            now: RenderOptions::default().now,
        }
    }

    /// The tree is still growing (live scan), so render at most about once a
    /// second. Turning it off lets the next render through immediately.
    pub fn set_partial(&mut self, partial: bool, cx: &mut Context<Self>) {
        if self.partial != partial {
            self.partial = partial;
            if !partial {
                self.last_render = None;
            }
            cx.notify();
        }
    }

    pub fn set_tree(&mut self, tree: Option<Arc<RwLock<Tree>>>, cx: &mut Context<Self>) {
        self.tree = tree;
        self.selection.clear();
        self.highlight = None;
        self.highlight_category = None;
        self.highlight_owner = None;
        self.zoom.clear();
        self.zoom_fs = None;
        self.zoom_gen += 1;
        // A comparison points into the old tree; the owner sets a new one if
        // the replacement is part of starting a comparison.
        self.change = None;
        self.change_gen += 1;
        self.hover = None;
        if let Some(img) = self.image.take() {
            self.stale.borrow_mut().push(img);
        }
        self.image_key = None;
        self.pending = None;
        self.age_totals = None;
        cx.notify();
    }

    /// Set or clear the comparison used by [`ColorMode::Change`]. A different
    /// diff (or clearing it) invalidates the rendered image.
    pub fn set_change(&mut self, change: Option<Arc<ScanDiff>>, cx: &mut Context<Self>) {
        let same = match (&self.change, &change) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.change = change;
            self.change_gen += 1;
            self.age_totals = None;
            cx.notify();
        }
    }

    pub fn set_selection(&mut self, selection: Vec<NodePath>, cx: &mut Context<Self>) {
        if self.selection != selection {
            self.selection = selection;
            cx.notify();
        }
    }

    /// Highlight the leaves of one extension rank; `None` shows every leaf in
    /// full colour. Clears a category highlight.
    pub fn set_highlight(&mut self, highlight: Option<u16>, cx: &mut Context<Self>) {
        if self.highlight != highlight
            || self.highlight_category.is_some()
            || self.highlight_owner.is_some()
        {
            self.highlight = highlight;
            self.highlight_category = None;
            self.highlight_owner = None;
            cx.notify();
        }
    }

    /// Highlight every leaf in one file-type category (index into
    /// `FileCategory::ALL`); `None` clears it. Clears an extension highlight.
    pub fn set_highlight_category(&mut self, highlight: Option<u8>, cx: &mut Context<Self>) {
        if self.highlight_category != highlight
            || self.highlight.is_some()
            || self.highlight_owner.is_some()
        {
            self.highlight_category = highlight;
            self.highlight = None;
            self.highlight_owner = None;
            cx.notify();
        }
    }

    /// Highlight every file owned by one account; `None` clears it. Clears an
    /// extension and category highlight.
    pub fn set_highlight_owner(&mut self, owner: Option<u16>, cx: &mut Context<Self>) {
        if self.highlight_owner != owner
            || self.highlight.is_some()
            || self.highlight_category.is_some()
        {
            self.highlight_owner = owner;
            self.highlight = None;
            self.highlight_category = None;
            cx.notify();
        }
    }

    /// The tree changed: drop the hover label (paths may have shifted) and
    /// find the zoom folder again, falling back to its closest surviving
    /// ancestor if it was deleted or moved away.
    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.hover = None;
        if let (Some(tree), Some(fs)) = (self.tree.clone(), self.zoom_fs.clone()) {
            let t = tree.read();
            let found = fs.ancestors().find_map(|p| {
                t.find(p)
                    .filter(|path| t.get(path).is_some_and(Node::is_dir))
                    .map(|path| (path, p))
            });
            let (path, p) = found.unwrap_or_else(|| (Vec::new(), t.root_path.as_path()));
            let p = p.to_path_buf();
            drop(t);
            if path != self.zoom {
                self.zoom = path;
                self.zoom_gen += 1;
                self.age_totals = None;
            }
            self.zoom_fs = Some(p);
        }
        cx.notify();
    }

    /// `old` was renamed or moved to `new` (the tree is already updated). If
    /// the zoom folder is `old` or inside it, follow it to its new location,
    /// unless it left the scanned tree. Call before [`Self::invalidate`].
    pub fn path_moved(&mut self, old: &std::path::Path, new: &std::path::Path) {
        let in_tree = self
            .tree
            .as_ref()
            .is_some_and(|t| t.read().find(new).is_some());
        if !in_tree {
            return;
        }
        if let Some(rest) = self
            .zoom_fs
            .as_deref()
            .and_then(|z| z.strip_prefix(old).ok())
        {
            self.zoom_fs = Some(new.join(rest));
        }
    }

    /// The folder the treemap is rooted at.
    pub fn zoom(&self) -> &NodePath {
        &self.zoom
    }

    /// Root the treemap at the folder `path`. Files zoom to their folder.
    pub fn set_zoom(&mut self, mut path: NodePath, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let fs = {
            let t = tree.read();
            while !path.is_empty() && !t.get(&path).is_some_and(Node::is_dir) {
                path.pop();
            }
            t.fs_path(&path)
        };
        if path == self.zoom {
            return;
        }
        self.zoom = path;
        self.zoom_fs = fs;
        self.zoom_gen += 1;
        self.hover = None;
        self.age_totals = None;
        cx.notify();
    }

    /// Go up one folder. Returns false when already at the scan root.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) -> bool {
        if self.zoom.is_empty() {
            return false;
        }
        let mut path = self.zoom.clone();
        path.pop();
        self.set_zoom(path, cx);
        true
    }

    fn request(&mut self, key: Key, cx: &mut Context<Self>) {
        if self.pending == Some(key) || self.image_key == Some(key) {
            return;
        }
        if self.partial
            && self
                .last_render
                .is_some_and(|last| last.elapsed() < PARTIAL_REFRESH)
        {
            // A scan is running: the UI ticker notifies again in a moment.
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        self.pending = Some(key);
        self.rendering = true;
        self.last_render = Some(Instant::now());
        let background = if key.dark {
            [24, 24, 27]
        } else {
            [235, 235, 235]
        };
        let first = self.image.is_none();
        let zoom = self.zoom.clone();
        let change = self.change.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            // Coalesce bursts of resizes into one render.
            if !first {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
            }
            let mut opts = key.opts();
            let (buf, totals) = cx
                .background_executor()
                .spawn(async move {
                    let t = tree.read();
                    let root = t.get(&zoom).unwrap_or(t.root());
                    opts.root_owner = t.owner_at(&zoom);
                    let buf = treemap::render_with_change(
                        &t,
                        root,
                        key.w,
                        key.h,
                        background,
                        &opts,
                        change.as_deref(),
                    );
                    let totals = (opts.color_mode == ColorMode::Age)
                        .then(|| treemap::age_bucket_totals(&t, root, opts.now, opts.use_disk));
                    (buf, totals)
                })
                .await;
            let Some(img) = RgbaImage::from_raw(key.w, key.h, buf) else {
                return;
            };
            let image = Arc::new(RenderImage::new(smallvec::smallvec![Frame::new(img)]));
            this.update(cx, |this, cx| {
                if this.pending != Some(key) {
                    return;
                }
                if let Some(old) = this.image.replace(image) {
                    this.stale.borrow_mut().push(old);
                }
                this.image_key = Some(key);
                this.pending = None;
                this.age_totals = totals;
                this.rendering = false;
                cx.notify();
            })
            .ok();
        }));
    }

    fn device_point(&self, position: Point<Pixels>) -> (f64, f64, f64, f64) {
        let b = self.bounds.get();
        let s = self.scale.get() as f64;
        let w = (f32::from(b.size.width) as f64 * s).round();
        let h = (f32::from(b.size.height) as f64 * s).round();
        let x = (f32::from(position.x - b.origin.x) as f64) * s;
        let y = (f32::from(position.y - b.origin.y) as f64) * s;
        (w, h, x, y)
    }

    fn hit(&self, position: Point<Pixels>) -> Option<NodePath> {
        let tree = self.tree.as_ref()?.read();
        let (w, h, x, y) = self.device_point(position);
        let root = tree.get(&self.zoom)?;
        let rel = treemap::hit_test(&tree, root, w, h, x, y, &self.render_opts())?;
        let mut path = self.zoom.clone();
        path.extend(rel);
        Some(path)
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.hit(ev.position) {
            // The owner decides what the click does to the selection and
            // pushes the result back through `set_selection`.
            let toggle = ev.button == MouseButton::Left && is_toggle_click(&ev.modifiers);
            if !toggle && ev.button == MouseButton::Left && ev.click_count >= 2 {
                cx.emit(TreemapEvent::Activate(path));
            } else {
                cx.emit(TreemapEvent::Select {
                    path,
                    toggle,
                    context: ev.button == MouseButton::Right,
                });
            }
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.hit(ev.position);
        if path.as_ref() == self.hover.as_ref().map(|h| &h.0) {
            return;
        }
        self.hover = path.and_then(|p| {
            let tree = self.tree.as_ref()?.read();
            let node = tree.get(&p)?;
            let full = tree.fs_path(&p)?;
            let size = if self.use_disk_size {
                node.disk_size
            } else {
                node.size
            };
            let link = if node.hard_link() {
                fastdir_locale::t(&self.lang, "tree.hover_link")
            } else {
                ""
            };
            let label = {
                // A trailing pseudo-item name is translated (`C:\<Free space>`).
                let raw = full.display().to_string();
                let tail = full.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let mut text = raw;
                if let Some(local) = fastdir_locale::special_display(tail, &self.lang) {
                    text.truncate(text.len() - tail.len());
                    text.push_str(local);
                }
                let mut label = fastdir_locale::fmt(
                    &self.lang,
                    "tree.hover",
                    &[
                        ("path", &text),
                        ("size", &format::bytes_with(size, self.decimal_sizes)),
                        ("link", link),
                    ],
                );
                let age = format::date_with_in(node.modified(), true, &self.lang);
                if !age.is_empty() {
                    label.push_str("  ·  ");
                    label.push_str(&fastdir_locale::fmt(
                        &self.lang,
                        "tree.hover_age",
                        &[("age", &age)],
                    ));
                }
                label
            };
            Some((p, label.into()))
        });
        cx.notify();
    }
}

/// Assign when different; reports whether anything changed.
fn set_if_diff<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

/// Locale key for an age bucket's legend label.
fn age_bucket_key(bucket: AgeBucket) -> &'static str {
    match bucket {
        AgeBucket::Week => "age.under_week",
        AgeBucket::Month => "age.under_month",
        AgeBucket::SixMonths => "age.under_6_months",
        AgeBucket::Year => "age.under_year",
        AgeBucket::ThreeYears => "age.under_3_years",
        AgeBucket::Older => "age.older",
        AgeBucket::Unknown => "age.unknown",
    }
}

impl Render for TreemapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        let generation = self.tree.as_ref().map(|t| t.read().generation).unwrap_or(0);
        let image = self.image.clone();
        let stale = self.stale.clone();
        let bounds_cell = self.bounds.clone();
        let scale_cell = self.scale.clone();
        let entity = cx.entity().downgrade();
        let tree = self.tree.clone();
        let selection = self.selection.clone();
        let pending = self.pending;
        let image_key = self.image_key;
        let highlight = self.highlight;
        let highlight_category = self.highlight_category;
        let highlight_owner = self.highlight_owner;
        let zoom = self.zoom_gen;
        let change_gen = self.change_gen;
        let zoom_path = self.zoom.clone();
        let has_tree = self.tree.is_some();
        let render_opts = self.render_opts();
        let paint_opts = render_opts;
        let _ = window;

        let canvas = canvas(
            move |bounds, window, cx| {
                bounds_cell.set(bounds);
                let scale = window.scale_factor();
                scale_cell.set(scale);
                let w = (f32::from(bounds.size.width) * scale).round() as u32;
                let h = (f32::from(bounds.size.height) * scale).round() as u32;
                if has_tree && w >= 4 && h >= 4 {
                    let key = Key::new(
                        generation,
                        w,
                        h,
                        dark,
                        highlight,
                        highlight_category,
                        highlight_owner,
                        zoom,
                        change_gen,
                        render_opts,
                    );
                    if image_key != Some(key) && pending != Some(key) {
                        cx.defer(move |cx| {
                            entity.update(cx, |this, cx| this.request(key, cx)).ok();
                        });
                    }
                }
            },
            move |bounds, (), window, cx| {
                for img in stale.borrow_mut().drain(..) {
                    window.drop_image(img).ok();
                }
                if let Some(img) = image {
                    window
                        .paint_image(bounds, bounds, Corners::default(), img, 0, false)
                        .ok();
                }
                let Some(tree) = tree else { return };
                let scale = window.scale_factor() as f64;
                let w = (f32::from(bounds.size.width) as f64 * scale).round();
                let h = (f32::from(bounds.size.height) as f64 * scale).round();
                let t = tree.read();
                let Some(root) = t.get(&zoom_path) else {
                    return;
                };
                for sel in &selection {
                    // Only things inside the zoomed folder are on screen.
                    let Some(rel) = sel.strip_prefix(zoom_path.as_slice()) else {
                        continue;
                    };
                    let Some(r) = treemap::rect_of(&t, root, w, h, rel, &paint_opts) else {
                        continue;
                    };
                    let origin = bounds.origin;
                    let rb = Bounds::new(
                        point(
                            origin.x + px((r.x0 / scale) as f32),
                            origin.y + px((r.y0 / scale) as f32),
                        ),
                        size(
                            px((r.w() / scale).max(2.0) as f32),
                            px((r.h() / scale).max(2.0) as f32),
                        ),
                    );
                    let _ = &cx;
                    window.paint_quad(quad(
                        rb,
                        px(0.),
                        transparent_black(),
                        px(2.),
                        if dark { white() } else { black() },
                        BorderStyle::Solid,
                    ));
                    if rb.size.width > px(6.) && rb.size.height > px(6.) {
                        window.paint_quad(quad(
                            rb.dilate(px(-2.)),
                            px(0.),
                            transparent_black(),
                            px(1.),
                            if dark {
                                black().opacity(0.6)
                            } else {
                                white().opacity(0.8)
                            },
                            BorderStyle::Solid,
                        ));
                    }
                }
            },
        )
        .size_full();

        let border = theme.border;
        let muted = theme.muted_foreground;
        let popover = theme.popover;
        let fg = theme.popover_foreground;
        let breadcrumb = has_tree.then(|| self.render_breadcrumb(cx));
        let legend = self
            .render_age_legend(cx)
            .or_else(|| self.render_change_legend(cx));

        let body = div()
            .id("treemap")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(if dark { rgb(0x18181b) } else { rgb(0xebebeb) })
            .child(canvas)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.hover.take().is_some() {
                    cx.notify();
                }
            }))
            .when(!has_tree, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(muted)
                        .child(fastdir_locale::t(&self.lang, "tree.empty")),
                )
            })
            .when(self.rendering && self.image.is_none() && has_tree, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(muted)
                        .child(fastdir_locale::t(&self.lang, "tree.rendering")),
                )
            })
            .when_some(self.hover.as_ref(), |this, (_, label)| {
                this.child(
                    h_flex()
                        .absolute()
                        .left_2()
                        .bottom_2()
                        .max_w(relative(0.9))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .bg(popover.opacity(0.92))
                        .text_color(fg)
                        .text_xs()
                        .shadow_md()
                        .overflow_hidden()
                        .child(label.clone()),
                )
            });

        v_flex()
            .size_full()
            .children(breadcrumb)
            .child(body)
            .children(legend)
    }
}

impl TreemapView {
    /// Bytes in each age bucket under the zoomed folder, shown while
    /// [`ColorMode::Age`] is active.
    fn render_age_legend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.color_mode != ColorMode::Age {
            return None;
        }
        let totals = self.age_totals?;
        let theme = cx.theme();
        Some(
            h_flex()
                .w_full()
                .flex_shrink_0()
                .flex_wrap()
                .gap_3()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_xs()
                .text_color(theme.muted_foreground)
                .children(
                    totals
                        .into_iter()
                        .filter(|(bucket, bytes)| *bucket != AgeBucket::Unknown || *bytes > 0)
                        .map(|(bucket, bytes)| {
                            let (r, g, b) = treemap::age_bucket_color(bucket, self.palette);
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .size(px(9.))
                                        .rounded_sm()
                                        .bg(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)),
                                )
                                .child(div().child(format!(
                                    "{}  {}",
                                    fastdir_locale::t(&self.lang, age_bucket_key(bucket)),
                                    format::bytes_with(bytes, self.decimal_sizes)
                                )))
                        }),
                )
                .into_any_element(),
        )
    }

    /// Diverging legend shown while [`ColorMode::Change`] is active and a
    /// comparison is loaded.
    fn render_change_legend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.color_mode != ColorMode::Change {
            return None;
        }
        let change = self.change.as_ref()?;
        let totals = change.totals();
        let theme = cx.theme();
        let swatch = |color: (u8, u8, u8), key: &'static str| {
            let (r, g, b) = color;
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .size(px(9.))
                        .rounded_sm()
                        .bg(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)),
                )
                .child(div().child(fastdir_locale::t(&self.lang, key)))
        };
        let net = totals.net();
        let sign = if net > 0 {
            "+"
        } else if net < 0 {
            "-"
        } else {
            ""
        };
        let net_text = format!(
            "{sign}{}",
            format::bytes_with(net.unsigned_abs(), self.decimal_sizes)
        );
        Some(
            h_flex()
                .w_full()
                .flex_shrink_0()
                .flex_wrap()
                .gap_3()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(swatch(treemap::CHANGE_SHRANK_COLOR, "change.legend_shrank"))
                .child(swatch(treemap::CHANGE_NEUTRAL_COLOR, "change.legend_same"))
                .child(swatch(treemap::CHANGE_GREW_COLOR, "change.legend_grew"))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(fastdir_locale::fmt(
                            &self.lang,
                            "change.legend_net",
                            &[("delta", &net_text)],
                        )),
                )
                .into_any_element(),
        )
    }

    /// `C:\ > Users > me > AppData`, one clickable part per folder.
    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let mut parts: Vec<(SharedString, NodePath)> = Vec::new();
        if let Some(tree) = &self.tree {
            let t = tree.read();
            parts.push((t.root_path.display().to_string().into(), Vec::new()));
            let mut node = t.root();
            for (depth, &i) in self.zoom.iter().enumerate() {
                let Some(child) = t.children(node).get(i as usize) else {
                    break;
                };
                let name = fastdir_locale::special_display(t.name(child), &self.lang)
                    .unwrap_or_else(|| t.name(child));
                parts.push((name.to_string().into(), self.zoom[..=depth].to_vec()));
                node = child;
            }
        }
        let last = parts.len().saturating_sub(1);
        let (fg, muted, hover_bg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.secondary_hover,
        );
        let zoomed = !self.zoom.is_empty();

        h_flex()
            .h(rems(26. / 16.))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .gap_2()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .gap_0p5()
                    .children(parts.into_iter().enumerate().map(|(ix, (label, path))| {
                        let current = ix == last;
                        h_flex()
                            .flex_shrink_0()
                            .gap_0p5()
                            .when(ix > 0, |this| {
                                this.child(
                                    Icon::new(IconName::ChevronRight).xsmall().text_color(muted),
                                )
                            })
                            .child(
                                div()
                                    .id(("crumb", ix))
                                    .px_1()
                                    .rounded_sm()
                                    .text_color(if current { fg } else { muted })
                                    .when(current, |this| this.font_weight(FontWeight::SEMIBOLD))
                                    .when(!current, |this| {
                                        this.cursor_pointer()
                                            .hover(move |s| s.bg(hover_bg).text_color(fg))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.set_zoom(path.clone(), cx);
                                            }))
                                    })
                                    .child(label),
                            )
                    })),
            )
            .child(div().flex_shrink_0().text_color(muted).child(if zoomed {
                fastdir_locale::t(&self.lang, "tree.zoom_hint")
            } else {
                fastdir_locale::t(&self.lang, "tree.zoom_tip")
            }))
    }
}

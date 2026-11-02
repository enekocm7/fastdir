//! The treemap panel: shows the cushion image rendered off the main thread
//! and turns clicks into selections. It can be zoomed into any folder; a
//! breadcrumb above the image leads back up.

use fastdir_core::format;
use fastdir_core::model::{Node, NodePath, Tree};
use fastdir_treemap as treemap;
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
    /// The user clicked a cell. `toggle` is set for Ctrl+click (add to or
    /// remove from the selection), `context` for a right click.
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
    /// Bumped whenever the zoom root changes.
    zoom: u64,
    show_free: bool,
    show_unknown: bool,
    use_disk_size: bool,
}

pub struct TreemapView {
    tree: Option<Arc<RwLock<Tree>>>,
    image: Option<Arc<RenderImage>>,
    image_key: Option<Key>,
    pending: Option<Key>,
    stale: Rc<std::cell::RefCell<Vec<Arc<RenderImage>>>>,
    /// Every selected node; each one gets an outline.
    selection: Vec<NodePath>,
    highlight: Option<u16>,
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
            stale: Default::default(),
            selection: Vec::new(),
            highlight: None,
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
            partial: false,
            last_render: None,
            _task: None,
        }
    }

    pub fn with_options(show_free: bool, show_unknown: bool, use_disk_size: bool) -> Self {
        let mut v = Self::new();
        v.show_free = show_free;
        v.show_unknown = show_unknown;
        v.use_disk_size = use_disk_size;
        v
    }

    pub fn set_show_free(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_free != show {
            self.show_free = show;
            cx.notify();
        }
    }

    pub fn set_show_unknown(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_unknown != show {
            self.show_unknown = show;
            cx.notify();
        }
    }

    /// Size cells by allocated bytes (`true`) or logical bytes (`false`).
    pub fn set_use_disk_size(&mut self, use_disk: bool, cx: &mut Context<Self>) {
        if self.use_disk_size != use_disk {
            self.use_disk_size = use_disk;
            cx.notify();
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
        self.zoom.clear();
        self.zoom_fs = None;
        self.zoom_gen += 1;
        self.hover = None;
        if let Some(img) = self.image.take() {
            self.stale.borrow_mut().push(img);
        }
        self.image_key = None;
        self.pending = None;
        cx.notify();
    }

    pub fn set_selection(&mut self, selection: Vec<NodePath>, cx: &mut Context<Self>) {
        if self.selection != selection {
            self.selection = selection;
            cx.notify();
        }
    }

    /// Highlight the leaves of one extension rank; `None` shows every leaf in
    /// full colour.
    pub fn set_highlight(&mut self, highlight: Option<u16>, cx: &mut Context<Self>) {
        if self.highlight != highlight {
            self.highlight = highlight;
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
        self._task = Some(cx.spawn(async move |this, cx| {
            // Coalesce bursts of resizes into one render.
            if !first {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
            }
            let buf = cx
                .background_executor()
                .spawn(async move {
                    let t = tree.read();
                    let root = t.get(&zoom).unwrap_or(t.root());
                    treemap::render(
                        &t,
                        root,
                        key.w,
                        key.h,
                        background,
                        key.highlight,
                        key.show_free,
                        key.show_unknown,
                        key.use_disk_size,
                    )
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
        let rel = treemap::hit_test(
            &tree,
            root,
            w,
            h,
            x,
            y,
            self.show_free,
            self.show_unknown,
            self.use_disk_size,
        )?;
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
                "  ·  hard link"
            } else {
                ""
            };
            let label = format!("{}  —  {}{link}", full.display(), format::bytes(size));
            Some((p, label.into()))
        });
        cx.notify();
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
        let zoom = self.zoom_gen;
        let zoom_path = self.zoom.clone();
        let has_tree = self.tree.is_some();
        let show_free = self.show_free;
        let show_unknown = self.show_unknown;
        let use_disk_size = self.use_disk_size;
        let _ = window;

        let canvas = canvas(
            move |bounds, window, cx| {
                bounds_cell.set(bounds);
                let scale = window.scale_factor();
                scale_cell.set(scale);
                let w = (f32::from(bounds.size.width) * scale).round() as u32;
                let h = (f32::from(bounds.size.height) * scale).round() as u32;
                if has_tree && w >= 4 && h >= 4 {
                    let key = Key {
                        generation,
                        w,
                        h,
                        dark,
                        highlight,
                        zoom,
                        show_free,
                        show_unknown,
                        use_disk_size,
                    };
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
                    let Some(r) = treemap::rect_of(
                        &t,
                        root,
                        w,
                        h,
                        rel,
                        show_free,
                        show_unknown,
                        use_disk_size,
                    ) else {
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
                        .child("Treemap appears here after a scan"),
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
                        .child("Rendering treemap…"),
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

        v_flex().size_full().children(breadcrumb).child(body)
    }
}

impl TreemapView {
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
                parts.push((
                    t.name(child).to_string().into(),
                    self.zoom[..=depth].to_vec(),
                ));
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
            .h(px(26.))
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
                "Z zoom in · Shift+Z zoom out"
            } else {
                "Z to zoom into a folder"
            }))
    }
}

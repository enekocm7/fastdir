//! The treemap panel: shows the cushion image rendered off the main thread
//! and turns clicks into selections.

use crate::format;
use crate::model::{NodePath, Tree};
use crate::treemap;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use image::{Frame, RgbaImage};
use parking_lot::RwLock;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub enum TreemapEvent {
    /// The user clicked a cell (left or right button).
    Select(NodePath),
    /// Double-click: open the item.
    Activate(NodePath),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    generation: u64,
    w: u32,
    h: u32,
    dark: bool,
}

pub struct TreemapView {
    tree: Option<Arc<RwLock<Tree>>>,
    image: Option<Arc<RenderImage>>,
    image_key: Option<Key>,
    pending: Option<Key>,
    stale: Rc<std::cell::RefCell<Vec<Arc<RenderImage>>>>,
    selection: Option<NodePath>,
    hover: Option<(NodePath, SharedString)>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scale: Rc<Cell<f32>>,
    rendering: bool,
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
            selection: None,
            hover: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            scale: Rc::new(Cell::new(1.0)),
            rendering: false,
            _task: None,
        }
    }

    pub fn set_tree(&mut self, tree: Option<Arc<RwLock<Tree>>>, cx: &mut Context<Self>) {
        self.tree = tree;
        self.selection = None;
        self.hover = None;
        if let Some(img) = self.image.take() {
            self.stale.borrow_mut().push(img);
        }
        self.image_key = None;
        self.pending = None;
        cx.notify();
    }

    pub fn set_selection(&mut self, selection: Option<NodePath>, cx: &mut Context<Self>) {
        if self.selection != selection {
            self.selection = selection;
            cx.notify();
        }
    }

    /// The tree changed: drop the hover label (paths may have shifted).
    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.hover = None;
        cx.notify();
    }

    fn request(&mut self, key: Key, cx: &mut Context<Self>) {
        if self.pending == Some(key) || self.image_key == Some(key) {
            return;
        }
        let Some(tree) = self.tree.clone() else { return };
        self.pending = Some(key);
        self.rendering = true;
        let background = if key.dark { [24, 24, 27] } else { [235, 235, 235] };
        let first = self.image.is_none();
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
                    treemap::render(&t, key.w, key.h, background)
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
        treemap::hit_test(&tree, w, h, x, y)
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.hit(ev.position) {
            self.selection = Some(path.clone());
            if ev.button == MouseButton::Left && ev.click_count >= 2 {
                cx.emit(TreemapEvent::Activate(path));
            } else {
                cx.emit(TreemapEvent::Select(path));
            }
            cx.notify();
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
            let label = format!("{}  —  {}", full.display(), format::bytes(node.size));
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
        let has_tree = self.tree.is_some();
        let _ = window;

        let canvas = canvas(
            move |bounds, window, cx| {
                bounds_cell.set(bounds);
                let scale = window.scale_factor();
                scale_cell.set(scale);
                let w = (f32::from(bounds.size.width) * scale).round() as u32;
                let h = (f32::from(bounds.size.height) * scale).round() as u32;
                if has_tree && w >= 4 && h >= 4 {
                    let key = Key { generation, w, h, dark };
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
                let (Some(tree), Some(sel)) = (tree, selection) else { return };
                let scale = window.scale_factor() as f64;
                let w = (f32::from(bounds.size.width) as f64 * scale).round();
                let h = (f32::from(bounds.size.height) as f64 * scale).round();
                let rect = {
                    let t = tree.read();
                    treemap::rect_of(&t, w, h, &sel)
                };
                if let Some(r) = rect {
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
                    let _ = cx;
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
                            if dark { black().opacity(0.6) } else { white().opacity(0.8) },
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

        div()
            .id("treemap")
            .relative()
            .size_full()
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
            })
    }
}

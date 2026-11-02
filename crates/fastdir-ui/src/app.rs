//! Main window: drive picker, scan progress, directory list, extension list
//! and treemap, laid out like WinDirStat.

use crate::treemap_view::{TreemapEvent, TreemapView, is_toggle_click};
use fastdir_core::drives::{self, Drive};
use fastdir_core::format;
use fastdir_core::fsops;
use fastdir_core::model::{NO_EXT, Node, NodePath, Subtree, Tree};
use fastdir_core::scanner::{self, LiveScanResult, Progress};
use fastdir_settings::{Settings, SortColumn as SortKey, ThemePreference, WindowState};
use fastdir_treemap as treemap;
use fastdir_updater::{self as updater, Release};
use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parking_lot::RwLock;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

actions!(
    fastdir,
    [
        SelectPrev,
        SelectNext,
        ExtendPrev,
        ExtendNext,
        SelectFirst,
        SelectLast,
        PageUp,
        PageDown,
        ExpandItem,
        CollapseItem,
        ToggleItem,
        GoParent,
        OpenItem,
        RevealItem,
        CopyPath,
        RenameItem,
        MoveItem,
        TrashItem,
        DeleteItem,
        RefreshItem,
        Rescan,
        StopScan,
        OpenFolder,
        ToggleTheme,
        ZoomIn,
        ZoomOut,
        ToggleFreeSpace,
        ToggleUnknown,
        OpenSettings,
    ]
);

mod context_menu;
mod dialogs;
mod file_list;
mod scan_state;
mod selection;
mod settings_ui;
mod status;
mod toolbar;
mod update_ui;

const CONTEXT: &str = "FastDir";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("shift-up", ExtendPrev, Some(CONTEXT)),
        KeyBinding::new("shift-down", ExtendNext, Some(CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(CONTEXT)),
        KeyBinding::new("right", ExpandItem, Some(CONTEXT)),
        KeyBinding::new("left", CollapseItem, Some(CONTEXT)),
        KeyBinding::new("enter", ToggleItem, Some(CONTEXT)),
        KeyBinding::new("backspace", GoParent, Some(CONTEXT)),
        KeyBinding::new("ctrl-enter", OpenItem, Some(CONTEXT)),
        KeyBinding::new("ctrl-e", RevealItem, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-c", CopyPath, Some(CONTEXT)),
        KeyBinding::new("f2", RenameItem, Some(CONTEXT)),
        KeyBinding::new("ctrl-m", MoveItem, Some(CONTEXT)),
        KeyBinding::new("delete", TrashItem, Some(CONTEXT)),
        KeyBinding::new("shift-delete", DeleteItem, Some(CONTEXT)),
        KeyBinding::new("f5", RefreshItem, Some(CONTEXT)),
        KeyBinding::new("ctrl-f5", Rescan, Some(CONTEXT)),
        KeyBinding::new("ctrl-r", Rescan, Some(CONTEXT)),
        KeyBinding::new("escape", StopScan, Some(CONTEXT)),
        KeyBinding::new("ctrl-o", OpenFolder, Some(CONTEXT)),
        KeyBinding::new("ctrl-t", ToggleTheme, Some(CONTEXT)),
        KeyBinding::new("z", ZoomIn, Some(CONTEXT)),
        KeyBinding::new("shift-z", ZoomOut, Some(CONTEXT)),
        KeyBinding::new("f6", ToggleFreeSpace, Some(CONTEXT)),
        KeyBinding::new("f7", ToggleUnknown, Some(CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(CONTEXT)),
    ]);
}

const ROW_H: f32 = 22.;
const COL_BAR: f32 = 110.;
const COL_PCT: f32 = 62.;
const COL_SIZE: f32 = 92.;
const COL_DISK: f32 = 100.;
const COL_ITEMS: f32 = 82.;
const COL_FILES: f32 = 82.;
const COL_DIRS: f32 = 74.;
const COL_DATE: f32 = 128.;

/// Colours for the "subtree percentage" bars, one per depth (WinDirStat style).
const DEPTH_COLORS: [u32; 8] = [
    0x3b82f6, 0xef4444, 0x22c55e, 0xeab308, 0xa855f7, 0x06b6d4, 0xf97316, 0xec4899,
];

/// What a click does to the selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClickMode {
    /// Select just this item.
    Plain,
    /// Ctrl+click: add to or remove from the selection.
    Toggle,
    /// Shift+click: select everything from the anchor to here.
    Range,
    /// Right click: keep a multi-selection if the item is part of it.
    Context,
}

impl ClickMode {
    fn from_modifiers(m: &Modifiers) -> Self {
        if m.shift {
            Self::Range
        } else if is_toggle_click(m) {
            Self::Toggle
        } else {
            Self::Plain
        }
    }
}

/// One item of the (normalised) selection, ready for a file operation.
#[derive(Clone)]
struct Target {
    path: NodePath,
    fs: PathBuf,
    is_dir: bool,
    size: u64,
    /// Files and folders this item stands for, itself included.
    items: u64,
}

/// A delete or move running in the background, shown in the toolbar with a
/// progress bar and a cancel button.
struct FileOp {
    label: SharedString,
    progress: Arc<fsops::OpProgress>,
    total_items: u64,
    total_bytes: u64,
    /// Moves are measured in bytes copied, deletes in items removed.
    by_bytes: bool,
}

#[derive(Clone)]
struct Row {
    path: NodePath,
    depth: u16,
}

enum Phase {
    Home,
    Scanning {
        progress: Arc<Progress>,
        root: PathBuf,
        started: Instant,
        /// Used bytes of the drive, when scanning a whole drive (gives a real %).
        expected: Option<u64>,
    },
    Ready,
}

pub struct FastDir {
    focus: FocusHandle,
    drives: Vec<Drive>,
    phase: Phase,
    tree: Option<Arc<RwLock<Tree>>>,
    scan_root: Option<PathBuf>,
    last_scan: Option<(Duration, bool)>,
    /// Tree generation the visible rows were built from during a live scan.
    partial_generation: u64,
    rows: Vec<Row>,
    /// Extension ids with files left, in rank order.
    ext_rows: Vec<u16>,
    /// The cursor row. With a multi-selection it is the item clicked last.
    selected: Option<usize>,
    /// Multi-selection (Ctrl/Shift+click). Empty means "just the cursor row".
    /// Never contains the scan root. Cleared whenever the tree changes, since
    /// node paths shift then.
    marked: BTreeSet<NodePath>,
    /// Where a Shift+click range starts.
    anchor: Option<NodePath>,
    /// Extension rank highlighted in the treemap, picked from the ext list.
    selected_ext: Option<u16>,
    sort: (SortKey, bool),
    list_scroll: UniformListScrollHandle,
    ext_scroll: UniformListScrollHandle,
    treemap: Entity<TreemapView>,
    /// Divider state for the top/treemap split and the list/extensions split.
    main_split: Entity<ResizableState>,
    top_split: Entity<ResizableState>,
    busy: Option<SharedString>,
    op: Option<FileOp>,
    settings: Settings,
    /// A newer release the user can install.
    available: Option<Arc<Release>>,
    checking: bool,
    updating: bool,
    _subscriptions: Vec<Subscription>,
    /// Follows Windows light / dark changes for the "System" theme.
    _appearance_sub: Option<Subscription>,
    _rename_sub: Option<Subscription>,
    _scan_task: Option<Task<()>>,
    _ticker: Option<Task<()>>,
    _update_ticker: Option<Task<()>>,
    _op_ticker: Option<Task<()>>,
}

impl FastDir {
    pub fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (show_free, show_unknown) = (settings.show_free_space, settings.show_unknown);
        let use_disk_size = settings.treemap_disk_size;
        let sort = (settings.sort.column, settings.sort.descending);
        let treemap = cx.new(|_| TreemapView::with_options(show_free, show_unknown, use_disk_size));
        let main_split = cx.new(|_| ResizableState::default());
        let top_split = cx.new(|_| ResizableState::default());
        let sub = cx.subscribe_in(
            &treemap,
            window,
            |this, _, ev: &TreemapEvent, window, cx| match ev {
                TreemapEvent::Select {
                    path,
                    toggle,
                    context,
                } => {
                    let mode = if *toggle {
                        ClickMode::Toggle
                    } else if *context {
                        ClickMode::Context
                    } else {
                        ClickMode::Plain
                    };
                    this.reveal_in_list(path.clone(), mode, cx);
                    this.focus.focus(window, cx);
                }
                TreemapEvent::Activate(path) => {
                    this.reveal_in_list(path.clone(), ClickMode::Plain, cx);
                    this.open_selected(&OpenItem, window, cx);
                }
            },
        );
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        // `fastdir <path>` starts scanning right away.
        if let Some(arg) = std::env::args_os().nth(1) {
            // Explorer passes drives as `"C:\"`, which arrives as `C:"`.
            let mut arg = arg.to_string_lossy().trim().trim_matches('"').to_string();
            if arg.ends_with(':') {
                arg.push(std::path::MAIN_SEPARATOR);
            }
            let path = PathBuf::from(arg);
            if path.is_dir() {
                cx.defer_in(window, move |this: &mut Self, window, cx| {
                    this.start_scan(path, window, cx)
                });
            }
        }
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.startup_update_tasks(window, cx)
        });
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.install_window_handlers(window, cx)
        });
        Self {
            focus,
            drives: drives::list(),
            phase: Phase::Home,
            tree: None,
            scan_root: None,
            last_scan: None,
            partial_generation: 0,
            rows: Vec::new(),
            ext_rows: Vec::new(),
            selected: None,
            marked: BTreeSet::new(),
            anchor: None,
            selected_ext: None,
            sort,
            list_scroll: UniformListScrollHandle::new(),
            ext_scroll: UniformListScrollHandle::new(),
            treemap,
            main_split,
            top_split,
            busy: None,
            op: None,
            settings,
            available: None,
            checking: false,
            updating: false,
            _subscriptions: vec![sub],
            _appearance_sub: None,
            _rename_sub: None,
            _scan_task: None,
            _ticker: None,
            _update_ticker: None,
            _op_ticker: None,
        }
    }

    /// Whether the cursor is on a drive pseudo-item (`<Free space>`/`<Unknown>`).
    pub(super) fn selected_is_special(&self) -> bool {
        let (Some(tree), Some(path)) = (&self.tree, self.selected_path()) else {
            return false;
        };
        tree.read().get(&path).is_some_and(|n| n.is_special())
    }

    pub(super) fn toggle_free_space(
        &mut self,
        _: &ToggleFreeSpace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.show_free_space = !self.settings.show_free_space;
        self.settings.save();
        let show = self.settings.show_free_space;
        self.treemap.update(cx, |t, cx| t.set_show_free(show, cx));
        cx.notify();
    }

    pub(super) fn toggle_unknown(
        &mut self,
        _: &ToggleUnknown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.show_unknown = !self.settings.show_unknown;
        self.settings.save();
        let show = self.settings.show_unknown;
        self.treemap
            .update(cx, |t, cx| t.set_show_unknown(show, cx));
        cx.notify();
    }

    /// Show or hide finished folders while a scan is still running.
    pub(super) fn toggle_partial_results(&mut self, cx: &mut Context<Self>) {
        self.settings.show_partial_results = !self.settings.show_partial_results;
        self.settings.save();
        let show = self.settings.show_partial_results;
        let scanning = matches!(self.phase, Phase::Scanning { .. });
        if scanning {
            // The tree has been filling in all along; make the view catch up
            // (or stop refreshing the preview) right away.
            if show {
                self.partial_generation = 0;
                self.rebuild_rows();
            }
            self.treemap.update(cx, |t, cx| {
                t.set_partial(show, cx);
                t.invalidate(cx);
            });
        }
        cx.notify();
    }

    /// Switch the treemap between logical size and size on disk.
    pub(super) fn toggle_treemap_disk_size(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.treemap_disk_size = !self.settings.treemap_disk_size;
        self.settings.save();
        let use_disk = self.settings.treemap_disk_size;
        self.treemap
            .update(cx, |t, cx| t.set_use_disk_size(use_disk, cx));
        cx.notify();
    }

    pub(super) fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let main_height = panel_px(self.settings.panels.main.first().copied(), 420.);
        let ext_width = panel_px(self.settings.panels.top.get(1).copied(), 330.);

        v_resizable("main-split")
            .with_state(&self.main_split)
            .on_resize({
                let entity = entity.clone();
                move |state, _, cx| {
                    let sizes = state.read(cx).sizes().iter().map(|s| s.as_f32()).collect();
                    entity
                        .update(cx, |this, _| this.remember_split(Split::Main, sizes))
                        .ok();
                }
            })
            .child(
                resizable_panel()
                    .size(main_height)
                    .size_range(px(120.)..Pixels::MAX)
                    .child(
                        h_resizable("top-split")
                            .with_state(&self.top_split)
                            .on_resize({
                                let entity = entity.clone();
                                move |state, _, cx| {
                                    let sizes =
                                        state.read(cx).sizes().iter().map(|s| s.as_f32()).collect();
                                    entity
                                        .update(cx, |this, _| {
                                            this.remember_split(Split::Top, sizes)
                                        })
                                        .ok();
                                }
                            })
                            .child(
                                resizable_panel()
                                    .size_range(px(400.)..Pixels::MAX)
                                    .child(self.render_list(cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size(ext_width)
                                    .size_range(px(200.)..px(700.))
                                    .child(self.render_ext_list(cx)),
                            ),
                    ),
            )
            .child(resizable_panel().size_range(px(80.)..Pixels::MAX).child({
                let entity = cx.entity();
                div()
                    .id("treemap-panel")
                    .size_full()
                    .child(self.treemap.clone())
                    .context_menu(move |menu, _, cx| entity.read(cx).build_context_menu(menu, cx))
            }))
    }

    /// Remember where the user dragged a divider.
    fn remember_split(&mut self, split: Split, sizes: Vec<f32>) {
        let sizes: Vec<f32> = sizes
            .into_iter()
            .filter(|s| s.is_finite() && *s > 0.)
            .collect();
        match split {
            Split::Main => self.settings.panels.main = sizes,
            Split::Top => self.settings.panels.top = sizes,
        }
        self.settings.save();
    }

    /// Persist window position, size and maximized state.
    fn remember_window(&mut self, window: &Window) {
        let bounds = window.window_bounds();
        let inner = bounds.get_bounds();
        self.settings.window = Some(WindowState {
            x: f32::from(inner.origin.x),
            y: f32::from(inner.origin.y),
            width: f32::from(inner.size.width),
            height: f32::from(inner.size.height),
            maximized: matches!(bounds, WindowBounds::Maximized(_)),
        });
        self.settings.save();
    }

    /// Install the callbacks that have to run after the window exists: save
    /// the geometry on close and follow Windows light / dark changes.
    pub(crate) fn install_window_handlers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |this, _| this.remember_window(window))
                .ok();
            true
        });

        let entity = cx.entity().downgrade();
        self._appearance_sub = Some(window.observe_window_appearance(move |window, cx| {
            let entity = entity.clone();
            // The observer fires once on activation, while this entity may
            // still be mid-update; defer so the update is never re-entrant.
            window.defer(cx, move |window, cx| {
                entity
                    .update(cx, |this, cx| {
                        if this.settings.theme == ThemePreference::System {
                            this.apply_theme(window, cx);
                        }
                    })
                    .ok();
            });
        }));
    }

    pub(super) fn set_theme_preference(
        &mut self,
        theme: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.theme = theme;
        self.settings.save();
        self.apply_theme(window, cx);
        cx.notify();
    }

    /// Apply the saved preference; `System` reads the current Windows mode.
    pub(super) fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = match self.settings.theme {
            ThemePreference::Light => ThemeMode::Light,
            ThemePreference::Dark => ThemeMode::Dark,
            ThemePreference::System => ThemeMode::from(window.appearance()),
        };
        Theme::change(mode, Some(window), cx);
        apply_accent(cx);
        window.refresh();
    }
}

/// The two resizable splits whose sizes are persisted.
#[derive(Clone, Copy)]
enum Split {
    Main,
    Top,
}

/// A saved panel size, ignoring nonsense left by a broken config.
fn panel_px(saved: Option<f32>, fallback: f32) -> Pixels {
    saved
        .filter(|v| v.is_finite() && *v > 0.)
        .map(px)
        .unwrap_or_else(|| px(fallback))
}

/// The window bounds to open with, falling back to a centered default when
/// nothing was saved or the saved position is no longer on a display.
pub(crate) fn restored_window_bounds(settings: &Settings, cx: &App) -> WindowBounds {
    let centered = || WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(920.)), cx));
    let Some(saved) = settings.window else {
        return centered();
    };
    let valid = [saved.x, saved.y, saved.width, saved.height]
        .into_iter()
        .all(f32::is_finite)
        && saved.width >= 900.
        && saved.height >= 600.;
    if !valid {
        return centered();
    }
    let bounds = Bounds::new(
        point(px(saved.x), px(saved.y)),
        size(px(saved.width), px(saved.height)),
    );
    let on_screen = cx
        .displays()
        .iter()
        .any(|display| display.visible_bounds().intersects(&bounds));
    if !on_screen {
        return centered();
    }
    if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

use std::rc::Rc;

impl Render for FastDir {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let live = matches!(self.phase, Phase::Scanning { .. })
            && self.settings.show_partial_results
            && self.tree.is_some();
        let body = match self.phase {
            Phase::Home => self.render_home(cx).into_any_element(),
            Phase::Scanning { .. } if live => self.render_ready(cx).into_any_element(),
            Phase::Scanning { .. } => self.render_scanning(cx).into_any_element(),
            Phase::Ready => self.render_ready(cx).into_any_element(),
        };
        let theme = cx.theme();
        v_flex()
            .id("fastdir")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::extend_prev))
            .on_action(cx.listener(Self::extend_next))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::expand_item))
            .on_action(cx.listener(Self::collapse_item))
            .on_action(cx.listener(Self::toggle_item))
            .on_action(cx.listener(Self::go_parent))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::reveal_selected))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::rename_item))
            .on_action(cx.listener(Self::move_item))
            .on_action(cx.listener(Self::trash_item))
            .on_action(cx.listener(Self::delete_item))
            .on_action(cx.listener(Self::refresh_item))
            .on_action(cx.listener(Self::rescan))
            .on_action(cx.listener(Self::stop_scan))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::toggle_free_space))
            .on_action(cx.listener(Self::toggle_unknown))
            .on_action(cx.listener(Self::open_settings))
            .child(self.render_title_bar(cx))
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().w_full().child(body))
            .child(self.render_status(cx))
    }
}

/// Blue accent on top of the default theme (its dark primary is plain white).
pub fn apply_accent(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let dark = theme.mode.is_dark();
    let blue: Hsla = rgb(if dark { 0x3b82f6 } else { 0x2563eb }).into();
    let hover: Hsla = rgb(if dark { 0x60a5fa } else { 0x1d4ed8 }).into();
    let active: Hsla = rgb(if dark { 0x2563eb } else { 0x1e40af }).into();
    let c = &mut theme.colors;
    c.primary = blue;
    c.primary_hover = hover;
    c.primary_active = active;
    c.primary_foreground = white();
    c.button_primary = blue;
    c.button_primary_hover = hover;
    c.button_primary_active = active;
    c.button_primary_foreground = white();
    c.progress_bar = blue;
    c.ring = blue;
    c.list_active = blue.opacity(if dark { 0.22 } else { 0.14 });
    c.list_active_border = blue;
    // Components read the derived tokens, so keep them in step.
    let t = &mut theme.tokens;
    t.primary = blue.into();
    t.primary_hover = hover.into();
    t.primary_active = active.into();
    t.primary_foreground = white().into();
    t.button_primary = blue.into();
    t.button_primary_hover = hover.into();
    t.button_primary_active = active.into();
    t.button_primary_foreground = white().into();
    t.progress_bar = blue.into();
    t.ring = blue.into();
    t.list_active = blue.opacity(if dark { 0.22 } else { 0.14 }).into();
    t.list_active_border = blue.into();
}

/// The app logo (same SVG the exe icon is generated from).
fn logo() -> Arc<Image> {
    static LOGO: std::sync::OnceLock<Arc<Image>> = std::sync::OnceLock::new();
    LOGO.get_or_init(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../../../assets/logo.svg").to_vec(),
        ))
    })
    .clone()
}

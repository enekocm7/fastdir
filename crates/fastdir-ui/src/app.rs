//! Main window: drive picker, scan progress, directory list, extension list
//! and treemap, laid out like WinDirStat.

use crate::treemap_view::{TreemapEvent, TreemapView, is_toggle_click};
use fastdir_core::drives::{self, Drive};
use fastdir_core::format;
use fastdir_core::fsops;
use fastdir_core::model::{NO_EXT, Node, NodePath, Tree};
use fastdir_core::scanner::{self, Progress, ScanResult};
use fastdir_settings::Settings;
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
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
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
    ]
);

mod context_menu;
mod dialogs;
mod file_list;
mod scan_state;
mod selection;
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
    ]);
}

const ROW_H: f32 = 22.;
const COL_BAR: f32 = 110.;
const COL_PCT: f32 = 62.;
const COL_SIZE: f32 = 92.;
const COL_ITEMS: f32 = 82.;
const COL_FILES: f32 = 82.;
const COL_DIRS: f32 = 74.;
const COL_DATE: f32 = 128.;

/// Colours for the "subtree percentage" bars, one per depth (WinDirStat style).
const DEPTH_COLORS: [u32; 8] = [
    0x3b82f6, 0xef4444, 0x22c55e, 0xeab308, 0xa855f7, 0x06b6d4, 0xf97316, 0xec4899,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    Name,
    Size,
    Items,
    Files,
    Dirs,
    Modified,
}

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
struct Target {
    path: NodePath,
    fs: PathBuf,
    is_dir: bool,
    size: u64,
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
    busy: Option<SharedString>,
    settings: Settings,
    /// A newer release the user can install.
    available: Option<Arc<Release>>,
    checking: bool,
    updating: bool,
    _subscriptions: Vec<Subscription>,
    _rename_sub: Option<Subscription>,
    _scan_task: Option<Task<()>>,
    _ticker: Option<Task<()>>,
    _update_ticker: Option<Task<()>>,
}

impl FastDir {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let treemap = cx.new(|_| TreemapView::new());
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
        Self {
            focus,
            drives: drives::list(),
            phase: Phase::Home,
            tree: None,
            scan_root: None,
            last_scan: None,
            rows: Vec::new(),
            ext_rows: Vec::new(),
            selected: None,
            marked: BTreeSet::new(),
            anchor: None,
            selected_ext: None,
            sort: (SortKey::Size, true),
            list_scroll: UniformListScrollHandle::new(),
            ext_scroll: UniformListScrollHandle::new(),
            treemap,
            busy: None,
            settings: Settings::load(),
            available: None,
            checking: false,
            updating: false,
            _subscriptions: vec![sub],
            _rename_sub: None,
            _scan_task: None,
            _ticker: None,
            _update_ticker: None,
        }
    }

    pub(super) fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_resizable("main-split")
            .child(
                resizable_panel()
                    .size(px(420.))
                    .size_range(px(120.)..Pixels::MAX)
                    .child(
                        h_resizable("top-split")
                            .child(
                                resizable_panel()
                                    .size_range(px(400.)..Pixels::MAX)
                                    .child(self.render_list(cx)),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(330.))
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
}

use std::rc::Rc;

impl Render for FastDir {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.phase {
            Phase::Home => self.render_home(cx).into_any_element(),
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

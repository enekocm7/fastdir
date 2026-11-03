//! Main window: drive picker, scan progress, directory list, extension list
//! and treemap, laid out like WinDirStat.

use crate::treemap_view::{TreemapEvent, TreemapView, is_toggle_click};
use fastdir_core::categories::{CategoryMap, FileCategory};
use fastdir_core::drives::{self, Drive};
use fastdir_core::elevate;
use fastdir_core::filter::Filter;
use fastdir_core::format;
use std::collections::VecDeque;
use gpui_kit::component::popover::Popover;
use fastdir_core::fsops;
use fastdir_core::integration;
use fastdir_core::model::{FileEntry, NO_EXT, Node, NodePath, Subtree, Tree};
use fastdir_core::scanner::{self, LiveScanResult, Progress};
use fastdir_settings::{
    DateFormat, DeleteAction, ExtGrouping, PercentBase, Settings, SizeUnit, SortColumn as SortKey,
    StartupMode, ThemePreference, TreemapColorMode, TreemapPalette, UpdateChannel, UpdateFrequency,
    WindowState,
};
use fastdir_treemap::{self as treemap, ColorMode, Palette, RenderOptions};
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
use std::net::TcpListener;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
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
        CopyName,
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
        FocusSearch,
        OpenFilters,
        OpenSettings,
        ExportResults,
    ]
);

mod context_menu;
mod dialogs;
mod export;
mod file_list;
mod filter;
mod platform;
mod scan_state;
mod selection;
mod settings_ui;
mod status;
mod toolbar;
mod update_ui;

use self::export::ExportForm;
use self::filter::FilterForm;

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
        KeyBinding::new("ctrl-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-f", OpenFilters, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-e", ExportResults, Some(CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(CONTEXT)),
    ]);
    // macOS users expect Cmd: bind it alongside Ctrl so both work. A full
    // shortcut overhaul (remappable keys) is out of scope.
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-enter", OpenItem, Some(CONTEXT)),
        KeyBinding::new("cmd-e", RevealItem, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-c", CopyPath, Some(CONTEXT)),
        KeyBinding::new("cmd-m", MoveItem, Some(CONTEXT)),
        KeyBinding::new("cmd-f5", Rescan, Some(CONTEXT)),
        KeyBinding::new("cmd-r", Rescan, Some(CONTEXT)),
        KeyBinding::new("cmd-o", OpenFolder, Some(CONTEXT)),
        KeyBinding::new("cmd-t", ToggleTheme, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-f", OpenFilters, Some(CONTEXT)),
        KeyBinding::new("cmd-,", OpenSettings, Some(CONTEXT)),
    ]);
}

/// Base font size at 100% interface scale.
const BASE_FONT_SIZE: f32 = 16.;
/// Base monospace font size at 100% interface scale.
const BASE_MONO_FONT_SIZE: f32 = 13.;

/// Applies the interface scale. gpui-component's root resets the window's rem
/// size from `Theme::font_size` on every frame, so the scale must live there;
/// a direct `Window::set_rem_size` would be overwritten before the next paint.
pub fn apply_ui_scale(scale: f32, cx: &mut App) {
    let scale = scale.clamp(0.5, 2.0);
    let theme = Theme::global_mut(cx);
    theme.font_size = px(BASE_FONT_SIZE * scale);
    theme.mono_font_size = px(BASE_MONO_FONT_SIZE * scale);
    cx.refresh_windows();
}

/// A size given in pixels at 100% scale that follows the interface scale.
fn sp(v: f32) -> Rems {
    rems(v / BASE_FONT_SIZE)
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

/// Map the persisted treemap enums to the renderer's types.
fn palette_of(p: TreemapPalette) -> Palette {
    match p {
        TreemapPalette::Default => Palette::Default,
        TreemapPalette::ColorBlind => Palette::ColorBlind,
        TreemapPalette::Mono => Palette::Mono,
    }
}

fn color_mode_of(m: TreemapColorMode) -> ColorMode {
    match m {
        TreemapColorMode::Extension => ColorMode::Extension,
        TreemapColorMode::Age => ColorMode::Age,
        TreemapColorMode::Category => ColorMode::Category,
    }
}

fn treemap_render_options(settings: &Settings) -> RenderOptions {
    let t = &settings.treemap;
    RenderOptions {
        highlight: None,
        highlight_category: None,
        show_free: t.show_free_space,
        show_unknown: t.show_unknown,
        use_disk: t.disk_size,
        palette: palette_of(t.palette),
        color_mode: color_mode_of(t.color_mode),
        cushion: t.cushion,
        cushion_strength: t.cushion_strength.clamp(0.0, 2.0),
        borders: t.borders,
        min_cell: t.min_cell.clamp(0.5, 16.0),
        now: RenderOptions::default().now,
    }
}

/// Map the updater channel setting to the updater crate's channel.
fn update_channel_of(channel: UpdateChannel) -> fastdir_updater::Channel {
    match channel {
        UpdateChannel::Stable => fastdir_updater::Channel::Stable,
        UpdateChannel::Prerelease => fastdir_updater::Channel::Prerelease,
    }
}

/// The two lists in the top-left panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListTab {
    Tree,
    TopFiles,
}

/// One row of the extension panel: a category header or an extension below
/// it (the flat extension view uses only the latter).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExtRow {
    Category(FileCategory),
    Extension { ext: u16 },
}
/// What a click does to the selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClickMode {
    /// Select just this item.
    Plain,
    /// Ctrl+click (Cmd+click on macOS): add to or remove from the selection.
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

/// What a queued file operation does.
#[derive(Clone)]
enum PendingKind {
    Delete { permanent: bool },
    Move { dest: PathBuf },
}

/// A delete or move waiting for the running one to finish. The targets are
/// snapshotted when it is queued.
struct PendingOp {
    id: u64,
    label: SharedString,
    targets: Vec<Target>,
    kind: PendingKind,
}

#[derive(Clone)]
struct Row {
    path: NodePath,
    depth: u16,
    /// The row itself passes the active filter (ancestors are shown for
    /// context and have this cleared).
    matched: bool,
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
    /// Whether the home screen lists drives (off for the "open nothing"
    /// startup mode until the user goes home or starts a scan).
    home_drives: bool,
    tree: Option<Arc<RwLock<Tree>>>,
    scan_root: Option<PathBuf>,
    last_scan: Option<(Duration, bool)>,
    /// Folders that could not be read in the last finished scan (access denied).
    last_errors: u64,
    /// Tree generation the visible rows were built from during a live scan.
    partial_generation: u64,
    rows: Vec<Row>,
    /// The tree view or the flat "largest files" list.
    tab: ListTab,
    /// Search box contents drive `filter` live.
    search: Entity<InputState>,
    filter: Filter,
    filter_form: FilterForm,
    export_form: ExportForm,
    /// The N largest files in the whole scan, sorted by `top_sort`.
    top_files: Vec<FileEntry>,
    /// The last top-files computation is stale and should run again when the
    /// tab is shown.
    top_stale: bool,
    top_selected: Option<usize>,
    top_sort: (SortKey, bool),
    top_scroll: UniformListScrollHandle,
    /// Rows of the extension panel, in display order.
    ext_rows: Vec<ExtRow>,
    /// Categories expanded in the "By category" view.
    expanded_categories: BTreeSet<FileCategory>,
    /// The cursor row. With a multi-selection it is the item clicked last.
    selected: Option<usize>,
    /// Multi-selection (Ctrl/Cmd/Shift+click). Empty means "just the cursor row".
    /// Never contains the scan root. Cleared whenever the tree changes, since
    /// node paths shift then.
    marked: BTreeSet<NodePath>,
    /// Where a Shift+click range starts.
    anchor: Option<NodePath>,
    /// Extension rank highlighted in the treemap, picked from the ext list.
    selected_ext: Option<u16>,
    /// Category highlighted in the treemap, picked from the category list.
    selected_category: Option<FileCategory>,
    sort: (SortKey, bool),
    list_scroll: UniformListScrollHandle,
    ext_scroll: UniformListScrollHandle,
    treemap: Entity<TreemapView>,
    /// Divider state for the top/treemap split and the list/extensions split.
    main_split: Entity<ResizableState>,
    top_split: Entity<ResizableState>,
    busy: Option<SharedString>,
    op: Option<FileOp>,
    /// Operations waiting to run after `op`, in order.
    op_queue: VecDeque<PendingOp>,
    next_op_id: u64,
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
    single_instance_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    _single_instance_task: Option<Task<()>>,
}

impl FastDir {
    pub fn new(
        settings: Settings,
        initial_path: Option<PathBuf>,
        primary_listener: Option<TcpListener>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sort = {
            let s = settings.effective_sort();
            (s.column, s.descending)
        };
        apply_ui_scale(settings.appearance.ui_scale, cx);
        let decimal_sizes = settings.list.size_unit == SizeUnit::Decimal;
        let render_opts = treemap_render_options(&settings);
        let treemap = cx.new(|_| TreemapView::new());
        treemap.update(cx, |t, cx| {
            t.set_render(&render_opts, decimal_sizes, cx);
            t.set_language(fastdir_locale::resolve(&settings.locale.language), cx);
        });
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
                    this.tab = ListTab::Tree;
                    this.reveal_in_list(path.clone(), mode, cx);
                    this.focus.focus(window, cx);
                }
                TreemapEvent::Activate(path) => {
                    this.tab = ListTab::Tree;
                    this.reveal_in_list(path.clone(), ClickMode::Plain, cx);
                    this.open_selected(&OpenItem, window, cx);
                }
            },
        );
        let search_placeholder: SharedString = fastdir_locale::fmt(
            &settings.locale.language,
            "search.placeholder",
            &[("mod", platform::mod_key())],
        )
        .into();
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder.clone()));
        let search_sub = cx.subscribe_in(
            &search,
            window,
            |this, state, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    let query = state.read(cx).value().to_string();
                    this.set_filter_query(query, cx);
                }
                InputEvent::PressEnter { .. } => {
                    this.focus.focus(window, cx);
                }
                _ => {}
            },
        );
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        // `fastdir <path>` starts scanning right away; otherwise the startup
        // mode decides: re-scan the last path, or open the drive overview.
        let startup_path = initial_path.or_else(|| {
            (settings.startup.mode == StartupMode::Last)
                .then(|| settings.startup.last_path.clone())
                .flatten()
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
        });
        // "Open nothing" shows a blank home until the user scans or goes home.
        let home_drives = startup_path.is_some() || settings.startup.mode != StartupMode::Empty;
        if let Some(path) = startup_path {
            cx.defer_in(window, move |this: &mut Self, window, cx| {
                this.start_scan(path, window, cx)
            });
        }
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.startup_update_tasks(window, cx)
        });
        cx.defer_in(window, |this: &mut Self, window, cx| {
            this.install_window_handlers(window, cx)
        });
        let single_instance_rx = primary_listener.map(|listener| {
            let (tx, rx) = mpsc::channel();
            integration::serve_requests(listener, tx);
            rx
        });
        let mut this = Self {
            focus,
            drives: drives::list(),
            phase: Phase::Home,
            home_drives,
            tree: None,
            scan_root: None,
            last_scan: None,
            last_errors: 0,
            partial_generation: 0,
            rows: Vec::new(),
            tab: ListTab::Tree,
            search,
            filter: Filter::default(),
            filter_form: FilterForm::default(),
            export_form: ExportForm::default(),
            top_files: Vec::new(),
            top_stale: true,
            top_selected: None,
            top_sort: (SortKey::Size, true),
            top_scroll: UniformListScrollHandle::new(),
            ext_rows: Vec::new(),
            expanded_categories: BTreeSet::new(),
            selected: None,
            marked: BTreeSet::new(),
            anchor: None,
            selected_ext: None,
            selected_category: None,
            sort,
            list_scroll: UniformListScrollHandle::new(),
            ext_scroll: UniformListScrollHandle::new(),
            treemap,
            main_split,
            top_split,
            busy: None,
            op: None,
            op_queue: VecDeque::new(),
            next_op_id: 0,
            settings,
            available: None,
            checking: false,
            updating: false,
            _subscriptions: vec![sub, search_sub],
            _appearance_sub: None,
            _rename_sub: None,
            _scan_task: None,
            _ticker: None,
            _update_ticker: None,
            _op_ticker: None,
            single_instance_rx,
            _single_instance_task: None,
        };
        if this.single_instance_rx.is_some() {
            this._single_instance_task = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let request: Option<Option<PathBuf>> = this
                        .update(cx, |this, _| {
                            this.single_instance_rx
                                .as_ref()
                                .and_then(|rx| rx.try_recv().ok())
                        })
                        .ok()
                        .flatten();
                    let Some(path) = request.flatten() else {
                        continue;
                    };
                    if !path.is_dir() {
                        continue;
                    }
                    this.update_in(cx, |this, window, cx| this.start_scan(path, window, cx))
                        .ok();
                }
            }));
        }
        this
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
        self.settings.treemap.show_free_space = !self.settings.treemap.show_free_space;
        self.settings.save();
        self.refresh_treemap_options(cx);
        cx.notify();
    }

    pub(super) fn toggle_unknown(
        &mut self,
        _: &ToggleUnknown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.treemap.show_unknown = !self.settings.treemap.show_unknown;
        self.settings.save();
        self.refresh_treemap_options(cx);
        cx.notify();
    }

    /// Show or hide empty folders in the directory list.
    pub(super) fn toggle_hide_empty(&mut self, cx: &mut Context<Self>) {
        self.settings.list.hide_empty_folders = !self.settings.list.hide_empty_folders;
        self.settings.save();
        self.rebuild_rows();
        if let Some(ix) = self.selected {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Toggle the list-only layout (hides the treemap panel).
    pub(super) fn toggle_treemap_visible(&mut self, cx: &mut Context<Self>) {
        self.settings.treemap.show = !self.settings.treemap.show;
        self.settings.save();
        cx.notify();
    }

    /// Show or hide finished folders while a scan is still running.
    pub(super) fn toggle_partial_results(&mut self, cx: &mut Context<Self>) {
        self.settings.scan.show_partial_results = !self.settings.scan.show_partial_results;
        self.settings.save();
        let show = self.settings.scan.show_partial_results;
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
        self.settings.treemap.disk_size = !self.settings.treemap.disk_size;
        self.settings.save();
        self.refresh_treemap_options(cx);
        cx.notify();
    }

    /// The scanner policy built from `[scan]` settings. Passed into
    /// `fastdir-core` on every scan rather than read globally.
    pub(super) fn scan_options(&self) -> scanner::ScanOptions {
        let s = &self.settings.scan;
        scanner::ScanOptions {
            excluded_globs: s.excluded.clone(),
            skip_hidden_system: s.skip_hidden_system,
            follow_junctions: s.follow_junctions,
            cross_volumes: s.cross_volumes,
            threads: s.thread_count.map(|n| (n as usize).clamp(1, 64)),
        }
    }

    /// Built-in file-type categories plus the user's `[categories]` overrides.
    pub(super) fn category_map(&self) -> CategoryMap {
        CategoryMap::with_overrides(
            self.settings
                .categories
                .iter()
                .map(|(name, extensions)| (name.clone(), extensions.clone())),
        )
    }

    /// Push all `[treemap]` settings (and the size-unit flag used for hover
    /// labels) into the treemap view and redraw it.
    pub(super) fn refresh_treemap_options(&mut self, cx: &mut Context<Self>) {
        let opts = treemap_render_options(&self.settings);
        let decimal = self.decimal_sizes();
        self.treemap.update(cx, |t, cx| {
            t.set_render(&opts, decimal, cx);
            t.invalidate(cx);
        });
    }

    pub(super) fn decimal_sizes(&self) -> bool {
        self.settings.list.size_unit == SizeUnit::Decimal
    }

    /// Resolved UI language code (`en`, `es`, …).
    pub(super) fn lang(&self) -> &'static str {
        fastdir_locale::resolve(&self.settings.locale.language)
    }

    /// Localized string. Every key exists in English, so with the fallback
    /// this never fails; unknown keys surface as-is for debugging.
    pub(super) fn tr(&self, key: &'static str) -> &'static str {
        fastdir_locale::t(&self.settings.locale.language, key)
    }

    /// Localized template with `{name}` placeholders filled in.
    pub(super) fn tr_fmt(&self, key: &'static str, args: &[(&str, &str)]) -> String {
        fastdir_locale::fmt(&self.settings.locale.language, key, args)
    }

    /// Localized tooltip for a modifier-key shortcut: fills `{mod}` with
    /// Cmd on macOS, Ctrl everywhere else.
    pub(super) fn mod_tip(&self, key: &'static str) -> String {
        self.tr_fmt(key, &[("mod", platform::mod_key())])
    }

    /// OS-appropriate trash name for the current language.
    pub(super) fn trash_name(&self) -> &'static str {
        fastdir_locale::trash_name(&self.settings.locale.language)
    }

    /// OS-appropriate file-manager label for the current language.
    pub(super) fn reveal_label(&self) -> &'static str {
        fastdir_locale::reveal_label(&self.settings.locale.language)
    }

    /// Switch the interface language, persist it and repaint everything
    /// (including the treemap hover labels, which cache their text).
    pub(super) fn set_language(
        &mut self,
        code: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.locale.language = code.to_string();
        self.settings.save();
        let placeholder = self.mod_tip("search.placeholder");
        self.search.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
        });
        let lang = self.lang();
        self.treemap.update(cx, |t, cx| t.set_language(lang, cx));
        cx.notify();
    }

    /// Integer with locale digit grouping.
    pub(super) fn count(&self, n: u64) -> String {
        fastdir_locale::format_count(n, &self.settings.locale.language)
    }

    /// Localized drive-kind name (`Local Disk`, `Disco local`, …).
    pub(super) fn drive_kind_name(&self, kind: drives::DriveKind) -> &'static str {
        fastdir_locale::drive_kind(kind.id(), &self.settings.locale.language)
    }

    /// Display name for a tree node: drive pseudo-items (`<Free space>`,
    /// `<Unknown>`) are translated, everything else is shown as-is.
    pub(super) fn display_name<'a>(&self, name: &'a str) -> &'a str {
        fastdir_locale::special_display(name, &self.settings.locale.language).unwrap_or(name)
    }

    /// Display form of a file-system path: a trailing pseudo-item name is
    /// translated (`C:\<Free space>`), everything else is shown as-is.
    pub(super) fn display_fs_path(&self, full: &std::path::Path) -> String {
        let text = full.display().to_string();
        let tail = full.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(local) = fastdir_locale::special_display(tail, &self.settings.locale.language) {
            let mut out = text;
            out.truncate(out.len() - tail.len());
            out.push_str(local);
            out
        } else {
            text
        }
    }

    /// Size formatted with the configured unit system.
    pub(super) fn bytes(&self, n: u64) -> String {
        format::bytes_with(n, self.decimal_sizes())
    }

    /// Timestamp formatted with the configured date style and locale.
    pub(super) fn date(&self, unix: i64) -> String {
        format::date_with_in(
            unix,
            self.settings.list.date_format == DateFormat::Relative,
            &self.settings.locale.language,
        )
    }

    /// Subtree share: of the parent folder, or of the whole scan.
    pub(super) fn percent_of(&self, tree: &Tree, path: &NodePath, node: &Node) -> String {
        let whole = if self.settings.list.percent_base == PercentBase::Total {
            tree.root().size
        } else if path.is_empty() {
            node.size
        } else {
            tree.get(&path[..path.len() - 1])
                .map(|p| p.size)
                .unwrap_or(node.size)
        };
        format::percent_in(node.size, whole, &self.settings.locale.language)
    }

    pub(super) fn render_ready(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let main_height = panel_px(self.settings.panels.main.first().copied(), 420.);
        let ext_width = panel_px(self.settings.panels.top.get(1).copied(), 330.);
        let banner = self.render_elevate_banner(cx);
        let lists = h_resizable("top-split")
            .with_state(&self.top_split)
            .on_resize({
                let entity = entity.clone();
                move |state, _, cx| {
                    let sizes = state.read(cx).sizes().iter().map(|s| s.as_f32()).collect();
                    entity
                        .update(cx, |this, _| this.remember_split(Split::Top, sizes))
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
            );
        // List-only layout for small screens or very large scans.
        let body: AnyElement = if !self.settings.treemap.show {
            div().size_full().child(lists).into_any_element()
        } else {
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
                        .child(lists),
                )
                .child(resizable_panel().size_range(px(80.)..Pixels::MAX).child({
                    let entity = cx.entity();
                    div()
                        .id("treemap-panel")
                        .size_full()
                        .child(self.treemap.clone())
                        .context_menu(move |menu, _, cx| {
                            entity.read(cx).build_context_menu(menu, cx)
                        })
                }))
                .into_any_element()
        };

        match banner {
            Some(banner) => v_flex()
                .size_full()
                .child(banner)
                .child(div().flex_1().min_h_0().w_full().child(body))
                .into_any_element(),
            None => body,
        }
    }

    /// Warning shown above the results when folders could not be read.
    /// Offers to relaunch elevated, passing the current scan path so the new
    /// instance resumes the same scan.
    fn render_elevate_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let errors = match &self.phase {
            Phase::Scanning { progress, .. } => progress.errors.load(Ordering::Relaxed),
            Phase::Ready => self.last_errors,
            Phase::Home => return None,
        };
        if errors == 0 {
            return None;
        }
        let theme = cx.theme();
        let root = self.scan_root.clone();
        let elevated = elevate::is_elevated();
        Some(
            h_flex()
                .flex_shrink_0()
                .w_full()
                .px_3()
                .py_1()
                .gap_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.warning.opacity(0.12))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(
                            self.tr_fmt("banner.limited", &[("errors", &self.count(errors))]),
                        ),
                )
                .when(!elevated, |d| {
                    d.child(
                        Button::new("restart-admin-ready")
                            .small()
                            .outline()
                            .icon(IconName::Shield)
                            .label(self.tr("banner.restart_admin"))
                            .tooltip(self.tr("banner.restart_tip"))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.restart_elevated(root.clone(), w, cx)
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// Relaunch the app elevated, passing `path` so the new instance resumes
    /// the same scan. Quits this instance on success; UAC cancellation or a
    /// launch failure is reported and keeps this instance running.
    pub(super) fn restart_elevated(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match elevate::restart_as_admin(path.as_deref()) {
            Ok(()) => cx.quit(),
            Err(e) if e == "Elevation was cancelled" => {
                let this = &*self;
                window.push_notification(
                    Notification::info(this.tr("banner.elev_cancelled_body"))
                        .title(this.tr("banner.elev_cancelled_title")),
                    cx,
                );
            }
            Err(e) => {
                window.push_notification(
                    Notification::error(e).title(self.tr("banner.elev_failed_title")),
                    cx,
                );
            }
        }
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
                        if this.settings.appearance.theme == ThemePreference::System {
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
        self.settings.appearance.theme = theme;
        self.settings.save();
        self.apply_theme(window, cx);
        cx.notify();
    }

    /// Apply the saved preference; `System` reads the current Windows mode.
    pub(super) fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = match self.settings.appearance.theme {
            ThemePreference::Light => ThemeMode::Light,
            ThemePreference::Dark => ThemeMode::Dark,
            ThemePreference::System => ThemeMode::from(window.appearance()),
        };
        Theme::change(mode, Some(window), cx);
        apply_accent(cx);
        apply_ui_scale(self.ui_scale(), cx);
        window.refresh();
    }

    pub(super) fn ui_scale(&self) -> f32 {
        self.settings.appearance.ui_scale.clamp(0.5, 2.0)
    }

    pub(super) fn apply_ui_scale(&mut self, window: &mut Window, cx: &mut App) {
        apply_ui_scale(self.ui_scale(), cx);
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
            && self.settings.scan.show_partial_results
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
            .on_action(cx.listener(Self::copy_name))
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
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::open_filters))
            .on_action(cx.listener(Self::open_export))
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

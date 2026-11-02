//! Main window: drive picker, scan progress, directory list, extension list
//! and treemap, laid out like WinDirStat.

use crate::drives::{self, Drive};
use crate::format;
use crate::fsops;
use crate::model::{Node, NodePath, Tree};
use crate::scanner::{self, Progress, ScanResult};
use crate::treemap;
use crate::treemap_view::{TreemapEvent, TreemapView};
use gpui_kit::assets::IconName;
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
use gpui_kit::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::prelude::FluentBuilder as _;
use parking_lot::RwLock;
use std::cell::Cell;
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
    ]
);

const CONTEXT: &str = "FastDir";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
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
    selected: Option<usize>,
    sort: (SortKey, bool),
    list_scroll: UniformListScrollHandle,
    ext_scroll: UniformListScrollHandle,
    treemap: Entity<TreemapView>,
    busy: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
    _rename_sub: Option<Subscription>,
    _scan_task: Option<Task<()>>,
    _ticker: Option<Task<()>>,
}

impl FastDir {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let treemap = cx.new(|_| TreemapView::new());
        let sub = cx.subscribe_in(&treemap, window, |this, _, ev: &TreemapEvent, window, cx| match ev {
            TreemapEvent::Select(path) => {
                this.reveal_in_list(path.clone(), cx);
                this.focus.focus(window, cx);
            }
            TreemapEvent::Activate(path) => {
                this.reveal_in_list(path.clone(), cx);
                this.open_selected(&OpenItem, window, cx);
            }
        });
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
                cx.defer_in(window, move |this: &mut Self, window, cx| this.start_scan(path, window, cx));
            }
        }
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
            sort: (SortKey::Size, true),
            list_scroll: UniformListScrollHandle::new(),
            ext_scroll: UniformListScrollHandle::new(),
            treemap,
            busy: None,
            _subscriptions: vec![sub],
            _rename_sub: None,
            _scan_task: None,
            _ticker: None,
        }
    }

    // ------------------------------------------------------------------
    // Scanning
    // ------------------------------------------------------------------

    fn start_scan(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.drives = drives::list();
        let expected = self
            .drives
            .iter()
            .find(|d| d.path == root)
            .map(|d| d.used())
            .filter(|&u| u > 0);
        let progress = Arc::new(Progress::default());
        self.phase = Phase::Scanning {
            progress: progress.clone(),
            root: root.clone(),
            started: Instant::now(),
            expected,
        };
        self.scan_root = Some(root.clone());
        self.tree = None;
        self.rows.clear();
        self.selected = None;
        self.treemap.update(cx, |t, cx| t.set_tree(None, cx));
        window.set_window_title(&format!("FastDir — scanning {}", root.display()));

        let p = progress.clone();
        let job = cx
            .background_executor()
            .spawn(async move { scanner::scan(root, p) });
        self._scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| this.finish_scan(result, window, cx))
                .ok();
        }));
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let still = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.phase, Phase::Scanning { .. })
                    })
                    .unwrap_or(false);
                if !still {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn finish_scan(&mut self, result: ScanResult, window: &mut Window, cx: &mut Context<Self>) {
        let ScanResult {
            tree,
            elapsed,
            cancelled,
        } = result;
        let title = format!(
            "FastDir — {}{}",
            tree.root_path.display(),
            if cancelled { " (incomplete)" } else { "" }
        );
        window.set_window_title(&title);
        let files = tree.root.files;
        let size = tree.root.size;
        let tree = Arc::new(RwLock::new(tree));
        self.tree = Some(tree.clone());
        self.last_scan = Some((elapsed, cancelled));
        self.phase = Phase::Ready;
        self.selected = Some(0);
        self.rebuild_rows();
        self.treemap.update(cx, |t, cx| {
            t.set_tree(Some(tree), cx);
            t.set_selection(Some(Vec::new()), cx);
        });
        let msg = format!(
            "Scanned {} files ({}) in {}",
            format::count(files as u64),
            format::bytes(size),
            format::duration(elapsed)
        );
        window.push_notification(
            Notification::new()
                .message(msg)
                .with_type(if cancelled {
                    NotificationType::Warning
                } else {
                    NotificationType::Success
                })
                .title(if cancelled { "Scan stopped" } else { "Scan complete" }),
            cx,
        );
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn stop_scan(&mut self, _: &StopScan, _: &mut Window, cx: &mut Context<Self>) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
            cx.notify();
        }
    }

    fn rescan(&mut self, _: &Rescan, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.scan_root.clone() {
            self.start_scan(root, window, cx);
        }
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Scan folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| this.start_scan(path, window, cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.phase = Phase::Home;
        self.tree = None;
        self.rows.clear();
        self.selected = None;
        self.drives = drives::list();
        self.treemap.update(cx, |t, cx| t.set_tree(None, cx));
        window.set_window_title("FastDir");
        cx.notify();
    }

    // ------------------------------------------------------------------
    // List model
    // ------------------------------------------------------------------

    fn child_order(node: &Node, sort: (SortKey, bool)) -> Vec<u32> {
        let mut idx: Vec<u32> = (0..node.children.len() as u32).collect();
        let (key, desc) = sort;
        if key == SortKey::Size && desc {
            return idx; // children are stored in this order
        }
        let c = &node.children;
        idx.sort_by(|&a, &b| {
            let (a, b) = (&c[a as usize], &c[b as usize]);
            let ord = match key {
                SortKey::Name => b
                    .is_dir()
                    .cmp(&a.is_dir())
                    .reverse()
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Items => a.items().cmp(&b.items()),
                SortKey::Files => a.files.cmp(&b.files),
                SortKey::Dirs => a.dirs.cmp(&b.dirs),
                SortKey::Modified => a.modified.cmp(&b.modified),
            };
            if desc { ord.reverse() } else { ord }
        });
        if key == SortKey::Name && desc {
            // Keep folders first even when names are descending.
            idx.sort_by_key(|&i| !c[i as usize].is_dir());
        }
        idx
    }

    fn rebuild_rows(&mut self) {
        let selected_path = self.selected_path();
        self.rows.clear();
        let Some(tree) = self.tree.clone() else { return };
        let tree = tree.read();
        fn walk(node: &Node, path: &mut NodePath, depth: u16, rows: &mut Vec<Row>, sort: (SortKey, bool)) {
            rows.push(Row {
                path: path.clone(),
                depth,
            });
            if node.expanded && node.is_dir() {
                for i in FastDir::child_order(node, sort) {
                    path.push(i);
                    walk(&node.children[i as usize], path, depth + 1, rows, sort);
                    path.pop();
                }
            }
        }
        let mut path = Vec::new();
        walk(&tree.root, &mut path, 0, &mut self.rows, self.sort);
        self.ext_rows = (0..tree.exts.len() as u16)
            .filter(|&i| tree.exts[i as usize].files > 0)
            .collect();
        if let Some(sel) = selected_path {
            self.selected = self.rows.iter().position(|r| r.path == sel).or(Some(0));
        }
    }

    fn selected_path(&self) -> Option<NodePath> {
        self.selected
            .and_then(|i| self.rows.get(i))
            .map(|r| r.path.clone())
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = ix.min(self.rows.len() - 1);
        self.selected = Some(ix);
        self.list_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        let path = self.rows[ix].path.clone();
        self.treemap
            .update(cx, |t, cx| t.set_selection(Some(path), cx));
        cx.notify();
    }

    /// Expand all ancestors of `path`, select it and scroll it into view.
    fn reveal_in_list(&mut self, path: NodePath, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.clone() else { return };
        {
            let mut t = tree.write();
            for depth in 0..path.len() {
                if let Some(n) = t.get_mut(&path[..depth]) {
                    n.expanded = true;
                }
            }
        }
        self.rebuild_rows();
        if let Some(ix) = self.rows.iter().position(|r| r.path == path) {
            self.selected = Some(ix);
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        self.treemap
            .update(cx, |t, cx| t.set_selection(Some(path), cx));
        cx.notify();
    }

    fn set_expanded(&mut self, ix: usize, expanded: Option<bool>, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.clone() else { return };
        let Some(row) = self.rows.get(ix) else { return };
        {
            let mut t = tree.write();
            let Some(n) = t.get_mut(&row.path) else { return };
            if !n.is_dir() || n.children.is_empty() {
                return;
            }
            n.expanded = expanded.unwrap_or(!n.expanded);
        }
        self.selected = Some(ix);
        self.rebuild_rows();
        cx.notify();
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort.0 == key {
            self.sort.1 = !self.sort.1;
        } else {
            self.sort = (key, key != SortKey::Name);
        }
        self.rebuild_rows();
        if let Some(ix) = self.selected {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Called after any mutation of the tree.
    fn tree_changed(&mut self, select: Option<NodePath>, cx: &mut Context<Self>) {
        if let Some(p) = &select {
            // Make sure the new selection is visible.
            if let Some(tree) = &self.tree {
                let mut t = tree.write();
                for depth in 0..p.len() {
                    if let Some(n) = t.get_mut(&p[..depth]) {
                        n.expanded = true;
                    }
                }
            }
        }
        self.selected = None;
        self.rebuild_rows();
        let sel = select.unwrap_or_default();
        self.selected = self.rows.iter().position(|r| r.path == sel).or(Some(0));
        if let Some(ix) = self.selected {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        let sel = self.selected_path();
        self.treemap.update(cx, |t, cx| {
            t.invalidate(cx);
            t.set_selection(sel, cx);
        });
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Keyboard navigation
    // ------------------------------------------------------------------

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0).saturating_sub(1);
        self.select_row(ix, cx);
    }
    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.map(|i| i + 1).unwrap_or(0);
        self.select_row(ix, cx);
    }
    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(0, cx);
    }
    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(usize::MAX, cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0).saturating_sub(20);
        self.select_row(ix, cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.selected.unwrap_or(0) + 20;
        self.select_row(ix, cx);
    }
    fn expand_item(&mut self, _: &ExpandItem, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else { return };
        let expanded = self.is_expanded(ix);
        if expanded {
            // Already open: step into the first child.
            if self.rows.get(ix + 1).is_some_and(|r| r.depth > self.rows[ix].depth) {
                self.select_row(ix + 1, cx);
            }
        } else {
            self.set_expanded(ix, Some(true), cx);
        }
    }
    fn collapse_item(&mut self, _: &CollapseItem, w: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else { return };
        if self.is_expanded(ix) {
            self.set_expanded(ix, Some(false), cx);
        } else {
            self.go_parent(&GoParent, w, cx);
        }
    }
    fn toggle_item(&mut self, _: &ToggleItem, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected {
            self.set_expanded(ix, None, cx);
        }
    }
    fn go_parent(&mut self, _: &GoParent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else { return };
        let depth = self.rows[ix].depth;
        if depth == 0 {
            return;
        }
        if let Some(p) = (0..ix).rev().find(|&i| self.rows[i].depth < depth) {
            self.select_row(p, cx);
        }
    }

    fn is_expanded(&self, ix: usize) -> bool {
        let (Some(tree), Some(row)) = (&self.tree, self.rows.get(ix)) else {
            return false;
        };
        tree.read().get(&row.path).is_some_and(|n| n.expanded)
    }

    // ------------------------------------------------------------------
    // File operations
    // ------------------------------------------------------------------

    fn selected_fs_path(&self) -> Option<(NodePath, PathBuf, bool)> {
        let path = self.selected_path()?;
        let tree = self.tree.as_ref()?.read();
        let fs = tree.fs_path(&path)?;
        let is_dir = tree.get(&path)?.is_dir();
        Some((path, fs, is_dir))
    }

    fn open_selected(&mut self, _: &OpenItem, _: &mut Window, _: &mut Context<Self>) {
        if let Some((_, p, _)) = self.selected_fs_path() {
            fsops::open(&p);
        }
    }

    fn reveal_selected(&mut self, _: &RevealItem, _: &mut Window, _: &mut Context<Self>) {
        if let Some((_, p, _)) = self.selected_fs_path() {
            fsops::reveal(&p);
        }
    }

    fn copy_path(&mut self, _: &CopyPath, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, p, _)) = self.selected_fs_path() {
            cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
            window.push_notification("Path copied to clipboard", cx);
        }
    }

    fn trash_item(&mut self, _: &TrashItem, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_delete(false, window, cx);
    }

    fn delete_item(&mut self, _: &DeleteItem, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_delete(true, window, cx);
    }

    fn confirm_delete(&mut self, permanent: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some((path, fs, is_dir)) = self.selected_fs_path() else { return };
        if path.is_empty() {
            window.push_notification(
                (NotificationType::Warning, "The scan root itself cannot be deleted here."),
                cx,
            );
            return;
        }
        let (size, items) = {
            let tree = self.tree.as_ref().unwrap().read();
            let n = tree.get(&path).unwrap();
            (n.size, n.items())
        };
        let what = if is_dir {
            format!(
                "the folder \"{}\" ({}, {} items)",
                fs.display(),
                format::bytes(size),
                format::count(items)
            )
        } else {
            format!("the file \"{}\" ({})", fs.display(), format::bytes(size))
        };
        let description: SharedString = if permanent {
            format!("Permanently delete {what}? This cannot be undone.").into()
        } else {
            format!("Move {what} to the Recycle Bin?").into()
        };
        let title = if permanent { "Delete permanently" } else { "Move to Recycle Bin" };
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let fs = fs.clone();
            alert
                .title(title)
                .description(description.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(if permanent { "Delete" } else { "Move to Recycle Bin" })
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel"),
                )
                .on_ok(move |_, window, cx| {
                    let fs = fs.clone();
                    entity
                        .update(cx, |this, cx| this.run_delete(fs, permanent, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn run_delete(&mut self, fs: PathBuf, permanent: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = Some(format!("Deleting {}…", fs.display()).into());
        cx.notify();
        let target = fs.clone();
        let job = cx
            .background_executor()
            .spawn(async move { fsops::delete(&target, permanent) });
        cx.spawn_in(window, async move |this, cx| {
            let res = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match res {
                    Ok(()) => {
                        let parent = this.remove_from_tree(&fs);
                        this.tree_changed(parent, cx);
                        window.push_notification(
                            Notification::success(format!("Deleted {}", fs.display())),
                            cx,
                        );
                    }
                    Err(e) => {
                        window.push_notification(
                            Notification::error(format!("Could not delete {}: {e}", fs.display()))
                                .title("Delete failed"),
                            cx,
                        );
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Remove the node for `fs` (if present). Returns the parent's new path.
    fn remove_from_tree(&mut self, fs: &std::path::Path) -> Option<NodePath> {
        let tree = self.tree.as_ref()?;
        let mut t = tree.write();
        let path = t.find(fs)?;
        let parent_fs = fs.parent()?.to_path_buf();
        t.remove(&path)?;
        t.find(&parent_fs)
    }

    fn rename_item(&mut self, _: &RenameItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some((path, fs, _)) = self.selected_fs_path() else { return };
        if path.is_empty() {
            return;
        }
        let name = fs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        let entity = cx.entity().downgrade();
        let submit = {
            let input = input.clone();
            let entity = entity.clone();
            let fs = fs.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| -> bool {
                let new_name = input.read(cx).value().to_string();
                let fs = fs.clone();
                entity
                    .update(cx, |this, cx| this.do_rename(fs, new_name, window, cx))
                    .unwrap_or(true)
            })
        };
        let sub = cx.subscribe_in(&input, window, {
            let fs = fs.clone();
            // `this` is already borrowed here, so call into it directly.
            move |this, state, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let new_name = state.read(cx).value().to_string();
                    if this.do_rename(fs.clone(), new_name, window, cx) {
                        window.close_dialog(cx);
                    }
                }
            }
        });
        self._rename_sub = Some(sub);
        let first = Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.focus_handle(cx).focus(window, cx);
                input.update(cx, |s, cx| s.select_all(window, cx));
            }
            let ok = submit.clone();
            dialog
                .title("Rename")
                .w(px(460.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("in {}", fs.parent().map(|p| p.display().to_string()).unwrap_or_default())),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("rename-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("rename-ok")
                                .primary()
                                .label("Rename")
                                .on_click(move |_, window, cx| {
                                    if ok(window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    /// Returns true when the dialog may close.
    fn do_rename(&mut self, fs: PathBuf, new_name: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let old = fs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if new_name.trim() == old {
            return true;
        }
        match fsops::rename(&fs, &new_name) {
            Ok(dest) => {
                let new_path = self.tree.as_ref().and_then(|tree| {
                    let mut t = tree.write();
                    let path = t.find(&fs)?;
                    let name = dest.file_name()?.to_string_lossy().to_string();
                    t.rename(&path, &name)
                });
                self.tree_changed(new_path, cx);
                window.push_notification(
                    Notification::success(format!("Renamed to {}", dest.display())),
                    cx,
                );
                true
            }
            Err(e) => {
                window.push_notification(
                    Notification::error(e).title("Rename failed"),
                    cx,
                );
                false
            }
        }
    }

    fn move_item(&mut self, _: &MoveItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some((path, fs, _)) = self.selected_fs_path() else { return };
        if path.is_empty() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Move here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(dest_dir) = paths.into_iter().next() else { return };
            let src = fs.clone();
            let label: SharedString = format!("Moving {} → {}…", fs.display(), dest_dir.display()).into();
            this.update(cx, |this, cx| {
                this.busy = Some(label);
                cx.notify();
            })
            .ok();
            let dest_dir2 = dest_dir.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let dest = fsops::move_into(&src, &dest_dir2)?;
                    // Measure what arrived so the destination's size is right.
                    let meta = std::fs::symlink_metadata(&dest).ok();
                    let node = if meta.as_ref().is_some_and(|m| m.is_dir()) {
                        scanner::scan_subtree(&dest)
                    } else {
                        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                        let modified = meta
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let name = dest.file_name().unwrap().to_string_lossy();
                        Node::file(name.into(), size, modified)
                    };
                    Ok::<_, String>((dest, node))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match res {
                    Ok((dest, node)) => {
                        let mut select = this.remove_from_tree(&fs);
                        if let Some(tree) = &this.tree {
                            let mut t = tree.write();
                            if let Some(dir) = t.find(&dest_dir) {
                                select = t.insert(&dir, node);
                            }
                        }
                        this.tree_changed(select, cx);
                        window.push_notification(
                            Notification::success(format!("Moved to {}", dest.display())),
                            cx,
                        );
                    }
                    Err(e) => {
                        window.push_notification(Notification::error(e).title("Move failed"), cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-scan just the selected folder and splice it into the tree.
    fn refresh_item(&mut self, _: &RefreshItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some((path, fs, is_dir)) = self.selected_fs_path() else { return };
        if path.is_empty() {
            self.rescan(&Rescan, window, cx);
            return;
        }
        if !is_dir {
            return;
        }
        self.busy = Some(format!("Refreshing {}…", fs.display()).into());
        cx.notify();
        let target = fs.clone();
        let job = cx
            .background_executor()
            .spawn(async move { scanner::scan_subtree(&target) });
        cx.spawn_in(window, async move |this, cx| {
            let mut node = job.await;
            this.update_in(cx, |this, _, cx| {
                this.busy = None;
                node.expanded = true;
                let select = this.tree.as_ref().and_then(|tree| {
                    let mut t = tree.write();
                    let path = t.find(&fs)?;
                    let parent: NodePath = path[..path.len() - 1].to_vec();
                    t.remove(&path)?;
                    t.insert(&parent, node)
                });
                this.tree_changed(select, cx);
            })
            .ok();
        })
        .detach();
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, Some(window), cx);
        apply_accent(cx);
        window.refresh();
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    fn build_context_menu(&self, menu: PopupMenu) -> PopupMenu {
        let is_root = self.selected_path().is_some_and(|p| p.is_empty());
        let is_dir = self.selected_fs_path().is_some_and(|s| s.2);
        menu.action_context(self.focus.clone())
            .menu_with_icon("Open", IconName::ExternalLink, Box::new(OpenItem))
            .menu_with_icon("Show in Explorer", IconName::FolderOpen, Box::new(RevealItem))
            .menu_with_icon("Copy path", IconName::Copy, Box::new(CopyPath))
            .separator()
            .menu_with_icon_and_disabled("Rename…", IconName::Pencil, Box::new(RenameItem), is_root)
            .menu_with_icon_and_disabled("Move to…", IconName::FolderInput, Box::new(MoveItem), is_root)
            .menu_with_icon_and_disabled("Refresh", IconName::RefreshCw, Box::new(RefreshItem), !is_dir)
            .separator()
            .menu_with_icon_and_disabled("Move to Recycle Bin", IconName::Recycle, Box::new(TrashItem), is_root)
            .menu_with_icon_and_disabled("Delete permanently", IconName::Trash, Box::new(DeleteItem), is_root)
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(tree) = self.tree.clone() else { return Vec::new() };
        let tree = tree.read();
        let theme = cx.theme();
        let total = tree.root.size.max(1);
        let selected = self.selected;
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let even = theme.list_even;
        let track = theme.muted;
        let danger = theme.danger;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = self.rows.get(ix) else { continue };
            let Some(node) = tree.get(&row.path) else { continue };
            let parent_size = if row.path.is_empty() {
                node.size
            } else {
                tree.get(&row.path[..row.path.len() - 1]).map(|p| p.size).unwrap_or(node.size)
            };
            let frac = if parent_size == 0 { 0.0 } else { node.size as f32 / parent_size as f32 };
            let is_sel = selected == Some(ix);
            let is_dir = node.is_dir();
            let has_children = is_dir && !node.children.is_empty();
            let depth = row.depth as f32;
            let bar_color = rgb(DEPTH_COLORS[row.depth as usize % DEPTH_COLORS.len()]);
            let icon = if !is_dir {
                let (r, g, b) = treemap::ext_color(node.ext);
                Icon::new(IconName::File)
                    .xsmall()
                    .text_color(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            } else if row.path.is_empty() {
                Icon::new(IconName::HardDrive).xsmall().text_color(muted)
            } else if node.expanded {
                Icon::new(IconName::FolderOpen).xsmall().text_color(rgb(0xe0b04a))
            } else {
                Icon::new(IconName::Folder).xsmall().text_color(rgb(0xe0b04a))
            };
            let chevron = div()
                .id(("chev", ix))
                .size(px(16.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .when(has_children, |d| {
                    d.hover(|s| s.bg(track))
                        .child(
                            Icon::new(if node.expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                                .xsmall()
                                .text_color(muted),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.focus.focus(window, cx);
                                this.set_expanded(ix, None, cx);
                            }),
                        )
                });
            let num = |w: f32, s: String| {
                div()
                    .w(px(w))
                    .flex_shrink_0()
                    .px_2()
                    .text_right()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(s)
            };
            let name_color = if node.error { danger } else { fg };
            let el = h_flex()
                .id(("row", ix))
                .h(px(ROW_H))
                .w_full()
                .text_xs()
                .text_color(fg)
                .when(ix % 2 == 1 && !is_sel, |d| d.bg(even))
                .when(is_sel, |d| d.bg(active).border_l_2().border_color(active_border))
                .when(!is_sel, |d| d.border_l_2().border_color(transparent_black()).hover(|s| s.bg(hover)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        this.focus.focus(window, cx);
                        this.select_row_no_scroll(ix, cx);
                        if ev.click_count == 2 {
                            this.set_expanded(ix, None, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        this.focus.focus(window, cx);
                        this.select_row_no_scroll(ix, cx);
                    }),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w(px(220.))
                        .overflow_hidden()
                        .gap_1()
                        .pl(px(4. + depth * 16.))
                        .child(chevron)
                        .child(icon)
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(name_color)
                                .when(is_dir, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(SharedString::from(node.name.to_string())),
                        ),
                )
                .child(
                    div()
                        .w(px(COL_BAR))
                        .flex_shrink_0()
                        .px_2()
                        .child(
                            div()
                                .h(px(10.))
                                .w_full()
                                .rounded_sm()
                                .bg(track)
                                .overflow_hidden()
                                .child(div().h_full().w(relative(frac)).bg(bar_color)),
                        ),
                )
                .child(num(COL_PCT, format::percent(node.size, parent_size)))
                .child(num(COL_SIZE, format::bytes(node.size)).font_weight(FontWeight::MEDIUM))
                .child(num(COL_ITEMS, if is_dir { format::count(node.items()) } else { String::new() }).text_color(muted))
                .child(num(COL_FILES, if is_dir { format::count(node.files as u64) } else { String::new() }).text_color(muted))
                .child(num(COL_DIRS, if is_dir { format::count(node.dirs as u64) } else { String::new() }).text_color(muted))
                .child(num(COL_DATE, format::date(node.modified)).text_color(muted));
            let _ = total;
            out.push(el.into_any_element());
        }
        out
    }

    fn select_row_no_scroll(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        if let Some(row) = self.rows.get(ix) {
            let path = row.path.clone();
            self.treemap
                .update(cx, |t, cx| t.set_selection(Some(path), cx));
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (key, desc) = self.sort;
        let head = |id: &'static str, label: &'static str, k: SortKey, w: Option<f32>, right: bool, cx: &mut Context<Self>| {
            let active = key == k;
            let arrow = if desc { IconName::ArrowDown } else { IconName::ArrowUp };
            h_flex()
                .id(id)
                .h_full()
                .px_2()
                .gap_1()
                .cursor_pointer()
                .when_some(w, |d, w| d.w(px(w)).flex_shrink_0())
                .when(w.is_none(), |d| d.flex_1().min_w(px(220.)).pl(px(24.)))
                .when(right, |d| d.justify_end())
                .hover(|s| s.bg(cx.theme().list_hover))
                .when(active, |d| d.text_color(cx.theme().foreground))
                .child(label)
                .when(active, |d| d.child(Icon::new(arrow).xsmall()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sort(k, cx)))
        };
        h_flex()
            .h(px(28.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.list_head)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(head("h-name", "Name", SortKey::Name, None, false, cx))
            .child(
                div()
                    .w(px(COL_BAR))
                    .flex_shrink_0()
                    .px_2()
                    .child("Subtree %"),
            )
            .child(head("h-pct", "%", SortKey::Size, Some(COL_PCT), true, cx))
            .child(head("h-size", "Size", SortKey::Size, Some(COL_SIZE), true, cx))
            .child(head("h-items", "Items", SortKey::Items, Some(COL_ITEMS), true, cx))
            .child(head("h-files", "Files", SortKey::Files, Some(COL_FILES), true, cx))
            .child(head("h-dirs", "Folders", SortKey::Dirs, Some(COL_DIRS), true, cx))
            .child(head("h-date", "Last Change", SortKey::Modified, Some(COL_DATE), true, cx))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        let entity = cx.entity();
        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .child(
                div()
                    .id("list-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        uniform_list("rows", count, cx.processor(Self::render_rows))
                            .size_full()
                            .track_scroll(&self.list_scroll),
                    )
                    .child(Scrollbar::vertical(&self.list_scroll))
                    .context_menu(move |menu, _, cx| entity.read(cx).build_context_menu(menu)),
            )
    }

    fn render_ext_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(tree) = self.tree.clone() else { return Vec::new() };
        let tree = tree.read();
        let theme = cx.theme();
        let total = tree.root.size.max(1);
        let muted = theme.muted_foreground;
        let even = theme.list_even;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(&ext) = self.ext_rows.get(ix) else { continue };
            let Some(e) = tree.exts.get(ext as usize) else { continue };
            let (r, g, b) = treemap::ext_color(ext);
            let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
            out.push(
                h_flex()
                    .id(("ext", ix))
                    .h(px(ROW_H))
                    .w_full()
                    .px_2()
                    .gap_2()
                    .text_xs()
                    .when(ix % 2 == 1, |d| d.bg(even))
                    .child(div().size(px(10.)).rounded_sm().flex_shrink_0().bg(color))
                    .child(div().flex_1().min_w(px(50.)).overflow_hidden().whitespace_nowrap().child(SharedString::from(e.name.to_string())))
                    .child(div().w(px(70.)).text_right().font_weight(FontWeight::MEDIUM).child(format::bytes(e.bytes)))
                    .child(div().w(px(48.)).text_right().text_color(muted).child(format::percent(e.bytes, total)))
                    .child(div().w(px(64.)).text_right().text_color(muted).child(format::count(e.files)))
                    .into_any_element(),
            );
        }
        out
    }

    fn render_ext_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.ext_rows.len();
        v_flex()
            .size_full()
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(10.)))
                    .child(div().flex_1().min_w(px(50.)).child("Extension"))
                    .child(div().w(px(70.)).text_right().child("Bytes"))
                    .child(div().w(px(48.)).text_right().child("%"))
                    .child(div().w(px(64.)).text_right().child("Files")),
            )
            .child(
                div()
                    .id("ext-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("exts", count, cx.processor(Self::render_ext_rows))
                            .size_full()
                            .track_scroll(&self.ext_scroll),
                    )
                    .child(Scrollbar::vertical(&self.ext_scroll)),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ready = matches!(self.phase, Phase::Ready);
        let scanning = matches!(self.phase, Phase::Scanning { .. });
        let has_sel = ready && self.selected.is_some();
        let is_root = self.selected_path().is_some_and(|p| p.is_empty());
        let drives = self.drives.clone();
        let entity = cx.entity().downgrade();
        let focus = self.focus.clone();
        let busy = self.busy.is_some();

        let tool = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id).ghost().small().icon(icon).tooltip(tip)
        };

        h_flex()
            .h(px(42.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                Button::new("drives")
                    .small()
                    .primary()
                    .icon(IconName::HardDrive)
                    .label("Scan drive")
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for d in &drives {
                            let path = d.path.clone();
                            let entity = entity.clone();
                            let label = format!(
                                "{}   {} free of {}",
                                d.title(),
                                format::bytes(d.free),
                                format::bytes(d.total)
                            );
                            menu = menu.item(
                                gpui_kit::component::menu::PopupMenuItem::new(label)
                                    .icon(IconName::HardDrive)
                                    .on_click(move |_, window, cx| {
                                        let path = path.clone();
                                        entity
                                            .update(cx, |this, cx| this.start_scan(path, window, cx))
                                            .ok();
                                    }),
                            );
                        }
                        menu.separator().item(
                            gpui_kit::component::menu::PopupMenuItem::new("Scan a folder…")
                                .icon(IconName::FolderSearch)
                                .on_click({
                                    let focus = focus.clone();
                                    move |_, window, cx| {
                                        focus.dispatch_action(&OpenFolder, window, cx);
                                    }
                                }),
                        )
                    }),
            )
            .child(
                tool("open", IconName::FolderSearch, "Scan a folder… (Ctrl+O)")
                    .on_click(cx.listener(|this, _, w, cx| this.open_folder(&OpenFolder, w, cx))),
            )
            .child(Separator::vertical().h(px(20.)).mx_1())
            .child(
                tool("rescan", IconName::RefreshCw, "Rescan everything (Ctrl+R)")
                    .disabled(self.scan_root.is_none() || scanning)
                    .on_click(cx.listener(|this, _, w, cx| this.rescan(&Rescan, w, cx))),
            )
            .child(
                tool("stop", IconName::CircleStop, "Stop scanning (Esc)")
                    .disabled(!scanning)
                    .on_click(cx.listener(|this, _, w, cx| this.stop_scan(&StopScan, w, cx))),
            )
            .child(Separator::vertical().h(px(20.)).mx_1())
            .child(
                tool("up", IconName::ArrowUp, "Select parent (Backspace)")
                    .disabled(!has_sel || is_root)
                    .on_click(cx.listener(|this, _, w, cx| this.go_parent(&GoParent, w, cx))),
            )
            .child(
                tool("reveal", IconName::FolderOpen, "Show in Explorer (Ctrl+E)")
                    .disabled(!has_sel)
                    .on_click(cx.listener(|this, _, w, cx| this.reveal_selected(&RevealItem, w, cx))),
            )
            .child(
                tool("refresh", IconName::RotateCw, "Refresh selected folder (F5)")
                    .disabled(!has_sel || busy)
                    .on_click(cx.listener(|this, _, w, cx| this.refresh_item(&RefreshItem, w, cx))),
            )
            .child(Separator::vertical().h(px(20.)).mx_1())
            .child(
                tool("rename", IconName::Pencil, "Rename (F2)")
                    .disabled(!has_sel || is_root || busy)
                    .on_click(cx.listener(|this, _, w, cx| this.rename_item(&RenameItem, w, cx))),
            )
            .child(
                tool("move", IconName::FolderInput, "Move to… (Ctrl+M)")
                    .disabled(!has_sel || is_root || busy)
                    .on_click(cx.listener(|this, _, w, cx| this.move_item(&MoveItem, w, cx))),
            )
            .child(
                tool("trash", IconName::Recycle, "Move to Recycle Bin (Del)")
                    .disabled(!has_sel || is_root || busy)
                    .on_click(cx.listener(|this, _, w, cx| this.trash_item(&TrashItem, w, cx))),
            )
            .child(
                Button::new("delete")
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::Trash).text_color(theme.danger))
                    .tooltip("Delete permanently (Shift+Del)")
                    .disabled(!has_sel || is_root || busy)
                    .on_click(cx.listener(|this, _, w, cx| this.delete_item(&DeleteItem, w, cx))),
            )
            .child(div().flex_1())
            .when_some(self.busy.clone(), |d, label| {
                d.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .max_w(px(420.))
                        .overflow_hidden()
                        .child(Spinner::new().small())
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label)),
                )
            })
            .when(!matches!(self.phase, Phase::Home), |d| {
                d.child(
                    tool("home", IconName::House, "Drive overview")
                        .on_click(cx.listener(|this, _, w, cx| this.go_home(w, cx))),
                )
            })
            .child(
                tool(
                    "theme",
                    if theme.mode.is_dark() { IconName::Sun } else { IconName::Moon },
                    "Toggle light / dark (Ctrl+T)",
                )
                .on_click(cx.listener(|this, _, w, cx| this.toggle_theme(&ToggleTheme, w, cx))),
            )
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let subtitle: Option<SharedString> = match &self.phase {
            Phase::Home => None,
            Phase::Scanning { root, .. } => Some(format!("Scanning {}", root.display()).into()),
            Phase::Ready => self.scan_root.as_ref().map(|r| r.display().to_string().into()),
        };
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .child(img(logo()).size(px(18.)))
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("FastDir"))
                        .when_some(subtitle, |d, s| {
                            d.child(div().text_xs().text_color(theme.muted_foreground).child(s))
                        }),
                )
        )
    }

    fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let cards = self.drives.iter().enumerate().map(|(i, d)| {
            let path = d.path.clone();
            let frac = d.used_fraction();
            let bar = if frac > 0.9 {
                theme.danger
            } else if frac > 0.75 {
                theme.warning
            } else {
                theme.primary
            };
            v_flex()
                .id(("drive", i))
                .w(px(300.))
                .p_4()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .cursor_pointer()
                .hover(|s| s.border_color(theme.primary).bg(theme.secondary_hover))
                .on_click(cx.listener(move |this, _, w, cx| this.start_scan(path.clone(), w, cx)))
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .size(px(40.))
                                .rounded_md()
                                .bg(theme.primary.opacity(0.15))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::HardDrive).text_color(theme.primary)),
                        )
                        .child(
                            v_flex()
                                .overflow_hidden()
                                .child(div().font_weight(FontWeight::SEMIBOLD).whitespace_nowrap().child(d.title()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} · {}", d.kind.label(), if d.fs.is_empty() { "—" } else { &d.fs })),
                                ),
                        ),
                )
                .child(ProgressBar::new(("drive-bar", i)).value(frac * 100.).color(bar))
                .child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} used", format::bytes(d.used())))
                        .child(format!("{} free of {}", format::bytes(d.free), format::bytes(d.total))),
                )
        });
        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .items_center()
            .pt(px(56.))
            .gap_6()
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_2xl().font_weight(FontWeight::BOLD).child("What's using my disk?"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Pick a drive to scan, or choose any folder."),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_4()
                    .max_w(px(960.))
                    .children(cards),
            )
            .child(
                Button::new("scan-folder")
                    .outline()
                    .icon(IconName::FolderSearch)
                    .label("Scan a folder…")
                    .on_click(cx.listener(|this, _, w, cx| this.open_folder(&OpenFolder, w, cx))),
            )
    }

    fn render_scanning(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Phase::Scanning {
            progress,
            root,
            started,
            expected,
        } = &self.phase
        else {
            return div().into_any_element();
        };
        let files = progress.files.load(Ordering::Relaxed);
        let dirs = progress.dirs.load(Ordering::Relaxed);
        let bytes = progress.bytes.load(Ordering::Relaxed);
        let errors = progress.errors.load(Ordering::Relaxed);
        let current = progress.current.lock().clone();
        let elapsed = started.elapsed();
        let rate = files as f64 / elapsed.as_secs_f64().max(0.001);
        let cancelling = progress.cancelled();
        let pct = expected.map(|e| (bytes as f64 / e as f64 * 100.0).min(99.0) as f32);
        let stat = |label: &'static str, value: String| {
            v_flex()
                .w(px(150.))
                .p_3()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(div().text_xs().text_color(theme.muted_foreground).child(label))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(value))
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_5()
            .child(
                h_flex()
                    .gap_3()
                    .child(Spinner::new().large().color(theme.primary))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child(format!("Scanning {}", root.display())),
                    ),
            )
            .child(
                div().w(px(640.)).child(
                    ProgressBar::new("scan-progress")
                        .when_some(pct, |p, v| p.value(v))
                        .when(pct.is_none(), |p| p.loading(true)),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(stat("Files", format::count(files)))
                    .child(stat("Folders", format::count(dirs)))
                    .child(stat("Size", format::bytes(bytes)))
                    .child(stat("Speed", format!("{}/s", format::count(rate as u64))))
                    .child(stat("Elapsed", format::duration(elapsed))),
            )
            .child(
                div()
                    .w(px(760.))
                    .text_xs()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(current),
            )
            .when(errors > 0, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(format!("{} folders could not be read (access denied)", format::count(errors))),
                )
            })
            .child(
                Button::new("stop-scan")
                    .outline()
                    .icon(IconName::CircleStop)
                    .label(if cancelling { "Stopping…" } else { "Stop" })
                    .disabled(cancelling)
                    .on_click(cx.listener(|this, _, w, cx| this.stop_scan(&StopScan, w, cx))),
            )
            .into_any_element()
    }

    fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_resizable("main-split")
            .child(
                resizable_panel().size(px(420.)).size_range(px(120.)..Pixels::MAX).child(
                    h_resizable("top-split")
                        .child(resizable_panel().size_range(px(400.)..Pixels::MAX).child(self.render_list(cx)))
                        .child(
                            resizable_panel()
                                .size(px(330.))
                                .size_range(px(200.)..px(700.))
                                .child(self.render_ext_list(cx)),
                        ),
                ),
            )
            .child(
                resizable_panel()
                    .size_range(px(80.)..Pixels::MAX)
                    .child(self.treemap.clone()),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let left: SharedString = match &self.phase {
            Phase::Home => format!("{} drives detected", self.drives.len()).into(),
            Phase::Scanning { progress, .. } => format!(
                "Scanning… {} files, {}",
                format::count(progress.files.load(Ordering::Relaxed)),
                format::bytes(progress.bytes.load(Ordering::Relaxed))
            )
            .into(),
            Phase::Ready => self
                .selected_fs_path()
                .map(|(p, fs, _)| {
                    let size = self
                        .tree
                        .as_ref()
                        .and_then(|t| t.read().get(&p).map(|n| n.size))
                        .unwrap_or(0);
                    format!("{}  ·  {}", fs.display(), format::bytes(size))
                })
                .unwrap_or_default()
                .into(),
        };
        let right: SharedString = match (&self.phase, &self.tree) {
            (Phase::Ready, Some(t)) => {
                let t = t.read();
                let (elapsed, cancelled) = self.last_scan.unwrap_or_default();
                let drive = self.drives.iter().find(|d| t.root_path.starts_with(&d.path));
                format!(
                    "{} files · {} folders · {}{}{}",
                    format::count(t.root.files as u64),
                    format::count(t.root.dirs as u64),
                    format::bytes(t.root.size),
                    drive
                        .map(|d| format!(" · {} free", format::bytes(d.free)))
                        .unwrap_or_default(),
                    format!(
                        " · scanned in {}{}",
                        format::duration(elapsed),
                        if cancelled { " (incomplete)" } else { "" }
                    )
                )
                .into()
            }
            _ => "".into(),
        };
        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .px_3()
            .gap_4()
            .justify_between()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(left))
            .child(div().flex_shrink_0().child(right))
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
            include_bytes!("../assets/logo.svg").to_vec(),
        ))
    })
    .clone()
}

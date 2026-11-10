//! Opening a scan saved earlier as JSON (`fastdir.scan`).
//!
//! The tree is read on a background thread so a multi-million-node document
//! does not freeze the window. While a saved scan is shown, file operations
//! only run when the paths really exist on this machine.

use super::*;
use fastdir_core::export::{self, ImportedScan};

/// Where a loaded tree came from, so the UI can mark it offline.
#[derive(Clone, Debug)]
pub(super) struct SavedScan {
    /// Unix seconds. `0` when the document does not carry one.
    pub scanned_at: i64,
    /// The saved root folder still exists on this machine.
    pub root_exists: bool,
}

impl FastDir {
    /// `Open scan…`: pick a JSON document and load it.
    pub(super) fn open_scan(&mut self, _: &OpenScan, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let prompt = self.tr("menu.open_scan");
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    this.load_saved_scan(path, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Read `path` on a background thread and show it when it is ready.
    pub(super) fn load_saved_scan(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() {
            return;
        }
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.busy = Some(self.tr("saved.opening").into());
        cx.notify();
        let job = cx.background_executor().spawn(async move {
            let result = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| {
                    export::import_json_with_meta(std::io::BufReader::new(file))
                        .map_err(|e| e.to_string())
                });
            (path, result)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (path, result) = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(scan) => this.show_saved_scan(scan, window, cx),
                    Err(error) => window.push_notification(
                        Notification::error(this.tr_fmt(
                            "saved.err_body",
                            &[("path", &path.display().to_string()), ("error", &error)],
                        ))
                        .title(this.tr("saved.err_title")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replace the current view with an imported scan.
    fn show_saved_scan(&mut self, scan: ImportedScan, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_comparison(cx);
        let ImportedScan { tree, scanned_at } = scan;
        let mut tree = tree;
        // A scan starts with its root row expanded so the top level is visible.
        if let Some(root) = tree.get_mut(&[]) {
            root.set_expanded(true);
        }
        let root = tree.root_path.clone();
        let root_exists = root.is_dir();
        let tree = Arc::new(RwLock::new(tree));
        self.phase = Phase::Ready;
        self.saved = Some(SavedScan {
            scanned_at,
            root_exists,
        });
        self.scan_root = Some(root.clone());
        self.reset_duplicates();
        self.tree = Some(tree.clone());
        self.sync_basket();
        self.home_drives = true;
        self.last_scan = None;
        self.last_errors = 0;
        self.partial_generation = tree.read().generation;
        self.rows.clear();
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.selected_ext = None;
        self.selected_owner = None;
        self.tab = ListTab::Tree;
        self.filter = Filter::default();
        self.filter_form = FilterForm::default();
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.top_files.clear();
        self.top_selected = None;
        self.top_stale = true;
        self.rebuild_rows();
        self.selected = Some(0);
        self.anchor = self.selected_path();
        self.refresh_top_files();
        self.treemap.update(cx, |t, cx| {
            t.set_tree(Some(tree), cx);
            t.set_partial(false, cx);
            t.set_selection(vec![Vec::new()], cx);
        });
        window.set_window_title(
            &self.tr_fmt("saved.title", &[("path", &root.display().to_string())]),
        );
        self.focus.focus(window, cx);
    }

    /// Open a path that arrived from the command line, a second instance or
    /// a file dropped on the window: a folder is scanned, a `.json` file is
    /// loaded as a saved scan.
    pub(super) fn open_path_arg(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match classify_open_path(&path) {
            Some(OpenTarget::Folder(path)) => self.start_scan(path, window, cx),
            Some(OpenTarget::SavedScan(path)) => self.load_saved_scan(path, window, cx),
            None => {}
        }
    }

    /// Handle paths dropped on the window. The first supported one wins.
    pub(super) fn open_dropped(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = paths
            .paths()
            .iter()
            .find(|path| classify_open_path(path).is_some())
        {
            self.open_path_arg(path.clone(), window, cx);
        }
    }

    /// Whether file operations may touch this path. A live scan always can;
    /// a saved scan only when the item still exists on disk.
    pub(super) fn can_touch_path(&self, fs: &std::path::Path) -> bool {
        self.saved.is_none() || fs.exists()
    }

    /// [`Self::can_touch_path`] for the current selection, used to grey out
    /// toolbar buttons and context-menu items.
    pub(super) fn can_touch_selected(&self) -> bool {
        self.saved.is_none()
            || self
                .selected_fs_path()
                .is_some_and(|(_, fs, _)| fs.exists())
    }

    /// True when every target still exists (always true outside saved scans).
    pub(super) fn targets_on_disk(&self, targets: &[Target]) -> bool {
        self.saved.is_none() || targets.iter().all(|t| t.fs.exists())
    }

    /// Warn that a saved scan cannot touch an item that no longer exists.
    pub(super) fn notify_saved_missing(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(Notification::warning(self.tr("saved.missing_item")), cx);
    }

    /// Banner above the results marking a saved scan and its timestamp.
    pub(super) fn render_saved_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let saved = self.saved.as_ref()?;
        let root = self.scan_root.as_ref()?;
        let theme = cx.theme();
        let when = if saved.scanned_at > 0 {
            self.date(saved.scanned_at)
        } else {
            "—".to_string()
        };
        let root_label = self.display_fs_path(root);
        let text = self.tr_fmt("saved.banner", &[("root", &root_label), ("when", &when)]);
        let note = (!saved.root_exists).then(|| self.tr("saved.missing"));
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
                .bg(theme.primary.opacity(0.08))
                .child(
                    v_flex()
                        .flex_1()
                        .overflow_hidden()
                        .gap_1()
                        .child(
                            div()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(text),
                        )
                        .when_some(note, |d, note| {
                            d.child(
                                div()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(theme.warning)
                                    .child(note),
                            )
                        }),
                )
                .child(
                    Button::new("saved-rescan")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(self.tr("saved.rescan"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.rescan(&Rescan, window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// What an argument (or dropped file) asks the GUI to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OpenTarget {
    Folder(PathBuf),
    SavedScan(PathBuf),
}

/// Classify a user-supplied path: a folder is scanned, a `.json` file is
/// loaded as a saved scan. Anything else is ignored.
pub(super) fn classify_open_path(path: &std::path::Path) -> Option<OpenTarget> {
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if is_json && !path.is_dir() {
        Some(OpenTarget::SavedScan(path.to_path_buf()))
    } else if path.is_dir() {
        Some(OpenTarget::Folder(path.to_path_buf()))
    } else {
        None
    }
}

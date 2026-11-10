//! The "Duplicates" tab: finding byte-identical files with
//! `fastdir_core::duplicates`, listing them by group, and freeing the wasted
//! space by recycling the extra copies or replacing them with hard links.
//!
//! The copy to keep is the selected row of a group (the first copy when the
//! group header itself is selected). Everything else in that group is what
//! the two actions act on.

use super::*;
use fastdir_core::duplicates::{self, DuplicateOptions, DuplicateProgress, DuplicateReport};

/// One row of the duplicates list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DupRow {
    /// Header of the group at this index in the report.
    Group(usize),
    /// Copy `file` of group `group`.
    File { group: usize, file: usize },
}

/// Where the duplicate search is.
pub(super) enum DupState {
    /// Never run for this tree.
    Idle,
    Running {
        progress: Arc<DuplicateProgress>,
    },
    Done(DuplicateReport),
}

/// State of the Duplicates tab.
pub(super) struct Duplicates {
    pub state: DupState,
    pub rows: Vec<DupRow>,
    pub selected: Option<usize>,
    pub scroll: UniformListScrollHandle,
    _job: Option<Task<()>>,
    _ticker: Option<Task<()>>,
}

impl Default for Duplicates {
    fn default() -> Self {
        Self {
            state: DupState::Idle,
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _job: None,
            _ticker: None,
        }
    }
}

impl Duplicates {
    pub(super) fn report(&self) -> Option<&DuplicateReport> {
        match &self.state {
            DupState::Done(report) => Some(report),
            _ => None,
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if let DupState::Done(report) = &self.state {
            for (group, g) in report.groups.iter().enumerate() {
                rows.push(DupRow::Group(group));
                rows.extend((0..g.files.len()).map(|file| DupRow::File { group, file }));
            }
        }
        self.rows = rows;
        self.selected = match self.selected {
            _ if self.rows.is_empty() => None,
            Some(ix) => Some(ix.min(self.rows.len() - 1)),
            None => None,
        };
    }

    /// The group and copy that the actions keep.
    pub(super) fn keep(&self) -> Option<(usize, usize)> {
        match self.rows.get(self.selected?)? {
            DupRow::Group(group) => Some((*group, 0)),
            DupRow::File { group, file } => Some((*group, *file)),
        }
    }
}

/// What to do with the copies that are not kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Dedupe {
    Recycle,
    HardLink,
}

impl FastDir {
    /// Forget the results: the tree they describe is gone.
    pub(super) fn reset_duplicates(&mut self) {
        if let DupState::Running { progress } = &self.dups.state {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.dups = Duplicates::default();
        if self.tab == ListTab::Duplicates {
            self.tab = ListTab::Tree;
        }
    }

    /// Hash the files of the current scan in the background.
    pub(super) fn start_duplicate_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Ready)
            || matches!(self.dups.state, DupState::Running { .. })
        {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let progress = Arc::new(DuplicateProgress::default());
        self.dups.state = DupState::Running {
            progress: progress.clone(),
        };
        self.dups.rows.clear();
        self.dups.selected = None;
        let options = DuplicateOptions::default();
        let source = tree.clone();
        let job_progress = progress.clone();
        let job = cx.background_executor().spawn(async move {
            // Only hold the tree lock while listing candidates; hashing can
            // take minutes and must not block file operations.
            let candidates = {
                let tree = source.read();
                duplicates::collect(&tree, &[], &options)
            };
            duplicates::find_candidates(candidates, &options, job_progress)
        });
        self.dups._job = Some(cx.spawn_in(window, async move |this, cx| {
            let report = job.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_duplicate_search(report, &tree, &progress, window, cx)
            })
            .ok();
        }));
        self.dups._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.dups.state, DupState::Running { .. })
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn finish_duplicate_search(
        &mut self,
        report: DuplicateReport,
        tree: &Arc<RwLock<Tree>>,
        progress: &Arc<DuplicateProgress>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A newer search, a reset or a different tree replaced this one.
        let current = matches!(&self.dups.state,
            DupState::Running { progress: p } if Arc::ptr_eq(p, progress));
        let same_tree = self.tree.as_ref().is_some_and(|t| Arc::ptr_eq(t, tree));
        if !current || !same_tree {
            return;
        }
        self.dups._ticker = None;
        if report.cancelled {
            self.dups.state = DupState::Idle;
            cx.notify();
            return;
        }
        let errors = report.hash_errors;
        self.dups.state = DupState::Done(report);
        self.dups.selected = Some(0);
        // Files may have been deleted or moved while hashing.
        self.prune_duplicates();
        self.dups.scroll.scroll_to_item(0, ScrollStrategy::Top);
        if self.tab == ListTab::Duplicates {
            self.sync_treemap_selection(cx);
        }
        if errors > 0 {
            window.push_notification(
                Notification::warning(
                    self.tr_fmt("dups.unreadable", &[("n", &self.count(errors))]),
                ),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn cancel_duplicate_search(&mut self, cx: &mut Context<Self>) {
        if let DupState::Running { progress } = &self.dups.state {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// Drop copies that left the tree (deleted, moved) or became hard links,
    /// and groups left with a single copy. Called whenever the tree changes.
    pub(super) fn prune_duplicates(&mut self) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let keep_path = self.dups.keep().and_then(|(group, file)| {
            self.dups
                .report()?
                .groups
                .get(group)?
                .files
                .get(file)
                .map(|f| f.path.clone())
        });
        let DupState::Done(report) = &mut self.dups.state else {
            return;
        };
        {
            let tree = tree.read();
            report.retain_files(|file| {
                tree.find(&file.path)
                    .and_then(|path| tree.get(&path).map(|node| !node.hard_link()))
                    .unwrap_or(false)
            });
        }
        self.dups.rebuild_rows();
        // Follow the selected copy to its new row.
        if let Some(path) = keep_path
            && let DupState::Done(report) = &self.dups.state
        {
            let found = self.dups.rows.iter().position(|row| match row {
                DupRow::File { group, file } => report.groups[*group].files[*file].path == path,
                DupRow::Group(_) => false,
            });
            if found.is_some() {
                self.dups.selected = found;
            }
        }
    }

    /// The node of the selected copy, for the treemap and file operations.
    pub(super) fn selected_dup_path(&self) -> Option<NodePath> {
        let report = self.dups.report()?;
        let DupRow::File { group, file } = *self.dups.rows.get(self.dups.selected?)? else {
            return None;
        };
        let fs = &report.groups.get(group)?.files.get(file)?.path;
        // Recursive: `targets()` already holds a read lock when it asks for
        // the selection, and a queued writer would otherwise deadlock us.
        self.tree.as_ref()?.read_recursive().find(fs)
    }

    pub(super) fn select_dup_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.dups.rows.is_empty() {
            return;
        }
        let ix = ix.min(self.dups.rows.len() - 1);
        self.dups.selected = Some(ix);
        self.leave_treemap(cx);
        self.marked.clear();
        self.anchor = None;
        self.dups.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    /// Enter on a copy: show it in the directory tree.
    pub(super) fn reveal_dup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected_dup_path() else {
            return;
        };
        if self.filter.is_active() {
            self.clear_filters(window, cx);
        }
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.reveal_in_list(path, ClickMode::Plain, cx);
        self.focus.focus(window, cx);
    }

    /// The kept copy and the other copies of the selected group as targets.
    fn dedupe_targets(&self) -> Option<(PathBuf, Vec<Target>)> {
        let (group, keep) = self.dups.keep()?;
        let group = self.dups.report()?.groups.get(group)?;
        let tree = self.tree.as_ref()?.read();
        let keep_path = group.files.get(keep)?.path.clone();
        let targets: Vec<Target> = group
            .files
            .iter()
            .enumerate()
            .filter(|(ix, _)| *ix != keep)
            .filter_map(|(_, file)| {
                let path = tree.find(&file.path)?;
                let node = tree.get(&path)?;
                Some(Target {
                    path,
                    fs: file.path.clone(),
                    is_dir: false,
                    size: node.size,
                    items: 1,
                })
            })
            .collect();
        (!targets.is_empty()).then_some((keep_path, targets))
    }

    /// Ask, then recycle or hard-link the copies the selection does not keep.
    pub(super) fn confirm_dedupe(
        &mut self,
        action: Dedupe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_refreshing() {
            return;
        }
        let Some((keep, targets)) = self.dedupe_targets() else {
            return;
        };
        if !self.targets_on_disk(&targets) || (self.saved.is_some() && !keep.exists()) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let n = self.count(targets.len() as u64);
        let size = self.bytes(targets.iter().map(|t| t.size).sum());
        let keep_text = keep.display().to_string();
        let trash = self.trash_name();
        let (title, description, ok): (SharedString, SharedString, SharedString) = match action {
            Dedupe::Recycle => (
                self.tr_fmt("dups.confirm_recycle_title", &[("n", &n), ("trash", trash)])
                    .into(),
                self.tr_fmt(
                    "dups.confirm_recycle",
                    &[
                        ("keep", &keep_text),
                        ("n", &n),
                        ("size", &size),
                        ("trash", trash),
                    ],
                )
                .into(),
                self.tr_fmt("dlg.title_recycle", &[("trash", trash)]).into(),
            ),
            Dedupe::HardLink => (
                self.tr_fmt("dups.confirm_link_title", &[("n", &n)]).into(),
                self.tr_fmt(
                    "dups.confirm_link",
                    &[("keep", &keep_text), ("n", &n), ("size", &size)],
                )
                .into(),
                self.tr("dups.ok_link").into(),
            ),
        };
        let cancel = self.tr("action.cancel");
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let targets = targets.clone();
            let keep = keep.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok.clone())
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(cancel),
                )
                .on_ok(move |_, window, cx| {
                    let targets = targets.clone();
                    let keep = keep.clone();
                    entity
                        .update(cx, |this, cx| match action {
                            Dedupe::Recycle => this.run_delete(targets, false, window, cx),
                            Dedupe::HardLink => this.run_hard_link(keep, targets, window, cx),
                        })
                        .ok();
                    true
                })
        });
    }

    fn run_hard_link(
        &mut self,
        keep: PathBuf,
        targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self
            .tr_fmt(
                "dups.op_linking",
                &[("n", &self.count(targets.len() as u64))],
            )
            .into();
        self.enqueue(targets, PendingKind::HardLink { keep }, label, window, cx);
    }

    /// Replace each target with a hard link to `keep`, then turn the
    /// replaced nodes into hard-link nodes in place (no rescan).
    pub(super) fn start_hard_link(
        &mut self,
        targets: Vec<Target>,
        keep: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self.tr_fmt(
            "dups.op_linking",
            &[("n", &self.count(targets.len() as u64))],
        );
        let progress = self.begin_op(
            label,
            targets.len() as u64,
            targets.iter().map(|t| t.size).sum(),
            false,
            cx,
        );
        let job = cx.background_executor().spawn(async move {
            let (mut items, mut bytes) = (0, 0);
            let mut done: Vec<(PathBuf, u64)> = Vec::new();
            let mut failed: Vec<(PathBuf, String)> = Vec::new();
            for t in targets {
                if progress.is_cancelled() {
                    break;
                }
                match fsops::replace_with_hard_link(&keep, &t.fs) {
                    Ok(()) => done.push((t.fs, t.size)),
                    Err(error) => failed.push((t.fs, error)),
                }
                items += 1;
                bytes += t.size;
                progress.reach(items, bytes);
            }
            (done, failed)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (done, failed) = job.await;
            this.update_in(cx, |this, window, cx| {
                this.end_op();
                let keep_fs = this
                    .selected_tree_path()
                    .and_then(|p| this.tree.as_ref()?.read().fs_path(&p));
                let mut freed = 0u64;
                for (fs, size) in &done {
                    if this.mark_hard_link(fs) {
                        freed += size;
                    }
                }
                if !done.is_empty() {
                    match keep_fs {
                        Some(fs) => this.tree_changed_keeping_selection(&fs, None, cx),
                        None => this.tree_changed(None, cx),
                    }
                    let msg = this.tr_fmt(
                        "dups.done_link",
                        &[
                            ("n", &this.count(done.len() as u64)),
                            ("size", &this.bytes(freed)),
                        ],
                    );
                    window.push_notification(Notification::success(msg), cx);
                }
                if !failed.is_empty() {
                    let lang = this.settings.locale.language.clone();
                    window.push_notification(
                        Notification::error(dialogs::failure_list_in(&failed, &lang)).title(
                            this.tr_fmt(
                                "dups.link_failed",
                                &[("n", &this.count(failed.len() as u64))],
                            ),
                        ),
                        cx,
                    );
                }
                this.start_next_op(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Turn the file node at `fs` into a hard link that adds no bytes.
    /// Returns whether the node was found.
    fn mark_hard_link(&mut self, fs: &std::path::Path) -> bool {
        let Some(tree) = self.tree.as_ref() else {
            return false;
        };
        let mut t = tree.write();
        let Some(path) = t.find(fs) else {
            return false;
        };
        let Some(node) = t.get(&path).copied() else {
            return false;
        };
        if node.is_dir() || node.hard_link() {
            return false;
        }
        let name = t.name(&node).to_string();
        let parent: NodePath = path[..path.len() - 1].to_vec();
        // `insert` replaces the child with the same name.
        t.insert(&parent, Subtree::hard_link(&name, node.modified()))
            .is_some()
    }

    pub(super) fn duplicates_tab_label(&self) -> String {
        match self.dups.report() {
            Some(report) => self.tr_fmt(
                "list.dups_tab_n",
                &[("n", &self.count(report.groups.len() as u64))],
            ),
            None => self.tr("list.dups_tab").to_string(),
        }
    }

    pub(super) fn render_duplicates_table(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let centered = |content: AnyElement| {
            div()
                .flex_1()
                .min_h_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .child(content)
                .into_any_element()
        };
        let can_search = matches!(self.phase, Phase::Ready) && self.tree.is_some();
        let search_button = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Button::new(id)
                .small()
                .primary()
                .icon(IconName::Search)
                .label(label)
                .disabled(!can_search)
                .on_click(
                    cx.listener(|this, _, window, cx| this.start_duplicate_search(window, cx)),
                )
        };
        match &self.dups.state {
            DupState::Idle => centered(
                v_flex()
                    .max_w(sp(420.))
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(div().text_center().child(self.tr_fmt(
                        "dups.intro",
                        &[("size", &self.bytes(duplicates::DEFAULT_MIN_SIZE))],
                    )))
                    .child(search_button("dups-find", self.tr("dups.find"), cx))
                    .into_any_element(),
            ),
            DupState::Running { progress } => {
                let files = progress.files_hashed.load(Ordering::Relaxed);
                let read = progress.bytes_hashed.load(Ordering::Relaxed);
                let current = progress.current.lock().clone();
                centered(
                    v_flex()
                        .w_full()
                        .max_w(sp(520.))
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(Spinner::new().color(theme.primary))
                        .child(self.tr_fmt(
                            "dups.hashing",
                            &[("files", &self.count(files)), ("bytes", &self.bytes(read))],
                        ))
                        .child(
                            div()
                                .w_full()
                                .text_center()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(current),
                        )
                        .child(
                            Button::new("dups-cancel")
                                .small()
                                .outline()
                                .label(self.tr("action.cancel"))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_duplicate_search(cx)),
                                ),
                        )
                        .into_any_element(),
                )
            }
            DupState::Done(report) if report.groups.is_empty() => centered(
                v_flex()
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(self.tr("dups.none"))
                    .child(search_button("dups-again", self.tr("dups.again"), cx))
                    .into_any_element(),
            ),
            DupState::Done(report) => {
                let summary = self.tr_fmt(
                    "dups.summary",
                    &[
                        ("groups", &self.count(report.groups.len() as u64)),
                        ("wasted", &self.bytes(report.total_wasted)),
                    ],
                );
                let can_act = self.dups.keep().is_some() && !self.is_refreshing();
                let toolbar = h_flex()
                    .flex_shrink_0()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(summary),
                    )
                    .child(
                        Button::new("dups-recycle")
                            .small()
                            .outline()
                            .icon(IconName::Delete)
                            .label(self.tr_fmt("dups.recycle", &[("trash", self.trash_name())]))
                            .tooltip(self.tr("dups.keep_hint"))
                            .disabled(!can_act)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_dedupe(Dedupe::Recycle, window, cx)
                            })),
                    )
                    .child(
                        Button::new("dups-link")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label(self.tr("dups.link"))
                            .tooltip(self.tr("dups.keep_hint"))
                            .disabled(!can_act)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_dedupe(Dedupe::HardLink, window, cx)
                            })),
                    )
                    .child(
                        Button::new("dups-refresh")
                            .small()
                            .ghost()
                            .icon(IconName::Redo)
                            .tooltip(self.tr("dups.again"))
                            .disabled(!can_search)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_duplicate_search(window, cx)
                            })),
                    );
                let header = h_flex()
                    .h(sp(28.))
                    .w_full()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w(sp(220.))
                            .px_2()
                            .child(self.tr("list.col_name")),
                    )
                    .child(
                        div()
                            .w(sp(COL_SIZE))
                            .flex_shrink_0()
                            .px_2()
                            .text_right()
                            .child(self.tr("list.col_size")),
                    )
                    .child(
                        div()
                            .w(sp(COL_DATE))
                            .flex_shrink_0()
                            .px_2()
                            .text_right()
                            .child(self.tr("list.col_date")),
                    );
                let entity = cx.entity();
                let body = div()
                    .id("dups-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        uniform_list(
                            "dups-rows",
                            self.dups.rows.len(),
                            cx.processor(Self::render_duplicate_rows),
                        )
                        .size_full()
                        .track_scroll(&self.dups.scroll),
                    )
                    .child(Scrollbar::vertical(&self.dups.scroll))
                    .context_menu(move |menu, window, cx| {
                        actions_ui::build_context_menu(entity.clone(), menu, window, cx)
                    });
                v_flex()
                    .size_full()
                    .child(toolbar)
                    .child(header)
                    .child(body)
                    .into_any_element()
            }
        }
    }

    fn render_duplicate_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(report) = self.dups.report() else {
            return Vec::new();
        };
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let head = theme.list_head;
        let selected = self.dups.selected;
        let keep = self.dups.keep();
        let num = |w: f32, s: String| {
            div()
                .w(sp(w))
                .flex_shrink_0()
                .px_2()
                .text_right()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(s)
        };
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = self.dups.rows.get(ix).copied() else {
                continue;
            };
            let is_sel = selected == Some(ix);
            let base = h_flex()
                .id(("dups-row", ix))
                .h(sp(ROW_H))
                .w_full()
                .text_xs()
                .text_color(fg)
                .when(is_sel, |d| {
                    d.bg(active).border_l_2().border_color(active_border)
                })
                .when(!is_sel, |d| {
                    d.border_l_2()
                        .border_color(transparent_black())
                        .hover(|s| s.bg(hover))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.focus.focus(window, cx);
                        this.select_dup_row(ix, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| this.select_dup_row(ix, cx)),
                );
            let element = match row {
                DupRow::Group(group) => {
                    let Some(g) = report.groups.get(group) else {
                        continue;
                    };
                    let name = g.files[0]
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    base.when(!is_sel, |d| d.bg(head))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(sp(220.))
                                .overflow_hidden()
                                .gap_2()
                                .pl_2()
                                .child(Icon::new(IconName::Copy).xsmall().text_color(primary))
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(SharedString::from(name)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(muted)
                                        .child(self.tr_fmt(
                                            "dups.group",
                                            &[
                                                ("n", &self.count(g.files.len() as u64)),
                                                ("wasted", &self.bytes(g.wasted())),
                                            ],
                                        )),
                                ),
                        )
                        .child(num(COL_SIZE, self.bytes(g.size)))
                        .child(num(COL_DATE, String::new()))
                }
                DupRow::File { group, file } => {
                    let Some(f) = report.groups.get(group).and_then(|g| g.files.get(file)) else {
                        continue;
                    };
                    let size = report.groups[group].size;
                    let kept = keep == Some((group, file));
                    base.child(
                        h_flex()
                            .flex_1()
                            .min_w(sp(220.))
                            .overflow_hidden()
                            .gap_2()
                            .pl(sp(26.))
                            .child(Icon::new(IconName::File).xsmall().text_color(muted))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(SharedString::from(self.display_fs_path(&f.path))),
                            )
                            .when(kept, |d| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(primary.opacity(0.15))
                                        .text_color(primary)
                                        .child(self.tr("dups.keep")),
                                )
                            }),
                    )
                    .child(num(COL_SIZE, self.bytes(size)))
                    .child(num(COL_DATE, self.date(f.modified)).text_color(muted))
                }
            };
            out.push(element.into_any_element());
        }
        out
    }
}

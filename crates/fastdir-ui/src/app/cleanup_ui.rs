//! The "Cleanup" tab: well-known places that hold reclaimable space (temp
//! folders, browser caches, `node_modules` and `target` folders, Windows
//! Update files, `Windows.old`, stale downloads), found with
//! `fastdir_core::cleanup` and freed through the regular delete operations.
//!
//! Every action asks for confirmation first, whatever the "confirm" settings
//! say: a suggestion can stand for thousands of files.

use super::*;
use fastdir_core::cleanup::{self, CleanupKind, CleanupOptions, KnownLocations, Suggestion};

/// Where the search for suggestions is.
pub(super) enum CleanupState {
    /// Not looked for yet in this tree.
    Idle,
    Running,
    Done(Vec<Suggestion>),
}

/// State of the Cleanup tab.
pub(super) struct Cleanup {
    pub state: CleanupState,
    pub selected: Option<usize>,
    pub scroll: UniformListScrollHandle,
    _job: Option<Task<()>>,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            state: CleanupState::Idle,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _job: None,
        }
    }
}

impl Cleanup {
    pub(super) fn suggestions(&self) -> Option<&[Suggestion]> {
        match &self.state {
            CleanupState::Done(list) => Some(list),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        self.suggestions().map_or(0, <[_]>::len)
    }

    fn selected_suggestion(&self) -> Option<&Suggestion> {
        self.suggestions()?.get(self.selected?)
    }
}

impl FastDir {
    /// Forget the suggestions: the tree they describe is gone.
    pub(super) fn reset_cleanup(&mut self) {
        self.cleanup = Cleanup::default();
        if self.tab == ListTab::Cleanup {
            self.tab = ListTab::Tree;
        }
    }

    /// Look for reclaimable places in the current scan, in the background.
    pub(super) fn start_cleanup_search(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Ready)
            || matches!(self.cleanup.state, CleanupState::Running)
        {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        self.cleanup.state = CleanupState::Running;
        self.cleanup.selected = None;
        let source = tree.clone();
        let job = cx.background_executor().spawn(async move {
            let known = KnownLocations::detect();
            let tree = source.read();
            cleanup::suggest(&tree, &known, &CleanupOptions::default())
        });
        self.cleanup._job = Some(cx.spawn(async move |this, cx| {
            let list = job.await;
            this.update(cx, |this, cx| this.finish_cleanup_search(list, &tree, cx))
                .ok();
        }));
        cx.notify();
    }

    fn finish_cleanup_search(
        &mut self,
        list: Vec<Suggestion>,
        tree: &Arc<RwLock<Tree>>,
        cx: &mut Context<Self>,
    ) {
        // A reset or a different tree replaced the one this was computed for.
        let same_tree = self.tree.as_ref().is_some_and(|t| Arc::ptr_eq(t, tree));
        if !matches!(self.cleanup.state, CleanupState::Running) || !same_tree {
            return;
        }
        self.cleanup.selected = (!list.is_empty()).then_some(0);
        self.cleanup.state = CleanupState::Done(list);
        // Files may have been deleted while looking.
        self.prune_cleanup();
        self.cleanup.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Drop targets that left the tree and refresh the sizes of the rest.
    /// Called whenever the tree changes.
    pub(super) fn prune_cleanup(&mut self) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let CleanupState::Done(list) = &mut self.cleanup.state else {
            return;
        };
        let selected_location = self
            .cleanup
            .selected
            .and_then(|ix| list.get(ix))
            .map(|s| (s.kind, s.location.clone()));
        {
            let tree = tree.read();
            for suggestion in list.iter_mut() {
                suggestion.retain_targets(|target| {
                    let Some(node) = tree
                        .find(&target.path)
                        .and_then(|path| tree.get(&path).copied())
                    else {
                        return false;
                    };
                    target.size = node.size;
                    target.items = if node.is_dir() { node.items() + 1 } else { 1 };
                    true
                });
            }
        }
        list.retain(|s| s.size > 0);
        list.sort_by(|a, b| {
            b.size
                .cmp(&a.size)
                .then_with(|| a.location.cmp(&b.location))
        });
        // Follow the selected suggestion to its new row.
        self.cleanup.selected = match selected_location {
            _ if list.is_empty() => None,
            Some((kind, location)) => list
                .iter()
                .position(|s| s.kind == kind && s.location == location)
                .or(Some(self.cleanup.selected.unwrap_or(0).min(list.len() - 1))),
            None => None,
        };
    }

    pub(super) fn select_cleanup_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let len = self.cleanup.len();
        if len == 0 {
            return;
        }
        let ix = ix.min(len - 1);
        self.cleanup.selected = Some(ix);
        self.leave_treemap(cx);
        self.marked.clear();
        self.anchor = None;
        self.cleanup.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    /// Enter on a suggestion: show its folder in the directory tree.
    pub(super) fn reveal_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(location) = self
            .cleanup
            .selected_suggestion()
            .map(|s| s.location.clone())
        else {
            return;
        };
        let Some(path) = self
            .tree
            .as_ref()
            .and_then(|tree| tree.read().find(&location))
        else {
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

    /// What the selected suggestion would delete, as file-operation targets.
    fn cleanup_targets(&self) -> Option<(&Suggestion, Vec<Target>)> {
        let suggestion = self.cleanup.selected_suggestion()?;
        let tree = self.tree.as_ref()?.read();
        let targets: Vec<Target> = suggestion
            .targets
            .iter()
            .filter_map(|target| {
                let path = tree.find(&target.path)?;
                let node = tree.get(&path)?;
                // Never the scan root, whatever the suggestion says.
                if path.is_empty() || node.is_special() {
                    return None;
                }
                Some(Target {
                    path,
                    fs: target.path.clone(),
                    is_dir: node.is_dir(),
                    size: node.size,
                    items: if node.is_dir() { node.items() + 1 } else { 1 },
                })
            })
            .collect();
        (!targets.is_empty()).then_some((suggestion, targets))
    }

    /// Ask, then recycle or permanently delete what the selected suggestion
    /// lists.
    pub(super) fn confirm_cleanup(
        &mut self,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_refreshing() {
            return;
        }
        let Some((suggestion, targets)) = self.cleanup_targets() else {
            return;
        };
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let items = self.count(targets.iter().map(|t| t.items).sum());
        let size = self.bytes(targets.iter().map(|t| t.size).sum());
        let location = self.display_fs_path(&suggestion.location);
        let trash = self.trash_name();
        let mut description = if permanent {
            self.tr_fmt(
                "cleanup.confirm_delete",
                &[("n", &items), ("size", &size), ("location", &location)],
            )
        } else {
            self.tr_fmt(
                "cleanup.confirm_recycle",
                &[
                    ("n", &items),
                    ("size", &size),
                    ("location", &location),
                    ("trash", trash),
                ],
            )
        };
        if suggestion.kind == CleanupKind::WindowsOld {
            description.push_str("\n\n");
            description.push_str(self.tr("cleanup.windows_old_note"));
        }
        let title: SharedString = if permanent {
            self.tr("dlg.title_perm").into()
        } else {
            self.tr_fmt("dlg.title_recycle", &[("trash", trash)]).into()
        };
        let description: SharedString = description.into();
        let ok: SharedString = if permanent {
            self.tr("dlg.ok_delete").into()
        } else {
            self.tr_fmt("dlg.title_recycle", &[("trash", trash)]).into()
        };
        let cancel = self.tr("action.cancel");
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let targets = targets.clone();
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
                    entity
                        .update(cx, |this, cx| {
                            this.run_delete(targets, permanent, window, cx)
                        })
                        .ok();
                    true
                })
        });
    }

    pub(super) fn cleanup_tab_label(&self) -> String {
        match self.cleanup.suggestions() {
            Some(list) => self.tr_fmt(
                "list.cleanup_tab_n",
                &[("n", &self.count(list.len() as u64))],
            ),
            None => self.tr("list.cleanup_tab").to_string(),
        }
    }

    fn cleanup_kind_label(&self, kind: CleanupKind) -> &'static str {
        self.tr(match kind {
            CleanupKind::Temp => "cleanup.kind.temp",
            CleanupKind::BrowserCache => "cleanup.kind.browser_cache",
            CleanupKind::NodeModules => "cleanup.kind.node_modules",
            CleanupKind::BuildTarget => "cleanup.kind.build_target",
            CleanupKind::WindowsUpdate => "cleanup.kind.windows_update",
            CleanupKind::WindowsOld => "cleanup.kind.windows_old",
            CleanupKind::StaleDownloads => "cleanup.kind.stale_downloads",
        })
    }

    pub(super) fn render_cleanup_table(&self, cx: &mut Context<Self>) -> AnyElement {
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
                .on_click(cx.listener(|this, _, _, cx| this.start_cleanup_search(cx)))
        };
        let list = match &self.cleanup.state {
            CleanupState::Idle => {
                return centered(
                    v_flex()
                        .max_w(sp(420.))
                        .gap_3()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(div().text_center().child(self.tr("cleanup.intro")))
                        .child(search_button("cleanup-find", self.tr("cleanup.find"), cx))
                        .into_any_element(),
                );
            }
            CleanupState::Running => {
                return centered(
                    v_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(Spinner::new().color(theme.primary))
                        .child(self.tr("cleanup.searching"))
                        .into_any_element(),
                );
            }
            CleanupState::Done(list) if list.is_empty() => {
                return centered(
                    v_flex()
                        .gap_3()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(self.tr("cleanup.none"))
                        .child(search_button("cleanup-again", self.tr("cleanup.again"), cx))
                        .into_any_element(),
                );
            }
            CleanupState::Done(list) => list,
        };
        let total: u64 = list.iter().map(|s| s.size).sum();
        let summary = self.tr_fmt(
            "cleanup.summary",
            &[
                ("n", &self.count(list.len() as u64)),
                ("size", &self.bytes(total)),
            ],
        );
        let can_act = self.cleanup.selected_suggestion().is_some() && !self.is_refreshing();
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
                Button::new("cleanup-recycle")
                    .small()
                    .outline()
                    .icon(IconName::Recycle)
                    .label(self.tr_fmt("cleanup.recycle", &[("trash", self.trash_name())]))
                    .tooltip(self.tr("cleanup.hint"))
                    .disabled(!can_act)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_cleanup(false, window, cx)),
                    ),
            )
            .child(
                Button::new("cleanup-delete")
                    .small()
                    .outline()
                    .icon(IconName::Trash)
                    .label(self.tr("cleanup.delete"))
                    .tooltip(self.tr("cleanup.hint"))
                    .disabled(!can_act)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_cleanup(true, window, cx)),
                    ),
            )
            .child(
                Button::new("cleanup-refresh")
                    .small()
                    .ghost()
                    .icon(IconName::Redo)
                    .tooltip(self.tr("cleanup.again"))
                    .disabled(!can_search)
                    .on_click(cx.listener(|this, _, _, cx| this.start_cleanup_search(cx))),
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
                    .w(sp(COL_SIZE))
                    .flex_shrink_0()
                    .px_2()
                    .text_right()
                    .child(self.tr("list.col_items")),
            );
        let body = div()
            .id("cleanup-body")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                uniform_list(
                    "cleanup-rows",
                    list.len(),
                    cx.processor(Self::render_cleanup_rows),
                )
                .size_full()
                .track_scroll(&self.cleanup.scroll),
            )
            .child(Scrollbar::vertical(&self.cleanup.scroll));
        v_flex()
            .size_full()
            .child(toolbar)
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_cleanup_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(list) = self.cleanup.suggestions() else {
            return Vec::new();
        };
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let selected = self.cleanup.selected;
        let num = |s: String| {
            div()
                .w(sp(COL_SIZE))
                .flex_shrink_0()
                .px_2()
                .text_right()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(s)
        };
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(s) = list.get(ix) else {
                continue;
            };
            let is_sel = selected == Some(ix);
            let icon = match s.kind {
                CleanupKind::Temp | CleanupKind::BrowserCache | CleanupKind::WindowsUpdate => {
                    IconName::Recycle
                }
                _ => IconName::Folder,
            };
            let row = h_flex()
                .id(("cleanup-row", ix))
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
                        this.select_cleanup_row(ix, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| this.select_cleanup_row(ix, cx)),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w(sp(220.))
                        .overflow_hidden()
                        .gap_2()
                        .pl_2()
                        .child(Icon::new(icon).xsmall().text_color(primary))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.cleanup_kind_label(s.kind)),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(muted)
                                .child(SharedString::from(self.display_fs_path(&s.location))),
                        ),
                )
                .child(num(self.bytes(s.size)))
                .child(num(self.count(s.items)).text_color(muted));
            out.push(row.into_any_element());
        }
        out
    }
}

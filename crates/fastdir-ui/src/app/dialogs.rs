//! File operations and their confirmation / rename dialogs.

use super::*;

impl FastDir {
    pub(super) fn selected_fs_path(&self) -> Option<(NodePath, PathBuf, bool)> {
        let path = self.selected_path()?;
        let tree = self.tree.as_ref()?.read();
        let fs = tree.fs_path(&path)?;
        let is_dir = tree.get(&path)?.is_dir();
        Some((path, fs, is_dir))
    }

    pub(super) fn open_selected(&mut self, _: &OpenItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_multi() || self.selected_is_special() {
            return;
        }
        if let Some((_, p, _)) = self.selected_fs_path() {
            if !self.can_touch_path(&p) {
                self.notify_saved_missing(window, cx);
                return;
            }
            fsops::open(&p);
        }
    }

    pub(super) fn reveal_selected(
        &mut self,
        _: &RevealItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_multi() || self.selected_is_special() {
            return;
        }
        if let Some((_, p, _)) = self.selected_fs_path() {
            if !self.can_touch_path(&p) {
                self.notify_saved_missing(window, cx);
                return;
            }
            fsops::reveal(&p);
        }
    }

    pub(super) fn copy_path(&mut self, _: &CopyPath, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let text = targets
            .iter()
            .map(|t| t.fs.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification(
            if targets.len() > 1 {
                self.tr("dlg.copied_many")
            } else {
                self.tr("dlg.copied_one")
            },
            cx,
        );
    }

    pub(super) fn copy_name(&mut self, _: &CopyName, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let text = targets
            .iter()
            .map(|t| {
                t.fs
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| t.fs.display().to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification(
            if targets.len() > 1 {
                self.tr("dlg.copied_many")
            } else {
                self.tr("dlg.copied_one")
            },
            cx,
        );
    }

    pub(super) fn trash_item(
        &mut self,
        _: &TrashItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Delete performs the configured default action.
        let permanent = self.settings.ops.default_delete == DeleteAction::Permanent;
        self.confirm_delete(permanent, window, cx);
    }

    pub(super) fn delete_item(
        &mut self,
        _: &DeleteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Shift+Delete always does the opposite of the Delete key.
        let permanent = self.settings.ops.default_delete == DeleteAction::Recycle;
        self.confirm_delete(permanent, window, cx);
    }

    pub(super) fn confirm_delete(
        &mut self,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_refreshing() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        if targets.iter().any(|t| t.path.is_empty()) {
            window.push_notification(
                (
                    NotificationType::Warning,
                    self.tr("dlg.root_note"),
                ),
                cx,
            );
            return;
        }
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        // Per-action confirmations; skip the dialog when this action is set
        // to apply immediately.
        let needs_confirm = if permanent {
            self.settings.ops.confirm_permanent
        } else {
            self.settings.ops.confirm_recycle
        };
        if !needs_confirm {
            self.run_delete(targets, permanent, window, cx);
            return;
        }
        let what = if let [t] = targets.as_slice() {
            if t.is_dir {
                let items = self
                    .tree
                    .as_ref()
                    .and_then(|tree| tree.read().get(&t.path).map(Node::items))
                    .unwrap_or(0);
                self.tr_fmt(
                    "dlg.what_folder",
                    &[
                        ("name", &t.fs.display().to_string()),
                        ("size", &self.bytes(t.size)),
                        ("items", &self.count(items)),
                    ],
                )
            } else {
                self.tr_fmt(
                    "dlg.what_file",
                    &[
                        ("name", &t.fs.display().to_string()),
                        ("size", &self.bytes(t.size)),
                    ],
                )
            }
        } else {
            let folders = targets.iter().filter(|t| t.is_dir).count();
            self.tr_fmt(
                "dlg.what_multi",
                &[
                    ("n", &targets.len().to_string()),
                    ("dirs", &folders.to_string()),
                    ("files", &(targets.len() - folders).to_string()),
                    ("size", &self.bytes(targets.iter().map(|t| t.size).sum())),
                ],
            )
        };
        let description: SharedString = if permanent {
            self.tr_fmt("dlg.confirm_perm", &[("what", &what)]).into()
        } else {
            self.tr_fmt(
                "dlg.confirm_recycle",
                &[("what", &what), ("trash", self.trash_name())],
            )
            .into()
        };
        let title: SharedString = if permanent {
            self.tr("dlg.title_perm").into()
        } else {
            self.tr_fmt("dlg.title_recycle", &[("trash", self.trash_name())])
                .into()
        };
        let ok_delete: SharedString = self.tr("dlg.ok_delete").into();
        let ok_recycle: SharedString = self
            .tr_fmt("dlg.title_recycle", &[("trash", self.trash_name())])
            .into();
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
                        .ok_text(if permanent {
                            ok_delete.clone()
                        } else {
                            ok_recycle.clone()
                        })
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

    /// Queue a delete; it starts right away when nothing else is running.
    pub(super) fn run_delete(
        &mut self,
        targets: Vec<Target>,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The confirmation dialog may outlive the item on disk.
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let label = self.delete_label(&targets, permanent).into();
        let kind = PendingKind::Delete { permanent };
        self.enqueue(targets, kind, label, window, cx);
    }

    /// Queue a move; it starts right away when nothing else is running.
    pub(super) fn run_move(
        &mut self,
        srcs: Vec<Target>,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.targets_on_disk(&srcs) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let label = self.move_label(&srcs, &dest_dir).into();
        let kind = PendingKind::Move { dest: dest_dir };
        self.enqueue(srcs, kind, label, window, cx);
    }

    pub(super) fn enqueue(
        &mut self,
        targets: Vec<Target>,
        kind: PendingKind,
        label: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.next_op_id += 1;
        self.op_queue.push_back(PendingOp {
            id: self.next_op_id,
            targets,
            kind,
            label,
        });
        if self.op.is_none() {
            self.start_next_op(window, cx);
        } else {
            cx.notify();
        }
    }

    /// Start the next queued operation, if any.
    pub(super) fn start_next_op(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.op_queue.pop_front() {
            Some(PendingOp {
                targets,
                kind: PendingKind::Delete { permanent },
                ..
            }) => self.start_delete(targets, permanent, window, cx),
            Some(PendingOp {
                targets,
                kind: PendingKind::Move { dest },
                ..
            }) => self.start_move(targets, dest, window, cx),
            Some(PendingOp {
                kind: PendingKind::Action { run },
                label,
                ..
            }) => self.start_action_run(label, run, window, cx),
            None => cx.notify(),
        }
    }

    fn delete_label(&self, targets: &[Target], permanent: bool) -> String {
        let total = targets.len();
        let lang = self.settings.locale.language.clone();
        let trash = fastdir_locale::trash_name(&lang);
        match targets {
            [one] if permanent => fastdir_locale::fmt(
                &lang,
                "dlg.op_deleting",
                &[("name", &one.fs.display().to_string())],
            ),
            [one] => fastdir_locale::fmt(
                &lang,
                "dlg.op_recycling",
                &[("name", &one.fs.display().to_string()), ("trash", trash)],
            ),
            _ if permanent => {
                fastdir_locale::fmt(&lang, "dlg.op_deleting_n", &[("total", &total.to_string())])
            }
            _ => fastdir_locale::fmt(
                &lang,
                "dlg.op_recycling_n",
                &[("total", &total.to_string()), ("trash", trash)],
            ),
        }
    }

    fn start_delete(
        &mut self,
        targets: Vec<Target>,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = targets.len();
        let label = self.delete_label(&targets, permanent);
        let progress = self.begin_op(
            label,
            targets.iter().map(|t| t.items).sum(),
            targets.iter().map(|t| t.size).sum(),
            false,
            cx,
        );
        let options = self.scan_options();
        let job = cx.background_executor().spawn(async move {
            let (mut items, mut bytes) = (0, 0);
            let mut out = Vec::with_capacity(targets.len());
            for t in targets {
                let report = fsops::delete_tracked(&t.fs, permanent, &progress);
                items += t.items;
                bytes += t.size;
                progress.reach(items, bytes);
                let gone = std::fs::symlink_metadata(&t.fs).is_err();
                // Part of the folder may be gone: re-measure what is left.
                let rest = (!gone && permanent && t.is_dir && !report.complete())
                    .then(|| scanner::scan_subtree(&t.fs, &options));
                out.push(DeleteOutcome {
                    path: t.fs,
                    gone,
                    rest,
                    report,
                });
            }
            out
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = job.await;
            this.update_in(cx, |this, window, cx| {
                let cancelled = this.end_op();
                // Remember where the user is before rows shift, so the delete
                // can finish without moving their view (opt-in setting).
                let keep_fs = if this.settings.ops.keep_selection_after_delete {
                    this.selected_fs_path().map(|(_, fs, _)| fs)
                } else {
                    None
                };
                let mut select = None;
                let mut done = Vec::new();
                let mut failed: Vec<(PathBuf, String)> = Vec::new();
                let mut changed = false;
                for r in results {
                    if r.gone {
                        // The last removal leaves the final, valid path.
                        if let Some(parent) = this.remove_from_tree(&r.path) {
                            select = Some(parent);
                        }
                        done.push(r.path);
                        changed = true;
                    } else if let Some(node) = r.rest {
                        if let Some(p) = this.splice_subtree(&r.path, node) {
                            select = Some(p);
                        }
                        changed = true;
                    }
                    failed.extend(r.report.failed);
                }
                if changed {
                    if let Some(fs) = keep_fs {
                        this.tree_changed_keeping_selection(&fs, select, cx);
                    } else {
                        this.tree_changed(select, cx);
                    }
                } else {
                    cx.notify();
                }
                let verb: String = if permanent {                    this.tr("dlg.done_deleted").to_string()
                } else {
                    this.tr_fmt("dlg.done_recycled", &[("trash", this.trash_name())])
                };
                if cancelled {
                    let msg = if done.is_empty() {
                        this.tr("dlg.cancel_none").to_string()
                    } else {
                        this.tr_fmt(
                            "dlg.cancel_some",
                            &[
                                ("verb", verb.as_str()),
                                ("done", &done.len().to_string()),
                                ("total", &total.to_string()),
                            ],
                        )
                    };
                    window.push_notification(
                        Notification::warning(msg).title(this.tr("dlg.cancel_title")),
                        cx,
                    );
                } else if !done.is_empty() {
                    let msg = match done.as_slice() {
                        [one] => this.tr_fmt(
                            "dlg.done_one",
                            &[("verb", verb.as_str()), ("name", &one.display().to_string())],
                        ),
                        _ => this.tr_fmt(
                            "dlg.done_multi",
                            &[("verb", verb.as_str()), ("n", &done.len().to_string())],
                        ),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if !failed.is_empty() {
                    let title = if failed.len() == 1 {
                        this.tr("dlg.fail_one").to_string()
                    } else {
                        this.tr_fmt("dlg.fail_multi", &[("n", &this.count(failed.len() as u64))])
                    };
                    let lang = this.settings.locale.language.clone();
                    window.push_notification(
                        Notification::error(failure_list_in(&failed, &lang)).title(title),
                        cx,
                    );
                }
                this.start_next_op(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Show the progress bar for a delete / move and keep it repainting.
    pub(super) fn begin_op(
        &mut self,
        label: String,
        total_items: u64,
        total_bytes: u64,
        by_bytes: bool,
        cx: &mut Context<Self>,
    ) -> Arc<fsops::OpProgress> {
        let progress = Arc::new(fsops::OpProgress::default());
        self.op = Some(FileOp {
            label: label.into(),
            progress: progress.clone(),
            total_items,
            total_bytes,
            by_bytes,
        });
        self._op_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.op.is_some()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
        progress
    }

    /// Clear the running operation. Returns whether it was cancelled.
    fn end_op(&mut self) -> bool {
        self._op_ticker = None;
        self.op.take().is_some_and(|op| op.progress.is_cancelled())
    }

    /// Cancel the running operation only; queued ones carry on.
    pub(super) fn cancel_op(&mut self, cx: &mut Context<Self>) {
        if let Some(op) = &self.op {
            op.progress.cancel();
            cx.notify();
        }
    }

    /// Drop one queued operation before it starts.
    pub(super) fn cancel_queued(&mut self, id: u64, cx: &mut Context<Self>) {
        self.op_queue.retain(|op| op.id != id);
        cx.notify();
    }

    /// Cancel the running operation and drop everything queued behind it.
    pub(super) fn cancel_all_ops(&mut self, cx: &mut Context<Self>) {
        self.op_queue.clear();
        self.cancel_op(cx);
        cx.notify();
    }

    /// Replace the node for the folder `fs` with a freshly scanned `node`.
    /// Returns the folder's new path.
    pub(super) fn splice_subtree(
        &mut self,
        fs: &std::path::Path,
        mut node: Subtree,
    ) -> Option<NodePath> {
        let tree = self.tree.as_ref()?;
        let mut t = tree.write();
        let path = t.find(fs)?;
        let parent: NodePath = path[..path.len().checked_sub(1)?].to_vec();
        node.root_mut()
            .set_expanded(t.get(&path).is_some_and(|n| n.expanded()));
        t.remove(&path)?;
        t.insert(&parent, node)
    }

    /// Remove the node for `fs` (if present). Returns the parent's new path.
    pub(super) fn remove_from_tree(&mut self, fs: &std::path::Path) -> Option<NodePath> {
        let tree = self.tree.as_ref()?;
        let mut t = tree.write();
        let path = t.find(fs)?;
        let parent_fs = fs.parent()?.to_path_buf();
        t.remove(&path)?;
        t.find(&parent_fs)
    }

    pub(super) fn rename_item(
        &mut self,
        _: &RenameItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_multi() || self.selected_is_special() {
            return;
        }
        let Some((path, fs, _)) = self.selected_fs_path() else {
            return;
        };
        if !self.can_touch_path(&fs) {
            self.notify_saved_missing(window, cx);
            return;
        }
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
        let dlg_title = self.tr("rename.title");
        let dlg_in = self.tr_fmt("rename.in_folder", &[("path", &fs.parent().map(|p| p.display().to_string()).unwrap_or_default())]);
        let dlg_cancel = self.tr("action.cancel");
        let dlg_ok = self.tr("rename.ok");
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.focus_handle(cx).focus(window, cx);
                input.update(cx, |s, cx| s.select_all(window, cx));
            }
            let ok = submit.clone();
            dialog
                .title(dlg_title)
                .w(sp(460.).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(dlg_in.clone()),
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
                                .label(dlg_cancel)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("rename-ok").primary().label(dlg_ok).on_click(
                            move |_, window, cx| {
                                if ok(window, cx) {
                                    window.close_dialog(cx);
                                }
                            },
                        )),
                )
        });
    }

    /// Returns true when the dialog may close.
    pub(super) fn do_rename(
        &mut self,
        fs: PathBuf,
        new_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let old = fs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if new_name.trim() == old {
            return true;
        }
        if !self.can_touch_path(&fs) {
            self.notify_saved_missing(window, cx);
            return false;
        }
        match fsops::rename(&fs, &new_name) {
            Ok(dest) => {
                let new_path = self.tree.as_ref().and_then(|tree| {
                    let mut t = tree.write();
                    let path = t.find(&fs)?;
                    let name = dest.file_name()?.to_string_lossy().to_string();
                    t.rename(&path, &name)
                });
                self.treemap.update(cx, |t, _| t.path_moved(&fs, &dest));
                self.tree_changed(new_path, cx);
                window.push_notification(
                    Notification::success(
                        self.tr_fmt("rename.done", &[("name", &dest.display().to_string())]),
                    ),
                    cx,
                );
                true
            }
            Err(e) => {
                window.push_notification(
                    Notification::error(e).title(self.tr("rename.failed")),
                    cx,
                );
                false
            }
        }
    }

    pub(super) fn move_item(&mut self, _: &MoveItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_refreshing() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() || targets.iter().any(|t| t.path.is_empty()) {
            return;
        }
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let srcs = targets;
        let move_here = self.tr("move.picker");
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(move_here.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(dest_dir) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if srcs.len() == 1 {
                    // Picking the folder was the confirmation.
                    this.run_move(srcs, dest_dir, window, cx);
                } else {
                    this.confirm_move(srcs, dest_dir, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// One confirmation for a whole multi-selection, showing its total size.
    pub(super) fn confirm_move(
        &mut self,
        srcs: Vec<Target>,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.ops.confirm_move {
            self.run_move(srcs, dest_dir, window, cx);
            return;
        }
        let description: SharedString = self
            .tr_fmt(
                "move.desc",
                &[
                    ("n", &srcs.len().to_string()),
                    ("size", &self.bytes(srcs.iter().map(|t| t.size).sum())),
                    ("dest", &dest_dir.display().to_string()),
                ],
            )
            .into();
        let title = self.tr("move.title");
        let ok = self.tr("move.ok");
        let cancel = self.tr("action.cancel");
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let srcs = srcs.clone();
            let dest_dir = dest_dir.clone();
            alert
                .title(title)
                .description(description.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok)
                        .cancel_text(cancel),
                )
                .on_ok(move |_, window, cx| {
                    let (srcs, dest_dir) = (srcs.clone(), dest_dir.clone());
                    entity
                        .update(cx, |this, cx| this.run_move(srcs, dest_dir, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn move_label(&self, srcs: &[Target], dest_dir: &std::path::Path) -> String {
        let total = srcs.len();
        let lang = self.settings.locale.language.clone();
        match srcs {
            [one] => fastdir_locale::fmt(
                &lang,
                "move.op_one",
                &[
                    ("src", &one.fs.display().to_string()),
                    ("dest", &dest_dir.display().to_string()),
                ],
            ),
            _ => fastdir_locale::fmt(
                &lang,
                "move.op_many",
                &[
                    ("n", &total.to_string()),
                    ("dest", &dest_dir.display().to_string()),
                ],
            ),
        }
    }

    fn start_move(
        &mut self,
        srcs: Vec<Target>,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = srcs.len();
        let label = self.move_label(&srcs, &dest_dir);
        let progress = self.begin_op(
            label,
            srcs.iter().map(|t| t.items).sum(),
            srcs.iter().map(|t| t.size).sum(),
            true,
            cx,
        );
        let dest = dest_dir.clone();
        let options = self.scan_options();
        let job = cx.background_executor().spawn(async move {
            let (mut items, mut bytes) = (0, 0);
            let mut out = Vec::with_capacity(srcs.len());
            for src in srcs {
                if progress.is_cancelled() {
                    break;
                }
                let res = fsops::move_into_tracked(&src.fs, &dest, &progress).map(|moved| {
                    // Measure what arrived so the destination's size is right.
                    let node = measure(&moved.dest, &options);
                    // Whatever could not be removed from the source stays there.
                    let rest = (!moved.leftover.is_empty()
                        && src.is_dir
                        && std::fs::symlink_metadata(&src.fs).is_ok())
                    .then(|| scanner::scan_subtree(&src.fs, &options));
                    MoveOutcome {
                        dest: moved.dest,
                        node,
                        leftover: moved.leftover,
                        rest,
                    }
                });
                items += src.items;
                bytes += src.size;
                progress.reach(items, bytes);
                out.push((src.fs, res));
            }
            out
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = job.await;
            this.update_in(cx, |this, window, cx| {
                let cancelled = this.end_op();
                let mut select = None;
                let mut moved = Vec::new();
                let mut failed: Vec<(PathBuf, String)> = Vec::new();
                let mut leftover: Vec<(PathBuf, String)> = Vec::new();
                for (src, res) in results {
                    match res {
                        Ok(m) => {
                            let mut sel = match m.rest {
                                Some(rest) => this.splice_subtree(&src, rest),
                                None => this.remove_from_tree(&src),
                            };
                            if let Some(tree) = &this.tree {
                                let mut t = tree.write();
                                if let Some(dir) = t.find(&dest_dir) {
                                    sel = t.insert(&dir, m.node);
                                }
                            }
                            if sel.is_some() {
                                select = sel;
                            }
                            if m.leftover.is_empty() {
                                this.treemap.update(cx, |t, _| t.path_moved(&src, &m.dest));
                            }
                            leftover.extend(m.leftover);
                            moved.push(m.dest);
                        }
                        Err(fsops::MoveError::Cancelled) => {}
                        Err(fsops::MoveError::Failed(e)) => failed.push((src, e)),
                    }
                }
                if moved.is_empty() {
                    cx.notify();
                } else {
                    // Remember where things go, for next time.
                    this.settings.ops.last_move_dest =
                        Some(dest_dir.to_string_lossy().into_owned());
                    this.settings.save();
                    this.tree_changed(select, cx);
                }
                if cancelled {
                    let msg = if moved.is_empty() {
                        this.tr("move.cancel_none").to_string()
                    } else {
                        this.tr_fmt(
                            "move.cancel_some",
                            &[
                                ("done", &moved.len().to_string()),
                                ("total", &total.to_string()),
                                ("dest", &dest_dir.display().to_string()),
                            ],
                        )
                    };
                    window
                        .push_notification(Notification::warning(msg).title(this.tr("move.cancel_title")), cx);
                } else if !moved.is_empty() {
                    let msg = match moved.as_slice() {
                        [one] => this.tr_fmt(
                            "move.done_one",
                            &[("dest", &one.display().to_string())],
                        ),
                        _ => this.tr_fmt(
                            "move.done_many",
                            &[
                                ("n", &moved.len().to_string()),
                                ("dest", &dest_dir.display().to_string()),
                            ],
                        ),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if !failed.is_empty() {
                    let msg = if total == 1 {
                        failed[0].1.clone()
                    } else {
                        failure_list_in(&failed, &this.settings.locale.language)
                    };
                    let title = if total == 1 {
                        this.tr("move.failed_title").to_string()
                    } else {
                        this.tr_fmt(
                            "move.failed_body",
                            &[("done", &failed.len().to_string()), ("total", &total.to_string())],
                        )
                    };
                    window.push_notification(Notification::error(msg).title(title), cx);
                }
                if !leftover.is_empty() {
                    window.push_notification(
                        Notification::error(failure_list_in(
                            &leftover,
                            &this.settings.locale.language,
                        ))
                        .title(this.tr("move.partial_title")),
                        cx,
                    );
                }
                this.start_next_op(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Re-scan just the selected folder and splice it into the tree.
    pub(super) fn refresh_item(
        &mut self,
        _: &RefreshItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_busy() || self.is_multi() || self.selected_is_special() {
            return;
        }
        let Some((path, fs, is_dir)) = self.selected_fs_path() else {
            return;
        };
        if !self.can_touch_path(&fs) {
            self.notify_saved_missing(window, cx);
            return;
        }
        if path.is_empty() {
            self.rescan(&Rescan, window, cx);
            return;
        }
        if !is_dir {
            return;
        }
        self.busy = Some(self.tr_fmt("move.refreshing", &[("name", &fs.display().to_string())]).into());
        cx.notify();
        let target = fs.clone();
        let options = self.scan_options();
        let job = cx
            .background_executor()
            .spawn(async move { scanner::scan_subtree(&target, &options) });
        cx.spawn_in(window, async move |this, cx| {
            let mut node = job.await;
            this.update_in(cx, |this, _, cx| {
                this.busy = None;
                node.root_mut().set_expanded(true);
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

    pub(super) fn toggle_theme(
        &mut self,
        _: &ToggleTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = if cx.theme().mode.is_dark() {
            ThemePreference::Light
        } else {
            ThemePreference::Dark
        };
        self.set_theme_preference(theme, window, cx);
    }
}

/// What a delete did to one selected item.
struct DeleteOutcome {
    path: PathBuf,
    /// The item no longer exists.
    gone: bool,
    /// A fresh scan of what is left of a partly deleted folder.
    rest: Option<Subtree>,
    report: fsops::DeleteReport,
}

/// One item that reached its destination.
struct MoveOutcome {
    dest: PathBuf,
    node: Subtree,
    leftover: Vec<(PathBuf, String)>,
    /// A fresh scan of the source folder if parts of it could not be removed.
    rest: Option<Subtree>,
}

/// A detached node for a freshly moved file or folder.
fn measure(path: &std::path::Path, options: &scanner::ScanOptions) -> Subtree {
    let meta = std::fs::symlink_metadata(path).ok();
    if meta.as_ref().is_some_and(|m| m.is_dir()) {
        return scanner::scan_subtree(path, options);
    }
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let disk_size = scanner::allocated_size(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    Subtree::file_on_disk(&name, size, disk_size, modified)
}

/// The first few failed paths with their reasons, one per line.
fn failure_list_in(failed: &[(PathBuf, String)], lang: &str) -> String {
    const SHOWN: usize = 5;
    let mut lines: Vec<String> = failed
        .iter()
        .take(SHOWN)
        .map(|(p, e)| fastdir_locale::fmt(lang, "dlg.fail_line", &[("name", &p.display().to_string()), ("error", e)]))
        .collect();
    if failed.len() > SHOWN {
        lines.push(fastdir_locale::fmt(
            lang,
            "dlg.more",
            &[("n", &fastdir_locale::format_count((failed.len() - SHOWN) as u64, lang))],
        ));
    }
    lines.join("\n")
}

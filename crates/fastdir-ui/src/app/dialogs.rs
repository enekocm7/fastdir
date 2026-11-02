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

    pub(super) fn open_selected(&mut self, _: &OpenItem, _: &mut Window, _: &mut Context<Self>) {
        if self.is_multi() || self.selected_is_special() {
            return;
        }
        if let Some((_, p, _)) = self.selected_fs_path() {
            fsops::open(&p);
        }
    }

    pub(super) fn reveal_selected(
        &mut self,
        _: &RevealItem,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.is_multi() || self.selected_is_special() {
            return;
        }
        if let Some((_, p, _)) = self.selected_fs_path() {
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
                "Paths copied to clipboard"
            } else {
                "Path copied to clipboard"
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
        if self.is_busy() {
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
                    "The scan root itself cannot be deleted here.",
                ),
                cx,
            );
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
                format!(
                    "the folder \"{}\" ({}, {} items)",
                    t.fs.display(),
                    self.bytes(t.size),
                    format::count(items)
                )
            } else {
                format!("the file \"{}\" ({})", t.fs.display(), self.bytes(t.size))
            }
        } else {
            let folders = targets.iter().filter(|t| t.is_dir).count();
            format!(
                "{} selected items ({} folders, {} files) totalling {}",
                targets.len(),
                folders,
                targets.len() - folders,
                self.bytes(targets.iter().map(|t| t.size).sum())
            )
        };
        let description: SharedString = if permanent {
            format!("Permanently delete {what}? This cannot be undone.").into()
        } else {
            format!("Move {what} to the {}?", platform::trash_name()).into()
        };
        let title: SharedString = if permanent {
            "Delete permanently".into()
        } else {
            format!("Move to {}", platform::trash_name()).into()
        };
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
                            "Delete".to_string()
                        } else {
                            format!("Move to {}", platform::trash_name())
                        })
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel"),
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

    pub(super) fn run_delete(
        &mut self,
        targets: Vec<Target>,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = targets.len();
        let label = match targets.as_slice() {
            [one] if permanent => format!("Deleting {}", one.fs.display()),
            [one] => format!(
                "Moving {} to the {}",
                one.fs.display(),
                platform::trash_name()
            ),
            _ if permanent => format!("Deleting {total} items"),
            _ => format!("Moving {total} items to the {}", platform::trash_name()),
        };
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
                    this.tree_changed(select, cx);
                } else {
                    cx.notify();
                }
                let verb: SharedString = if permanent {
                    "Deleted".into()
                } else {
                    format!("Moved to the {}:", platform::trash_name()).into()
                };
                if cancelled {
                    let msg = if done.is_empty() {
                        "Nothing was fully deleted. Anything already removed is gone from the list."
                            .to_string()
                    } else {
                        format!("{verb} {} of {total} items before cancelling.", done.len())
                    };
                    window.push_notification(
                        Notification::warning(msg).title("Delete cancelled"),
                        cx,
                    );
                } else if !done.is_empty() {
                    let msg = match done.as_slice() {
                        [one] => format!("{verb} {}", one.display()),
                        _ => format!("{verb} {} items", done.len()),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if !failed.is_empty() {
                    let title = if failed.len() == 1 {
                        "Could not delete 1 item".to_string()
                    } else {
                        format!(
                            "Could not delete {} items",
                            format::count(failed.len() as u64)
                        )
                    };
                    window.push_notification(
                        Notification::error(failure_list(&failed)).title(title),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Show the progress bar for a delete / move and keep it repainting.
    fn begin_op(
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

    pub(super) fn cancel_op(&mut self, cx: &mut Context<Self>) {
        if let Some(op) = &self.op {
            op.progress.cancel();
            cx.notify();
        }
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
                                .child(format!(
                                    "in {}",
                                    fs.parent()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default()
                                )),
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
                        .child(Button::new("rename-ok").primary().label("Rename").on_click(
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
                    Notification::success(format!("Renamed to {}", dest.display())),
                    cx,
                );
                true
            }
            Err(e) => {
                window.push_notification(Notification::error(e).title("Rename failed"), cx);
                false
            }
        }
    }

    pub(super) fn move_item(&mut self, _: &MoveItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() || targets.iter().any(|t| t.path.is_empty()) {
            return;
        }
        let srcs = targets;
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Move here".into()),
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
        let description: SharedString = format!(
            "Move {} selected items ({}) to \"{}\"?",
            srcs.len(),
            self.bytes(srcs.iter().map(|t| t.size).sum()),
            dest_dir.display()
        )
        .into();
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let srcs = srcs.clone();
            let dest_dir = dest_dir.clone();
            alert
                .title("Move items")
                .description(description.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Move")
                        .cancel_text("Cancel"),
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

    pub(super) fn run_move(
        &mut self,
        srcs: Vec<Target>,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = srcs.len();
        let label = match srcs.as_slice() {
            [one] => format!("Moving {} → {}", one.fs.display(), dest_dir.display()),
            _ => format!("Moving {total} items → {}", dest_dir.display()),
        };
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
                        "Nothing was moved. Any partial copy was removed.".to_string()
                    } else {
                        format!(
                            "Moved {} of {total} items to {} before cancelling. \
                             Any partial copy was removed.",
                            moved.len(),
                            dest_dir.display()
                        )
                    };
                    window
                        .push_notification(Notification::warning(msg).title("Move cancelled"), cx);
                } else if !moved.is_empty() {
                    let msg = match moved.as_slice() {
                        [one] => format!("Moved to {}", one.display()),
                        _ => format!("Moved {} items to {}", moved.len(), dest_dir.display()),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if !failed.is_empty() {
                    let msg = if total == 1 {
                        failed[0].1.clone()
                    } else {
                        failure_list(&failed)
                    };
                    let title = if total == 1 {
                        "Move failed".to_string()
                    } else {
                        format!("Could not move {} of {total} items", failed.len())
                    };
                    window.push_notification(Notification::error(msg).title(title), cx);
                }
                if !leftover.is_empty() {
                    window.push_notification(
                        Notification::error(failure_list(&leftover))
                            .title("Copied, but some originals could not be removed"),
                        cx,
                    );
                }
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
fn failure_list(failed: &[(PathBuf, String)]) -> String {
    const SHOWN: usize = 5;
    let mut lines: Vec<String> = failed
        .iter()
        .take(SHOWN)
        .map(|(p, e)| format!("{}: {e}", p.display()))
        .collect();
    if failed.len() > SHOWN {
        lines.push(format!(
            "…and {} more",
            format::count((failed.len() - SHOWN) as u64)
        ));
    }
    lines.join("\n")
}

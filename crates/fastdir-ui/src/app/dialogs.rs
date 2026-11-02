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
        if self.is_multi() {
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
        if self.is_multi() {
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
        self.confirm_delete(false, window, cx);
    }

    pub(super) fn delete_item(
        &mut self,
        _: &DeleteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_delete(true, window, cx);
    }

    pub(super) fn confirm_delete(
        &mut self,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
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
                    format::bytes(t.size),
                    format::count(items)
                )
            } else {
                format!(
                    "the file \"{}\" ({})",
                    t.fs.display(),
                    format::bytes(t.size)
                )
            }
        } else {
            let folders = targets.iter().filter(|t| t.is_dir).count();
            format!(
                "{} selected items ({} folders, {} files) totalling {}",
                targets.len(),
                folders,
                targets.len() - folders,
                format::bytes(targets.iter().map(|t| t.size).sum())
            )
        };
        let description: SharedString = if permanent {
            format!("Permanently delete {what}? This cannot be undone.").into()
        } else {
            format!("Move {what} to the Recycle Bin?").into()
        };
        let title = if permanent {
            "Delete permanently"
        } else {
            "Move to Recycle Bin"
        };
        let paths: Vec<PathBuf> = targets.into_iter().map(|t| t.fs).collect();
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let paths = paths.clone();
            alert
                .title(title)
                .description(description.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(if permanent {
                            "Delete"
                        } else {
                            "Move to Recycle Bin"
                        })
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel"),
                )
                .on_ok(move |_, window, cx| {
                    let paths = paths.clone();
                    entity
                        .update(cx, |this, cx| this.run_delete(paths, permanent, window, cx))
                        .ok();
                    true
                })
        });
    }

    pub(super) fn run_delete(
        &mut self,
        paths: Vec<PathBuf>,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = paths.len();
        self.busy = Some(
            match paths.as_slice() {
                [one] => format!("Deleting {}…", one.display()),
                _ => format!("Deleting {total} items…"),
            }
            .into(),
        );
        cx.notify();
        let job = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .map(|p| {
                    let res = fsops::delete(&p, permanent);
                    (p, res)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let mut select = None;
                let mut done = Vec::new();
                let mut failed = Vec::new();
                for (path, res) in results {
                    match res {
                        Ok(()) => {
                            // The last removal leaves the final, valid path.
                            if let Some(parent) = this.remove_from_tree(&path) {
                                select = Some(parent);
                            }
                            done.push(path);
                        }
                        Err(e) => failed.push((path, e)),
                    }
                }
                if done.is_empty() {
                    cx.notify();
                } else {
                    this.tree_changed(select, cx);
                    let msg = match done.as_slice() {
                        [one] => format!("Deleted {}", one.display()),
                        _ => format!("Deleted {} items", done.len()),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if let Some((first, err)) = failed.first() {
                    let msg = if total == 1 {
                        format!("Could not delete {}: {err}", first.display())
                    } else {
                        format!(
                            "Could not delete {} of {total} items. {}: {err}",
                            failed.len(),
                            first.display()
                        )
                    };
                    window.push_notification(Notification::error(msg).title("Delete failed"), cx);
                }
            })
            .ok();
        })
        .detach();
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
        if self.busy.is_some() || self.is_multi() {
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
        if self.busy.is_some() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() || targets.iter().any(|t| t.path.is_empty()) {
            return;
        }
        let total_size: u64 = targets.iter().map(|t| t.size).sum();
        let srcs: Vec<PathBuf> = targets.into_iter().map(|t| t.fs).collect();
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
                    this.confirm_move(srcs, total_size, dest_dir, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// One confirmation for a whole multi-selection, showing its total size.
    pub(super) fn confirm_move(
        &mut self,
        srcs: Vec<PathBuf>,
        total_size: u64,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let description: SharedString = format!(
            "Move {} selected items ({}) to \"{}\"?",
            srcs.len(),
            format::bytes(total_size),
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
        srcs: Vec<PathBuf>,
        dest_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = srcs.len();
        self.busy = Some(
            match srcs.as_slice() {
                [one] => format!("Moving {} → {}…", one.display(), dest_dir.display()),
                _ => format!("Moving {total} items → {}…", dest_dir.display()),
            }
            .into(),
        );
        cx.notify();
        let dest = dest_dir.clone();
        let job = cx.background_executor().spawn(async move {
            srcs.into_iter()
                .map(|src| {
                    let res = fsops::move_into(&src, &dest).map(|dest| {
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
                        (dest, node)
                    });
                    (src, res)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let mut select = None;
                let mut moved = Vec::new();
                let mut failed = Vec::new();
                for (src, res) in results {
                    match res {
                        Ok((dest, node)) => {
                            let mut sel = this.remove_from_tree(&src);
                            if let Some(tree) = &this.tree {
                                let mut t = tree.write();
                                if let Some(dir) = t.find(&dest_dir) {
                                    sel = t.insert(&dir, node);
                                }
                            }
                            if sel.is_some() {
                                select = sel;
                            }
                            this.treemap.update(cx, |t, _| t.path_moved(&src, &dest));
                            moved.push(dest);
                        }
                        Err(e) => failed.push((src, e)),
                    }
                }
                if moved.is_empty() {
                    cx.notify();
                } else {
                    this.tree_changed(select, cx);
                    let msg = match moved.as_slice() {
                        [one] => format!("Moved to {}", one.display()),
                        _ => format!("Moved {} items to {}", moved.len(), dest_dir.display()),
                    };
                    window.push_notification(Notification::success(msg), cx);
                }
                if let Some((first, err)) = failed.first() {
                    let msg = if total == 1 {
                        err.clone()
                    } else {
                        format!(
                            "Could not move {} of {total} items. {}: {err}",
                            failed.len(),
                            first.display()
                        )
                    };
                    window.push_notification(Notification::error(msg).title("Move failed"), cx);
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
        if self.busy.is_some() || self.is_multi() {
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

    pub(super) fn toggle_theme(
        &mut self,
        _: &ToggleTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
}

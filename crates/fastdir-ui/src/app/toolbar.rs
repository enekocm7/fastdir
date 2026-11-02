//! The top toolbar and the window title bar.

use super::*;

impl FastDir {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let op_view = self.op.as_ref().map(|op| self.render_op(op, cx));
        let search = self.render_search(cx);
        let scan_view = self.render_scan_progress(cx);
        let theme = cx.theme();
        let ready = matches!(self.phase, Phase::Ready);
        let scanning = matches!(self.phase, Phase::Scanning { .. });
        let has_sel = ready && !self.selection().is_empty();
        // Pseudo-items can be selected but have no file operations.
        let special = self.selected_is_special();
        let multi = self.is_multi();
        let is_root = self.only_root();
        let drives = self.drives.clone();
        let entity = cx.entity().downgrade();
        let update_entity = entity.clone();
        let focus = self.focus.clone();
        let busy = self.is_busy();

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
                                            .update(cx, |this, cx| {
                                                this.start_scan(path, window, cx)
                                            })
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
                    .disabled(!has_sel || is_root || multi)
                    .on_click(cx.listener(|this, _, w, cx| this.go_parent(&GoParent, w, cx))),
            )
            .child(
                tool("reveal", IconName::FolderOpen, "Show in Explorer (Ctrl+E)")
                    .disabled(!has_sel || multi || special)
                    .on_click(
                        cx.listener(|this, _, w, cx| this.reveal_selected(&RevealItem, w, cx)),
                    ),
            )
            .child(
                tool(
                    "refresh",
                    IconName::RotateCw,
                    "Refresh selected folder (F5)",
                )
                .disabled(!has_sel || busy || multi || special)
                .on_click(cx.listener(|this, _, w, cx| this.refresh_item(&RefreshItem, w, cx))),
            )
            .child(Separator::vertical().h(px(20.)).mx_1())
            .child(
                tool("rename", IconName::Pencil, "Rename (F2)")
                    .disabled(!has_sel || is_root || busy || multi || special)
                    .on_click(cx.listener(|this, _, w, cx| this.rename_item(&RenameItem, w, cx))),
            )
            .child(
                tool("move", IconName::FolderInput, "Move to… (Ctrl+M)")
                    .disabled(!has_sel || is_root || busy || special)
                    .on_click(cx.listener(|this, _, w, cx| this.move_item(&MoveItem, w, cx))),
            )
            .child(
                tool("trash", IconName::Recycle, "Move to Recycle Bin (Del)")
                    .disabled(!has_sel || is_root || busy || special)
                    .on_click(cx.listener(|this, _, w, cx| this.trash_item(&TrashItem, w, cx))),
            )
            .child(
                Button::new("delete")
                    .small()
                    .ghost()
                    .icon(Icon::new(IconName::Trash).text_color(theme.danger))
                    .tooltip("Delete permanently (Shift+Del)")
                    .disabled(!has_sel || is_root || busy || special)
                    .on_click(cx.listener(|this, _, w, cx| this.delete_item(&DeleteItem, w, cx))),
            )
            .child(Separator::vertical().h(px(20.)).mx_1())
            .child(search)
            .child(div().flex_1())
            .children(op_view)
            .children(scan_view)
            .when_some(self.busy.clone(), |d, label| {
                d.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .max_w(px(420.))
                        .overflow_hidden()
                        .child(Spinner::new().small())
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(label),
                        ),
                )
            })
            .when(!matches!(self.phase, Phase::Home), |d| {
                d.child(
                    tool("home", IconName::House, "Drive overview")
                        .on_click(cx.listener(|this, _, w, cx| this.go_home(w, cx))),
                )
            })
            .when_some(self.available.clone(), |d, rel| {
                let entity = update_entity.clone();
                d.child(
                    Button::new("update")
                        .small()
                        .primary()
                        .icon(IconName::ArrowDown)
                        .label(format!("Update to v{}", rel.version))
                        .dropdown_caret(true)
                        .dropdown_menu(move |menu, _, _| {
                            let (e1, e2, e3) = (entity.clone(), entity.clone(), entity.clone());
                            menu.item(
                                gpui_kit::component::menu::PopupMenuItem::new("Update now")
                                    .icon(IconName::ArrowDown)
                                    .on_click(move |_, window, cx| {
                                        e1.update(cx, |this, cx| this.start_update(window, cx))
                                            .ok();
                                    }),
                            )
                            .item(
                                gpui_kit::component::menu::PopupMenuItem::new("Release notes")
                                    .icon(IconName::FileText)
                                    .on_click(move |_, window, cx| {
                                        e2.update(cx, |this, cx| {
                                            this.show_release_notes(window, cx)
                                        })
                                        .ok();
                                    }),
                            )
                            .item(
                                gpui_kit::component::menu::PopupMenuItem::new("Skip this version")
                                    .icon(IconName::Ban)
                                    .on_click(move |_, window, cx| {
                                        e3.update(cx, |this, cx| this.skip_update(window, cx)).ok();
                                    }),
                            )
                        }),
                )
            })
            .child(
                tool("settings", IconName::Settings, "Settings (Ctrl+,)").on_click(
                    cx.listener(|this, _, w, cx| this.open_settings(&OpenSettings, w, cx)),
                ),
            )
            .child(tool("more", IconName::Ellipsis, "More").dropdown_menu({
                let auto = self.settings.auto_check_updates;
                let show_free = self.settings.show_free_space;
                let show_unknown = self.settings.show_unknown;
                let treemap_disk = self.settings.treemap_disk_size;
                let show_partial = self.settings.show_partial_results;
                let entity = update_entity.clone();
                move |menu, _, _| {
                    let (e0, e1, e2) = (entity.clone(), entity.clone(), entity.clone());
                    let (e3, e4) = (entity.clone(), entity.clone());
                    let e5 = entity.clone();
                    let e6 = entity.clone();
                    menu.item(
                        gpui_kit::component::menu::PopupMenuItem::new("Settings…")
                            .icon(IconName::Settings)
                            .on_click(move |_, window, cx| {
                                e0.update(cx, |this, cx| {
                                    this.open_settings(&OpenSettings, window, cx)
                                })
                                .ok();
                            }),
                    )
                    .separator()
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new("Check for updates")
                            .icon(IconName::RefreshCw)
                            .on_click(move |_, window, cx| {
                                e1.update(cx, |this, cx| this.check_for_updates(true, window, cx))
                                    .ok();
                            }),
                    )
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new(
                            "Check for updates automatically",
                        )
                        .checked(auto)
                        .on_click(move |_, _, cx| {
                            e2.update(cx, |this, cx| this.toggle_auto_update_check(cx))
                                .ok();
                        }),
                    )
                    .separator()
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new("Show free space in treemap")
                            .checked(show_free)
                            .on_click(move |_, window, cx| {
                                e3.update(cx, |this, cx| {
                                    this.toggle_free_space(&ToggleFreeSpace, window, cx)
                                })
                                .ok();
                            }),
                    )
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new("Show unknown in treemap")
                            .checked(show_unknown)
                            .on_click(move |_, window, cx| {
                                e4.update(cx, |this, cx| {
                                    this.toggle_unknown(&ToggleUnknown, window, cx)
                                })
                                .ok();
                            }),
                    )
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new("Treemap: size on disk")
                            .checked(treemap_disk)
                            .on_click(move |_, window, cx| {
                                e5.update(cx, |this, cx| this.toggle_treemap_disk_size(window, cx))
                                    .ok();
                            }),
                    )
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new(
                            "Show partial results while scanning",
                        )
                        .checked(show_partial)
                        .on_click(move |_, _, cx| {
                            e6.update(cx, |this, cx| this.toggle_partial_results(cx))
                                .ok();
                        }),
                    )
                    .separator()
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new(format!(
                            "FastDir v{}",
                            updater::CURRENT_VERSION
                        ))
                        .disabled(true),
                    )
                }
            }))
            .child(
                tool(
                    "theme",
                    if theme.mode.is_dark() {
                        IconName::Sun
                    } else {
                        IconName::Moon
                    },
                    "Toggle light / dark (Ctrl+T)",
                )
                .on_click(cx.listener(|this, _, w, cx| this.toggle_theme(&ToggleTheme, w, cx))),
            )
    }

    /// Compact live-scan progress in the toolbar while the results are shown.
    fn render_scan_progress(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Phase::Scanning {
            progress,
            started,
            expected,
            ..
        } = &self.phase
        else {
            return None;
        };
        let files = progress.files.load(Ordering::Relaxed);
        let bytes = progress.bytes.load(Ordering::Relaxed);
        let elapsed = started.elapsed();
        let rate = files as f64 / elapsed.as_secs_f64().max(0.001);
        let pct = expected.map(|e| (bytes as f64 / e as f64 * 100.0).min(99.0) as f32);
        let cancelling = progress.cancelled();
        let theme = cx.theme();
        Some(
            h_flex()
                .gap_2()
                .px_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .max_w(px(520.))
                .overflow_hidden()
                .child(Spinner::new().small())
                .child(div().flex_shrink_0().child(if cancelling {
                    "Stopping…"
                } else {
                    "Scanning"
                }))
                .child(
                    div().w(px(120.)).flex_shrink_0().child(
                        ProgressBar::new("toolbar-scan-progress")
                            .when_some(pct, |p, v| p.value(v))
                            .when(pct.is_none(), |p| p.loading(true)),
                    ),
                )
                .child(div().flex_shrink_0().whitespace_nowrap().child(format!(
                    "{} files · {}/s",
                    format::count(files),
                    format::count(rate as u64)
                )))
                .into_any_element(),
        )
    }

    /// Progress of a running delete / move: label, bar, counts and Cancel.
    fn render_op(&self, op: &FileOp, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let items = op
            .progress
            .items
            .load(Ordering::Relaxed)
            .min(op.total_items);
        let bytes = op
            .progress
            .bytes
            .load(Ordering::Relaxed)
            .min(op.total_bytes);
        let cancelling = op.progress.is_cancelled();
        let (done, total) = if op.by_bytes && op.total_bytes > 0 {
            (bytes, op.total_bytes)
        } else {
            (items, op.total_items)
        };
        let pct = (total > 0).then(|| (done as f64 / total as f64 * 100.0).min(100.0) as f32);
        let detail = if op.by_bytes {
            format!(
                "{} / {}",
                format::bytes(bytes),
                format::bytes(op.total_bytes)
            )
        } else {
            format!(
                "{} / {} items",
                format::count(items),
                format::count(op.total_items)
            )
        };
        h_flex()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .max_w(px(640.))
            .overflow_hidden()
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(if cancelling {
                        SharedString::from("Cancelling…")
                    } else {
                        op.label.clone()
                    }),
            )
            .child(
                div().w(px(140.)).flex_shrink_0().child(
                    ProgressBar::new("op-progress")
                        .when_some(pct, |p, v| p.value(v))
                        .when(pct.is_none(), |p| p.loading(true)),
                ),
            )
            .child(div().flex_shrink_0().whitespace_nowrap().child(detail))
            .child(
                Button::new("op-cancel")
                    .ghost()
                    .small()
                    .icon(IconName::CircleStop)
                    .label("Cancel")
                    .tooltip("Cancel (Esc)")
                    .disabled(cancelling)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_op(cx))),
            )
            .into_any_element()
    }

    pub(super) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let subtitle: Option<SharedString> = match &self.phase {
            Phase::Home => None,
            Phase::Scanning { root, .. } => Some(format!("Scanning {}", root.display()).into()),
            Phase::Ready => self
                .scan_root
                .as_ref()
                .map(|r| r.display().to_string().into()),
        };
        TitleBar::new().child(
            h_flex().w_full().pr_2().justify_between().child(
                h_flex()
                    .gap_2()
                    .child(img(logo()).size(px(18.)))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("FastDir"),
                    )
                    .when_some(subtitle, |d, s| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).child(s))
                    }),
            ),
        )
    }
}

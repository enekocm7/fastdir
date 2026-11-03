//! Scan orchestration, scan progress and the home screen.

use super::*;

impl FastDir {
    // ------------------------------------------------------------------
    // Scanning
    // ------------------------------------------------------------------

    pub(super) fn start_scan(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.home_drives = true;
        self.scan_root = Some(root.clone());
        self.settings.startup.last_path = Some(root.to_string_lossy().into_owned());
        self.settings.save();
        self.last_errors = 0;
        // The scanner fills this tree in place, so the list and the treemap
        // can show finished top-level folders before the scan ends.
        let tree = Arc::new(RwLock::new(Tree::empty(root.clone())));
        self.tree = Some(tree.clone());
        self.partial_generation = 0;
        self.rows.clear();
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.selected_ext = None;
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
        self.treemap.update(cx, |t, cx| {
            t.set_tree(Some(tree.clone()), cx);
            t.set_selection(vec![Vec::new()], cx);
            t.set_partial(self.settings.scan.show_partial_results, cx);
        });
        window.set_window_title(
            &self.tr_fmt("win.scanning", &[("path", &root.display().to_string())]),
        );

        let p = progress.clone();
        let scan_tree = tree.clone();
        let options = self.scan_options();
        let job = cx
            .background_executor()
            .spawn(async move { scanner::scan_live(root, p, scan_tree, &options) });
        self._scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_scan(result, tree, window, cx)
            })
            .ok();
        }));
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let still = this
                    .update(cx, |this, cx| {
                        this.live_tick(cx);
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

    /// Pull newly inserted top-level folders into the visible rows. Called by
    /// the scan ticker, so the list keeps up with the scanner without a full
    /// rebuild on every render.
    fn live_tick(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Scanning { .. }) {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let generation = tree.read().generation;
        if generation == self.partial_generation {
            return;
        }
        self.partial_generation = generation;
        // Node paths may have shifted; keep the cursor on the same folder but
        // drop multi-selections and the range anchor.
        let selected_fs = self.selected_fs_path().map(|(_, fs, _)| fs);
        self.marked.clear();
        self.rebuild_rows();
        self.restore_selected_fs(selected_fs);
        let selection = self.selection();
        self.treemap.update(cx, |t, cx| {
            t.invalidate(cx);
            t.set_selection(selection, cx);
        });
    }

    /// Re-select the node at `fs` after a rebuild, when it is still there.
    /// First-level indices move as finished top-level folders arrive, so a
    /// node path from before the rebuild cannot be trusted.
    fn restore_selected_fs(&mut self, fs: Option<PathBuf>) {
        if let Some(fs) = fs {
            let path = self.tree.as_ref().and_then(|t| t.read().find(&fs));
            if let Some(path) = path
                && let Some(ix) = self.rows.iter().position(|r| r.path == path)
            {
                self.selected = Some(ix);
            }
        }
        self.anchor = self.selected_path();
    }

    pub(super) fn finish_scan(
        &mut self,
        result: LiveScanResult,
        tree: Arc<RwLock<Tree>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A newer scan (or "home") may have replaced this one while it ran.
        if !self.tree.as_ref().is_some_and(|t| Arc::ptr_eq(t, &tree)) {
            return;
        }
        let LiveScanResult { elapsed, cancelled } = result;
        // Drive scans get `<Free space>` and `<Unknown>` pseudo-items so the
        // picture is complete (scanned vs used). Volume figures are from the
        // drive list taken when the scan started.
        let (root_path, files, size) = {
            let mut tree = tree.write();
            if let Some(drive) = self.drives.iter().find(|d| d.path == tree.root_path) {
                tree.attach_volume_info(drive.total, drive.free);
            }
            (
                tree.root_path.clone(),
                tree.root().files,
                tree.scanned_bytes(),
            )
        };
        let title = self.tr_fmt(
            "win.ready",
            &[
                ("path", &root_path.display().to_string()),
                ("suffix", if cancelled { self.tr("win.incomplete") } else { "" }),
            ],
        );
        window.set_window_title(&title);
        self.last_scan = Some((elapsed, cancelled));
        self.last_errors = if let Phase::Scanning { progress, .. } = &self.phase {
            progress.errors.load(Ordering::Relaxed)
        } else {
            0
        };
        self.phase = Phase::Ready;
        // Keep the folder the user was browsing selected across the finish.
        let selected_fs = self.selected_fs_path().map(|(_, fs, _)| fs);
        self.marked.clear();
        self.selected_ext = None;
        self.tab = ListTab::Tree;
        self.rebuild_rows();
        self.restore_selected_fs(selected_fs);
        if self.selected.is_none() {
            self.selected = Some(0);
            self.anchor = self.selected_path();
        }
        let selection = self.selection();
        self.partial_generation = tree.read().generation;
        self.top_stale = true;
        self.refresh_top_files();
        self.treemap.update(cx, |t, cx| {
            t.set_partial(false, cx);
            t.invalidate(cx);
            t.set_selection(selection, cx);
        });
        let msg = self.tr_fmt(
            "scan.done_files",
            &[
                ("files", &self.count(files as u64)),
                ("size", &self.bytes(size)),
                ("dur", &format::duration(elapsed)),
            ],
        );
        window.push_notification(
            Notification::new()
                .message(msg)
                .with_type(if cancelled {
                    NotificationType::Warning
                } else {
                    NotificationType::Success
                })
                .title(if cancelled {
                    self.tr("scan.stopped")
                } else {
                    self.tr("scan.complete")
                }),
            cx,
        );
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn stop_scan(&mut self, _: &StopScan, window: &mut Window, cx: &mut Context<Self>) {
        let scanning = if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        };
        // Esc also cancels a running delete / move...
        self.cancel_op(cx);
        // ...clears an active search/filter...
        if self.filter.is_active() {
            self.clear_filters(window, cx);
        }
        // ...and drops the treemap highlight.
        self.set_selected_ext(None, cx);
        if scanning {
            cx.notify();
        }
    }

    pub(super) fn rescan(&mut self, _: &Rescan, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.scan_root.clone() {
            self.start_scan(root, window, cx);
        }
    }

    pub(super) fn open_folder(
        &mut self,
        _: &OpenFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.tr("tb.scan_folder").into()),
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

    pub(super) fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.phase = Phase::Home;
        self.home_drives = true;
        self.tree = None;
        self.rows.clear();
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.tab = ListTab::Tree;
        self.filter = Filter::default();
        self.filter_form = FilterForm::default();
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.top_files.clear();
        self.top_selected = None;
        self.top_stale = true;
        self.drives = drives::list();
        self.last_errors = 0;
        self.treemap.update(cx, |t, cx| t.set_tree(None, cx));
        window.set_window_title(self.tr("app.name"));
        cx.notify();
    }

    pub(super) fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if !self.home_drives {
            // "Open nothing" startup mode: a blank home until the user scans.
            return v_flex()
                .id("home")
                .size_full()
                .overflow_y_scroll()
                .items_center()
                .pt(sp(56.))
                .gap_6()
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .child(self.tr("home.title")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(self.tr("home.pick_folder")),
                        ),
                )
                .child(
                    Button::new("scan-folder")
                        .outline()
                        .icon(IconName::FolderSearch)
                        .label(self.tr("home.scan_folder"))
                        .on_click(cx.listener(|this, _, w, cx| this.open_folder(&OpenFolder, w, cx))),
                );
        }
        let show_admin = !elevate::is_elevated();
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
                .w(sp(300.))
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
                                .size(sp(40.))
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
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .whitespace_nowrap()
                                        .child(d.title_in(self.drive_kind_name(d.kind))),
                                )
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    format!(
                                        "{} · {}",
                                        self.drive_kind_name(d.kind),
                                        if d.fs.is_empty() { "—" } else { &d.fs }
                                    ),
                                )),
                        ),
                )
                .child(
                    ProgressBar::new(("drive-bar", i))
                        .value(frac * 100.)
                        .color(bar),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(self.tr_fmt("home.used", &[("size", &self.bytes(d.used()))]))
                        .child(self.tr_fmt(
                            "tb.drive_free",
                            &[("free", &self.bytes(d.free)), ("total", &self.bytes(d.total))],
                        )),
                )
        });
        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .items_center()
            .pt(sp(56.))
            .gap_6()
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .child(self.tr("home.title")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(self.tr("home.pick_drive")),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_4()
                    .max_w(sp(960.))
                    .children(cards),
            )
            .child(
                h_flex()
                    .justify_center()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("scan-folder")
                            .outline()
                            .icon(IconName::FolderSearch)
                            .label(self.tr("home.scan_folder"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.open_folder(&OpenFolder, w, cx)),
                            ),
                    )
                    .when(show_admin, |d| {
                        d.child(
                            Button::new("restart-admin-home")
                                .outline()
                                .icon(IconName::Shield)
                                .label(self.tr("home.restart_admin"))
                                .tooltip(self.tr("home.restart_tip"))
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.restart_elevated(None, w, cx)
                                })),
                        )
                    }),
            )
            .when(show_admin, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(
                            self.tr("home.no_elev_note"),
                        ),
                )
            })
    }

    pub(super) fn render_scanning(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                .w(sp(150.))
                .p_3()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
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
                            .child(self.tr_fmt(
                                "scan.heading",
                                &[("path", &root.display().to_string())],
                            )),
                    ),
            )
            .child(
                div().w(sp(640.)).child(
                    ProgressBar::new("scan-progress")
                        .when_some(pct, |p, v| p.value(v))
                        .when(pct.is_none(), |p| p.loading(true)),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(stat(self.tr("scan.files"), self.count(files)))
                    .child(stat(self.tr("scan.folders"), self.count(dirs)))
                    .child(stat(self.tr("scan.size"), self.bytes(bytes)))
                    .child(stat(self.tr("scan.speed"), format!("{}/s", self.count(rate as u64))))
                    .child(stat(self.tr("scan.elapsed"), format::duration(elapsed))),
            )
            .child(
                div()
                    .w(sp(760.))
                    .text_xs()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(current),
            )
            .when(errors > 0, |d| {
                let root = root.clone();
                d.child(
                    h_flex()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(div().text_xs().text_color(theme.warning).child(
                            self.tr_fmt("scan.unreadable", &[("n", &self.count(errors))]),
                        ))
                        .when(!elevate::is_elevated(), |d| {
                            d.child(
                                Button::new("restart-admin-scanning")
                                    .small()
                                    .outline()
                                    .icon(IconName::Shield)
                                    .label(self.tr("scan.restart_admin"))
                                    .tooltip(self.tr("scan.restart_rescan_tip"))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.restart_elevated(Some(root.clone()), w, cx)
                                    })),
                            )
                        }),
                )
            })
            .child(
                Button::new("stop-scan")
                    .outline()
                    .icon(IconName::CircleStop)
                    .label(if cancelling {
                        self.tr("action.stopping")
                    } else {
                        self.tr("action.stop")
                    })
                    .disabled(cancelling)
                    .on_click(cx.listener(|this, _, w, cx| this.stop_scan(&StopScan, w, cx))),
            )
            .into_any_element()
    }
}

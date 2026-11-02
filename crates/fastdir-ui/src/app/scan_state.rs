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
        self.scan_root = Some(root.clone());
        self.tree = None;
        self.rows.clear();
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.selected_ext = None;
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

    pub(super) fn finish_scan(
        &mut self,
        result: ScanResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ScanResult {
            mut tree,
            elapsed,
            cancelled,
        } = result;
        let title = format!(
            "FastDir — {}{}",
            tree.root_path.display(),
            if cancelled { " (incomplete)" } else { "" }
        );
        window.set_window_title(&title);
        // Drive scans get `<Free space>` and `<Unknown>` pseudo-items so the
        // picture is complete (scanned vs used). Volume figures are from the
        // drive list taken when the scan started.
        if let Some(drive) = self.drives.iter().find(|d| d.path == tree.root_path) {
            tree.attach_volume_info(drive.total, drive.free);
        }
        let files = tree.root.files;
        let size = tree.scanned_bytes();
        let tree = Arc::new(RwLock::new(tree));
        self.tree = Some(tree.clone());
        self.last_scan = Some((elapsed, cancelled));
        self.phase = Phase::Ready;
        self.selected = Some(0);
        self.marked.clear();
        self.anchor = Some(Vec::new());
        self.selected_ext = None;
        self.rebuild_rows();
        self.treemap.update(cx, |t, cx| {
            t.set_tree(Some(tree), cx);
            t.set_selection(vec![Vec::new()], cx);
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
                .title(if cancelled {
                    "Scan stopped"
                } else {
                    "Scan complete"
                }),
            cx,
        );
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn stop_scan(&mut self, _: &StopScan, _: &mut Window, cx: &mut Context<Self>) {
        let scanning = if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        };
        // Esc also drops the treemap highlight.
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

    pub(super) fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Phase::Scanning { progress, .. } = &self.phase {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        self.phase = Phase::Home;
        self.tree = None;
        self.rows.clear();
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.drives = drives::list();
        self.treemap.update(cx, |t, cx| t.set_tree(None, cx));
        window.set_window_title("FastDir");
        cx.notify();
    }

    pub(super) fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .whitespace_nowrap()
                                        .child(d.title()),
                                )
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    format!(
                                        "{} · {}",
                                        d.kind.label(),
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
                        .child(format!("{} used", format::bytes(d.used())))
                        .child(format!(
                            "{} free of {}",
                            format::bytes(d.free),
                            format::bytes(d.total)
                        )),
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
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .child("What's using my disk?"),
                    )
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
                .w(px(150.))
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
                d.child(div().text_xs().text_color(theme.warning).child(format!(
                    "{} folders could not be read (access denied)",
                    format::count(errors)
                )))
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
}

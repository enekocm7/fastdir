//! Update flow: background check on startup, "Update now" with a progress
//! dialog, release notes and "Skip this version".

use super::*;
use fastdir_updater::{self as updater, DownloadProgress, InstallKind, Release, UpdateError};

/// Live contents of the "Updating FastDir" dialog.
struct DownloadView {
    version: String,
    progress: Arc<DownloadProgress>,
    /// Resolved language code, captured when the download starts.
    lang: String,
}

impl Render for DownloadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pct = self.progress.percent();
        let done = self.progress.downloaded.load(Ordering::Relaxed);
        let total = self.progress.total.load(Ordering::Relaxed);
        let detail = if total > 0 {
            fastdir_locale::fmt(
                &self.lang,
                "upd.progress",
                &[
                    ("done", &format::bytes(done)),
                    ("total", &format::bytes(total)),
                ],
            )
        } else {
            format::bytes(done)
        };
        v_flex()
            .gap_2()
            .child(div().text_sm().child(fastdir_locale::fmt(
                &self.lang,
                "upd.downloading",
                &[("version", &self.version)],
            )))
            .child(
                ProgressBar::new("update-progress")
                    .when_some(pct, |p, v| p.value(v))
                    .when(pct.is_none(), |p| p.loading(true)),
            )
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(detail))
    }
}

impl FastDir {
    /// Runs once per launch: reports a finished update and kicks off the
    /// automatic check when it is due under the configured frequency.
    pub(super) fn startup_update_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = updater::CURRENT_VERSION;
        let previous = self
            .settings
            .updates
            .last_run_version
            .replace(current.to_string());
        if previous.as_deref() != Some(current) {
            self.settings.save();
            if previous.as_deref().is_some_and(|prev| updater::is_newer(prev, current)) {
                window.push_notification(
                    Notification::success(
                        self.tr_fmt("upd.updated_body", &[("current", current)]),
                    )
                    .title(self.tr("upd.updated_title")),
                    cx,
                );
            }
        }
        if self.settings.updates.auto_check && self.update_check_due() {
            self.check_for_updates(false, window, cx);
        }
    }

    /// Whether the automatic check is due under the configured frequency.
    fn update_check_due(&self) -> bool {
        match self.settings.updates.frequency {
            UpdateFrequency::Start => true,
            UpdateFrequency::Daily => self.hours_since_last_check() >= 24,
            UpdateFrequency::Weekly => self.hours_since_last_check() >= 24 * 7,
        }
    }

    fn hours_since_last_check(&self) -> i64 {
        let now = unix_now();
        match self.settings.updates.last_check {
            Some(last) => now.saturating_sub(last).max(0) / 3600,
            // Never checked: due.
            None => i64::MAX,
        }
    }

    /// `manual` checks report errors and "you're up to date"; automatic ones
    /// stay silent unless there is something to install.
    pub(super) fn check_for_updates(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking || self.updating {
            return;
        }
        self.checking = true;
        // Any check (manual or automatic) counts for daily / weekly pacing.
        self.settings.updates.last_check = Some(unix_now());
        self.settings.save();
        cx.notify();
        let channel = update_channel_of(self.settings.updates.channel);
        let job = cx
            .background_executor()
            .spawn(async move { updater::fetch_channel(channel) });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.checking = false;
                this.on_check_result(result, manual, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_check_result(
        &mut self,
        result: Result<Option<Release>, UpdateError>,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let release = match result {
            Ok(release) => release.filter(Release::is_newer_than_current),
            Err(e) => {
                if manual {
                    window.push_notification(
                        Notification::error(
                            self.tr_fmt("upd.check_fail_body", &[("error", &e.to_string())]),
                        )
                        .title(self.tr("upd.check_fail_title")),
                        cx,
                    );
                }
                return;
            }
        };
        let Some(release) = release else {
            if manual {
                window.push_notification(
                    Notification::success(self.tr_fmt(
                        "upd.uptodate_body",
                        &[("version", &updater::CURRENT_VERSION.to_string())],
                    ))
                    .title(self.tr("upd.uptodate_title")),
                    cx,
                );
            }
            return;
        };

        let version = release.version.to_string();
        // A skip only applies to that exact version; a newer release clears it.
        if self.settings.updates.skipped_version.as_deref().is_some_and(|s| s != version) {
            self.settings.updates.skipped_version = None;
            self.settings.save();
        }
        let skipped = self.settings.updates.skipped_version.as_deref() == Some(version.as_str());
        if skipped && !manual {
            return;
        }
        self.available = Some(Arc::new(release));
        self.notify_available(&version, window, cx);
        cx.notify();
    }

    fn notify_available(&self, version: &str, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        let body = self.tr_fmt(
            "upd.avail_body",
            &[
                ("version", version),
                ("current", &updater::CURRENT_VERSION.to_string()),
            ],
        );
        let title = self.tr("upd.avail_title");
        if let Some(hint) = self.no_self_update_hint(updater::detect_kind()) {
            // No "Update now" for installs FastDir must not replace itself.
            window.push_notification(
                Notification::info(format!("{body}\n{hint}")).title(title),
                cx,
            );
            return;
        }
        let label = self.tr("upd.update_now");
        window.push_notification(
            Notification::info(body)
                .title(title)
                .action(move |_, _, cx| {
                    let entity = entity.clone();
                    Button::new("update-now")
                        .primary()
                        .label(label)
                        .on_click(cx.listener(move |note, _, window, cx| {
                            note.dismiss(window, cx);
                            entity
                                .update(cx, |this, cx| this.start_update(window, cx))
                                .ok();
                        }))
                }),
            cx,
        );
    }

    /// What to tell the user instead of updating, for installs FastDir does
    /// not update itself (package-managed or unpacked by hand).
    fn no_self_update_hint(&self, kind: InstallKind) -> Option<&'static str> {
        match kind {
            InstallKind::PackageManager => Some(self.tr("upd.use_package_manager")),
            InstallKind::Manual => Some(self.tr("upd.manual_download")),
            InstallKind::Installed { .. } | InstallKind::Portable | InstallKind::AppImage => None,
        }
    }

    pub(super) fn show_release_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.available.clone() else { return };
        let entity = cx.entity().downgrade();
        let notes: SharedString = if release.notes.trim().is_empty() {
            self.tr("upd.no_notes").into()
        } else {
            release.notes.clone().into()
        };
        let title = self.tr_fmt("upd.notes_title", &[("version", &release.version.to_string())]);
        let close_label = self.tr("action.close");
        let update_label = self.tr("upd.update_now");
        window.open_dialog(cx, move |dialog, window, cx| {
            let entity = entity.clone();
            dialog
                .title(title.clone())
                .w(sp(640.).to_pixels(window.rem_size()))
                .child(
                    div()
                        .id("release-notes")
                        .max_h(sp(380.))
                        .overflow_y_scroll()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(notes.clone()),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("notes-close")
                                .outline()
                                .label(close_label)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("notes-update")
                                .primary()
                                .label(update_label)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    entity
                                        .update(cx, |this, cx| this.start_update(window, cx))
                                        .ok();
                                }),
                        ),
                )
        });
    }

    pub(super) fn skip_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.available.take() else { return };
        let version = release.version.to_string();
        self.settings.updates.skipped_version = Some(version.clone());
        self.settings.save();
        window.push_notification(
            Notification::info(
                self.tr_fmt("upd.skip_body", &[("version", &version)]),
            )
            .title(self.tr("upd.skip_title")),
            cx,
        );
        cx.notify();
    }

    pub(super) fn toggle_auto_update_check(&mut self, cx: &mut Context<Self>) {
        self.settings.updates.auto_check = !self.settings.updates.auto_check;
        self.settings.save();
        cx.notify();
    }

    /// Downloads and verifies the update with a progress dialog.
    pub(super) fn start_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.updating {
            return;
        }
        let Some(release) = self.available.clone() else { return };
        let kind = updater::detect_kind();
        if let Some(hint) = self.no_self_update_hint(kind) {
            // Never touch binaries a package manager owns; point there instead.
            window.push_notification(
                Notification::info(hint).title(self.tr("upd.avail_title")),
                cx,
            );
            return;
        }
        if release.asset_for(kind).is_none() {
            window.push_notification(
                Notification::error(self.tr("upd.no_asset")).title(self.tr("upd.fail_title")),
                cx,
            );
            return;
        }
        self.updating = true;
        cx.notify();

        let progress = Arc::new(DownloadProgress::default());
        let lang = self.settings.locale.language.clone();
        let view = cx.new(|_| DownloadView {
            version: release.version.to_string(),
            progress: progress.clone(),
            lang: fastdir_locale::resolve(&lang).to_string(),
        });
        let dlg_title = self.tr("upd.updating_title");
        let dlg_cancel = self.tr("action.cancel");
        window.open_dialog(cx, {
            let progress = progress.clone();
            let view = view.clone();
            move |dialog, window, _| {
                let progress = progress.clone();
                dialog
                    .title(dlg_title)
                    .w(sp(460.).to_pixels(window.rem_size()))
                    .close_button(false)
                    .overlay_closable(false)
                    .keyboard(false)
                    .child(view.clone())
                    .footer(
                        h_flex().w_full().justify_end().child(
                            Button::new("update-cancel")
                                .outline()
                                .label(dlg_cancel)
                                .on_click(move |_, window, cx| {
                                    progress.cancel.store(true, Ordering::Relaxed);
                                    window.close_dialog(cx);
                                }),
                        ),
                    )
            }
        });

        let view = view.downgrade();
        self._update_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                if !this.update(cx, |this, _| this.updating).unwrap_or(false) {
                    break;
                }
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));

        let job = {
            let release = release.clone();
            let progress = progress.clone();
            cx.background_executor()
                .spawn(async move { updater::download(&release, kind, &progress) })
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_download(result, &progress, release, kind, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_download(
        &mut self,
        result: Result<std::path::PathBuf, UpdateError>,
        progress: &DownloadProgress,
        release: Arc<Release>,
        kind: InstallKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.updating = false;
        // The Cancel button already closed the dialog.
        let cancelled = progress.cancel.load(Ordering::Relaxed);
        if !cancelled {
            window.close_dialog(cx);
        }
        cx.notify();

        let path = match result {
            Ok(path) if !cancelled => path,
            Ok(_) | Err(UpdateError::Cancelled) => {
                updater::discard_downloads();
                window.push_notification(
                    Notification::info(self.tr("upd.cancelled_body")),
                    cx,
                );
                return;
            }
            Err(e) => {
                window.push_notification(
                    Notification::error(e.to_string()).title(self.tr("upd.fail_title")),
                    cx,
                );
                return;
            }
        };

        let version = release.version.to_string();
        if matches!(self.phase, Phase::Scanning { .. }) || self.is_busy() {
            let entity = cx.entity().downgrade();
            let alert_title = self.tr("upd.restart_title");
            let alert_body = self.tr_fmt("upd.restart_body", &[("version", &version)]);
            let alert_ok = self.tr("upd.restart_now");
            let alert_cancel = self.tr("upd.later");
            window.open_alert_dialog(cx, move |alert, _, _| {
                let entity = entity.clone();
                let path = path.clone();
                alert
                    .title(alert_title)
                    .description(alert_body.clone())
                    .confirm()
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text(alert_ok)
                            .cancel_text(alert_cancel)
                            .on_cancel(|_, _, _| {
                                updater::discard_downloads();
                                true
                            }),
                    )
                    .on_ok(move |_, window, cx| {
                        let path = path.clone();
                        entity
                            .update(cx, |this, cx| this.apply_update(path, kind, window, cx))
                            .ok();
                        true
                    })
            });
        } else {
            self.apply_update(path, kind, window, cx);
        }
    }

    fn apply_update(&mut self, path: std::path::PathBuf, kind: InstallKind, window: &mut Window, cx: &mut Context<Self>) {
        match updater::apply(&path, kind) {
            Ok(()) => {
                self.remember_window(window);
                cx.quit()
            }
            Err(e) => {
                updater::discard_downloads();
                window.push_notification(
                    Notification::error(
                        self.tr_fmt("upd.install_fail", &[("error", &e.to_string())]),
                    )
                    .title(self.tr("upd.fail_title")),
                    cx,
                );
            }
        }
    }
}

/// Current time as a Unix timestamp, for update pacing.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

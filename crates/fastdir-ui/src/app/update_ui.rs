//! Update flow: background check on startup, "Update now" with a progress
//! dialog, release notes and "Skip this version".

use super::*;
use fastdir_updater::{self as updater, DownloadProgress, InstallKind, Release, UpdateError};

/// Live contents of the "Updating FastDir" dialog.
struct DownloadView {
    version: String,
    progress: Arc<DownloadProgress>,
}

impl Render for DownloadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pct = self.progress.percent();
        let done = self.progress.downloaded.load(Ordering::Relaxed);
        let total = self.progress.total.load(Ordering::Relaxed);
        let detail = if total > 0 {
            format!("{} of {}", format::bytes(done), format::bytes(total))
        } else {
            format::bytes(done)
        };
        v_flex()
            .gap_2()
            .child(div().text_sm().child(format!("Downloading FastDir v{}…", self.version)))
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
                    Notification::success(format!("Updated to v{current}")).title("FastDir updated"),
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
                        Notification::error(format!("Could not check for updates: {e}"))
                            .title("Update check failed"),
                        cx,
                    );
                }
                return;
            }
        };
        let Some(release) = release else {
            if manual {
                window.push_notification(
                    Notification::success(format!("FastDir v{} is the latest version.", updater::CURRENT_VERSION))
                        .title("You're up to date"),
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
        window.push_notification(
            Notification::info(format!(
                "FastDir v{version} is available (you have v{}).",
                updater::CURRENT_VERSION
            ))
            .title("Update available")
            .action(move |_, _, cx| {
                let entity = entity.clone();
                Button::new("update-now")
                    .primary()
                    .label("Update now")
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

    pub(super) fn show_release_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(release) = self.available.clone() else { return };
        let entity = cx.entity().downgrade();
        let notes: SharedString = if release.notes.trim().is_empty() {
            "No release notes were published for this version.".into()
        } else {
            release.notes.clone().into()
        };
        let title = format!("FastDir v{} — release notes", release.version);
        window.open_dialog(cx, move |dialog, _, cx| {
            let entity = entity.clone();
            dialog
                .title(title.clone())
                .w(px(640.))
                .child(
                    div()
                        .id("release-notes")
                        .max_h(px(380.))
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
                                .label("Close")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("notes-update")
                                .primary()
                                .label("Update now")
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
            Notification::info(format!("You won't be reminded about v{version} again.")).title("Version skipped"),
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
        if release.asset_for(kind).is_none() {
            window.push_notification(
                Notification::error("This release has no download for your type of install.")
                    .title("Update failed"),
                cx,
            );
            return;
        }
        self.updating = true;
        cx.notify();

        let progress = Arc::new(DownloadProgress::default());
        let view = cx.new(|_| DownloadView {
            version: release.version.to_string(),
            progress: progress.clone(),
        });
        window.open_dialog(cx, {
            let progress = progress.clone();
            let view = view.clone();
            move |dialog, _, _| {
                let progress = progress.clone();
                dialog
                    .title("Updating FastDir")
                    .w(px(460.))
                    .close_button(false)
                    .overlay_closable(false)
                    .keyboard(false)
                    .child(view.clone())
                    .footer(
                        h_flex().w_full().justify_end().child(
                            Button::new("update-cancel")
                                .outline()
                                .label("Cancel")
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
                window.push_notification(Notification::info("The update was cancelled."), cx);
                return;
            }
            Err(e) => {
                window.push_notification(
                    Notification::error(e.to_string()).title("Update failed"),
                    cx,
                );
                return;
            }
        };

        let version = release.version.to_string();
        if matches!(self.phase, Phase::Scanning { .. }) || self.is_busy() {
            let entity = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let entity = entity.clone();
                let path = path.clone();
                alert
                    .title("Restart to finish updating?")
                    .description(format!(
                        "FastDir v{version} is ready to install. A scan or file operation is still running and \
                         will be lost when FastDir restarts."
                    ))
                    .confirm()
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text("Restart now")
                            .cancel_text("Later")
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
                    Notification::error(format!("Could not install the update: {e}")).title("Update failed"),
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

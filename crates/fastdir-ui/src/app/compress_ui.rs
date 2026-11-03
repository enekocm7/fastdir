//! Compress / uncompress: the dialog, the queue entry and the in-place
//! `disk_size` refresh after the operation.

use super::dialogs::failure_list_in;
use super::*;
use fastdir_core::compress::{self as compression, Algorithm, CompressOptions, CompressReport};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;

/// The choices in the Compress dialog. Kept on [`FastDir`] because the dialog
/// is rebuilt every frame and reads the current values back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CompressForm {
    pub algorithm: Algorithm,
    pub folders_too: bool,
    pub skip_poorly_compressed: bool,
}

impl Default for CompressForm {
    fn default() -> Self {
        let options = CompressOptions::default();
        Self {
            algorithm: options.algorithm,
            folders_too: options.folders_too,
            skip_poorly_compressed: options.skip_poorly_compressed,
        }
    }
}

/// The locale key for an algorithm's label.
fn algorithm_key(algorithm: Algorithm) -> &'static str {
    match algorithm {
        Algorithm::Lznt1 => "compress.alg.lznt1",
        Algorithm::Xpress4K => "compress.alg.xpress4k",
        Algorithm::Xpress8K => "compress.alg.xpress8k",
        Algorithm::Xpress16K => "compress.alg.xpress16k",
        Algorithm::Lzx => "compress.alg.lzx",
    }
}

impl FastDir {
    /// Whether the current selection can be compressed: everything is on an
    /// NTFS volume and can be touched on disk.
    pub(super) fn can_compress_selected(&self) -> bool {
        let targets = self.targets();
        !targets.is_empty()
            && targets
                .iter()
                .all(|t| compression::compression_supported(&t.fs))
            && self.targets_on_disk(&targets)
    }

    /// Open the algorithm dialog for the current selection.
    pub(super) fn compress_item(
        &mut self,
        _: &CompressItem,
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
        if !targets
            .iter()
            .all(|t| compression::compression_supported(&t.fs))
        {
            window.push_notification(Notification::warning(self.tr("compress.no_support")), cx);
            return;
        }
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        self.compress_form = CompressForm::default();
        let items = targets.iter().map(|t| t.items).sum::<u64>();
        let size = targets.iter().map(|t| t.size).sum::<u64>();
        let summary = self.tr_fmt(
            "compress.items",
            &[("items", &self.count(items)), ("size", &self.bytes(size))],
        );
        let caption = self.tr("compress.caption");
        let algorithm_label = self.tr("compress.algorithm");
        let folders_label = self.tr("compress.folders");
        let skip_label = self.tr("compress.skip");
        let estimate_note = self.tr("compress.estimate_note");
        let title = self.tr("compress.title");
        let ok = self.tr("compress.start");
        let cancel = self.tr("action.cancel");
        let lang = self.settings.locale.language.clone();
        let entity = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(app) = entity.upgrade() else {
                return dialog;
            };
            let (form, estimate) = {
                let view = app.read(cx);
                let estimate_size = view
                    .bytes((size as f64 * view.compress_form.algorithm.estimated_ratio()) as u64);
                (
                    view.compress_form,
                    view.tr_fmt("compress.estimate", &[("size", &estimate_size)]),
                )
            };
            let set_entity = entity.clone();
            let folder_entity = entity.clone();
            let skip_entity = entity.clone();
            let form_folders = form.folders_too;
            let form_skip = form.skip_poorly_compressed;
            let start_entity = entity.clone();
            let dialog_lang = lang.clone();
            let targets = targets.clone();
            dialog
                .title(title)
                .w(sp(560.).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_xs().child(summary.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(caption.to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(algorithm_label),
                        )
                        .child(
                            RadioGroup::vertical("compress-algorithm")
                                .selected_index(Some(
                                    compression::ALGORITHMS
                                        .iter()
                                        .position(|a| *a == form.algorithm)
                                        .unwrap_or(2),
                                ))
                                .children(compression::ALGORITHMS.iter().map(|algorithm| {
                                    Radio::new(format!("compress-alg-{}", algorithm.id())).label(
                                        fastdir_locale::t(&dialog_lang, algorithm_key(*algorithm)),
                                    )
                                }))
                                .on_change(move |ix: &usize, _, cx| {
                                    let algorithm = compression::ALGORITHMS
                                        .get(*ix)
                                        .copied()
                                        .unwrap_or(Algorithm::DEFAULT);
                                    set_entity
                                        .update(cx, |this, _| {
                                            this.compress_form.algorithm = algorithm;
                                        })
                                        .ok();
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(estimate),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(estimate_note),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Switch::new("compress-folders")
                                        .checked(form_folders)
                                        .on_change(move |checked: &bool, _, cx| {
                                            folder_entity
                                                .update(cx, |this, _| {
                                                    this.compress_form.folders_too = *checked;
                                                })
                                                .ok();
                                        }),
                                )
                                .child(div().text_xs().child(folders_label)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Switch::new("compress-skip").checked(form_skip).on_change(
                                    move |checked: &bool, _, cx| {
                                        skip_entity
                                            .update(cx, |this, _| {
                                                this.compress_form.skip_poorly_compressed =
                                                    *checked;
                                            })
                                            .ok();
                                    },
                                ))
                                .child(div().text_xs().child(skip_label)),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("compress-cancel")
                                .outline()
                                .label(cancel)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("compress-ok").primary().label(ok).on_click(
                            move |_, window, cx| {
                                let targets = targets.clone();
                                start_entity
                                    .update(cx, |this, cx| {
                                        let options = CompressOptions {
                                            algorithm: this.compress_form.algorithm,
                                            folders_too: this.compress_form.folders_too,
                                            skip_poorly_compressed: this
                                                .compress_form
                                                .skip_poorly_compressed,
                                        };
                                        this.run_compress(targets, options, window, cx);
                                    })
                                    .ok();
                                window.close_dialog(cx);
                            },
                        )),
                )
        });
    }

    /// Uncompress the selection without a dialog: there is nothing to choose.
    pub(super) fn uncompress_item(
        &mut self,
        _: &UncompressItem,
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
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let label = self.compress_label(&targets, false).into();
        self.enqueue(
            targets,
            PendingKind::Compress { options: None },
            label,
            window,
            cx,
        );
    }

    /// Queue compression of the selection.
    pub(super) fn run_compress(
        &mut self,
        targets: Vec<Target>,
        options: CompressOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.targets_on_disk(&targets) {
            self.notify_saved_missing(window, cx);
            return;
        }
        let label = self.compress_label(&targets, true).into();
        self.enqueue(
            targets,
            PendingKind::Compress {
                options: Some(options),
            },
            label,
            window,
            cx,
        );
    }

    fn compress_label(&self, targets: &[Target], compress: bool) -> String {
        let total = targets.len();
        let lang = self.settings.locale.language.clone();
        match (targets, compress) {
            ([one], true) => fastdir_locale::fmt(
                &lang,
                "compress.op_one",
                &[("name", &one.fs.display().to_string())],
            ),
            ([one], false) => fastdir_locale::fmt(
                &lang,
                "compress.op_one_un",
                &[("name", &one.fs.display().to_string())],
            ),
            (_, true) => {
                fastdir_locale::fmt(&lang, "compress.op_many", &[("n", &total.to_string())])
            }
            (_, false) => {
                fastdir_locale::fmt(&lang, "compress.op_many_un", &[("n", &total.to_string())])
            }
        }
    }

    /// Start a queued compression. `options == None` means uncompress.
    pub(super) fn start_compress(
        &mut self,
        targets: Vec<Target>,
        options: CompressOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_compression(targets, Some(options), window, cx);
    }

    pub(super) fn start_uncompress(
        &mut self,
        targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_compression(targets, None, window, cx);
    }

    fn start_compression(
        &mut self,
        targets: Vec<Target>,
        options: Option<CompressOptions>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let compress = options.is_some();
        let label = self.compress_label(&targets, compress);
        let total_items = targets.iter().map(|t| t.items).sum();
        let total_bytes = targets.iter().map(|t| t.size).sum();
        let progress = self.begin_op(label, total_items, total_bytes, true, cx);
        let paths: Vec<PathBuf> = targets.iter().map(|t| t.fs.clone()).collect();
        let job = cx.background_executor().spawn(async move {
            match options {
                Some(options) => compression::compress_tracked(&paths, options, &progress),
                None => compression::uncompress_tracked(&paths, &progress),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let report = job.await;
            this.update_in(cx, |this, window, cx| {
                this.end_op();
                this.finish_compression(targets, report, compress, window, cx);
                this.start_next_op(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Refresh the tree's `disk_size` values in place and report the result.
    fn finish_compression(
        &mut self,
        targets: Vec<Target>,
        report: CompressReport,
        compress: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut before = 0u64;
        let mut after = 0u64;
        let mut select = None;
        if let Some(tree) = &self.tree {
            let mut tree = tree.write();
            for target in &targets {
                let Some(path) = tree.find(&target.fs) else {
                    continue;
                };
                before += tree.get(&path).map(|n| n.disk_size).unwrap_or(0);
                if let Some(new_path) = tree.refresh_disk_sizes(&path, |file| {
                    // Items that vanished (saved scans) keep their old size
                    // instead of dropping to 0.
                    std::fs::symlink_metadata(file)
                        .ok()
                        .map(|_| scanner::allocated_size(file))
                }) {
                    after += tree.get(&new_path).map(|n| n.disk_size).unwrap_or(0);
                    select = Some(new_path);
                }
            }
        }
        if select.is_some() {
            self.tree_changed(select, cx);
        } else {
            cx.notify();
        }

        let saved = before.saturating_sub(after);
        if report.cancelled {
            window.push_notification(
                Notification::warning(self.tr_fmt(
                    "compress.cancel_some",
                    &[("done", &self.count(report.files))],
                ))
                .title(self.tr("compress.cancel_title")),
                cx,
            );
        } else if report.files > 0 {
            let body = if compress {
                match targets.as_slice() {
                    [one] => self.tr_fmt(
                        "compress.done_one",
                        &[
                            ("name", &one.fs.display().to_string()),
                            ("saved", &self.bytes(saved)),
                        ],
                    ),
                    _ => self.tr_fmt(
                        "compress.done_many",
                        &[
                            ("n", &self.count(report.files)),
                            ("saved", &self.bytes(saved)),
                        ],
                    ),
                }
            } else {
                match targets.as_slice() {
                    [one] => self.tr_fmt(
                        "compress.unc_done_one",
                        &[("name", &one.fs.display().to_string())],
                    ),
                    _ => self.tr_fmt(
                        "compress.unc_done_many",
                        &[("n", &self.count(report.files))],
                    ),
                }
            };
            window.push_notification(Notification::success(body), cx);
        } else if report.failed.is_empty() {
            window.push_notification(Notification::info(self.tr("compress.nothing")), cx);
        }
        if report.skipped > 0 {
            window.push_notification(
                Notification::info(
                    self.tr_fmt("compress.skipped", &[("n", &self.count(report.skipped))]),
                ),
                cx,
            );
        }
        if !report.failed.is_empty() {
            let title = if report.failed.len() == 1 {
                self.tr("compress.failed_title").to_string()
            } else {
                self.tr_fmt(
                    "compress.failed_body",
                    &[
                        ("done", &self.count(report.failed.len() as u64)),
                        (
                            "total",
                            &self.count(report.files + report.failed.len() as u64),
                        ),
                    ],
                )
            };
            let lang = self.settings.locale.language.clone();
            window.push_notification(
                Notification::error(failure_list_in(&report.failed, &lang)).title(title),
                cx,
            );
        }
    }
}

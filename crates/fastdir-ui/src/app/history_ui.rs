//! Scan history in the GUI: saving snapshots (on demand or after every
//! scan), the "Growth over time" dialog for a folder, and the Snapshots
//! card in Settings. Storage, retention and reading are in
//! `fastdir_core::history`.

use super::settings_ui::SettingsView;
use super::*;
use fastdir_core::export::ExportOptions;
use fastdir_core::history::{self, FolderSize, GrowthPoint, Retention};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;

/// "Keep per folder" choices in Settings (`0` = no limit).
const KEEP_CHOICES: [u32; 4] = [10, 30, 100, 0];
/// "Remove older than" choices in Settings, in days (`0` = never).
const AGE_CHOICES: [u32; 3] = [90, 365, 0];

/// One bar / row of the growth dialog.
#[derive(Clone)]
struct GrowthRow {
    when: i64,
    size: Option<FolderSize>,
    /// The snapshot file, or `None` for the current scan.
    file: Option<PathBuf>,
    /// The snapshot is of the root shown now, so it can be compared with it.
    comparable: bool,
}

impl FastDir {
    fn snapshot_retention(&self) -> Retention {
        let s = &self.settings.snapshots;
        Retention::from_limits(s.keep_count, s.max_age_days)
    }

    /// A finished, real scan is on screen (saved scans are already on disk).
    pub(super) fn can_snapshot(&self) -> bool {
        matches!(self.phase, Phase::Ready) && self.tree.is_some() && self.saved.is_none()
    }

    /// `…` → *Save snapshot*.
    pub(super) fn save_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_snapshot() || self.is_busy() {
            return;
        }
        if let Some(tree) = self.tree.clone() {
            self.write_snapshot(tree, true, false, window, cx);
        }
    }

    /// After a completed scan, when automatic snapshots are on.
    pub(super) fn auto_snapshot(
        &mut self,
        tree: Arc<RwLock<Tree>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.snapshots.auto_save && self.saved.is_none() {
            self.write_snapshot(tree, false, false, window, cx);
        }
    }

    /// Save `tree` as a snapshot in the background and apply retention.
    /// `manual` saves show progress and a confirmation; automatic ones only
    /// report failures. With `then_show`, the growth dialog opens afterwards.
    fn write_snapshot(
        &mut self,
        tree: Arc<RwLock<Tree>>,
        manual: bool,
        then_show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.settings.snapshots.effective_dir() else {
            window.push_notification(
                Notification::error(self.tr("history.no_dir")).title(self.tr("history.err_title")),
                cx,
            );
            return;
        };
        let retention = self.snapshot_retention();
        if manual {
            self.busy = Some(self.tr("history.saving").into());
            cx.notify();
        }
        let job = cx.background_executor().spawn(async move {
            let now = history::now_unix();
            let (file, root) = {
                let t = tree.read();
                let file = history::save(&t, now, &dir, &ExportOptions::default());
                (file, t.root_path.clone())
            };
            let file = file?;
            let pruned = history::prune(&dir, &root, &retention, now)?;
            Ok::<_, std::io::Error>((file, pruned.len()))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                if manual {
                    this.busy = None;
                }
                match result {
                    Ok((file, pruned)) => {
                        if manual && !then_show {
                            let mut body = this.tr_fmt(
                                "history.saved_body",
                                &[("path", &file.display().to_string())],
                            );
                            if pruned > 0 {
                                body.push('\n');
                                body.push_str(&this.tr_fmt(
                                    "history.pruned",
                                    &[("n", &this.count(pruned as u64))],
                                ));
                            }
                            window.push_notification(
                                Notification::success(body).title(this.tr("history.saved_title")),
                                cx,
                            );
                        }
                        if then_show {
                            this.show_growth(window, cx);
                        }
                    }
                    Err(error) => {
                        window.push_notification(
                            Notification::error(error.to_string())
                                .title(this.tr("history.err_title")),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `…` → *Growth over time…*: the selected folder (or the scan root)
    /// across every saved snapshot that contains it.
    pub(super) fn show_growth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Ready) || self.is_busy() {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let Some(dir) = self.settings.snapshots.effective_dir() else {
            window.push_notification(Notification::error(self.tr("history.no_dir")), cx);
            return;
        };
        // A selected folder, else the root. Files have no history worth a
        // chart, so they fall back to the root too.
        let (target, current, root) = {
            let t = tree.read();
            let selected = self
                .selected_path()
                .filter(|p| t.get(p).is_some_and(|n| n.is_dir()))
                .unwrap_or_default();
            let node = t.get(&selected).copied();
            let target = t.fs_path(&selected).unwrap_or_else(|| t.root_path.clone());
            let current = node.map(|n| FolderSize {
                size: n.size,
                disk_size: n.disk_size,
                files: u64::from(n.files),
                dirs: u64::from(n.dirs),
            });
            (target, current, t.root_path.clone())
        };
        // A saved scan on screen is one of the points already (or an older
        // document); only a live scan adds a "now" bar.
        let current = current.filter(|_| self.saved.is_none());
        self.busy = Some(self.tr("history.loading").into());
        cx.notify();
        let job = {
            let target = target.clone();
            let dir = dir.clone();
            cx.background_executor()
                .spawn(async move { history::growth(&dir, &target) })
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(points) => {
                        this.open_growth_dialog(target, root, dir, points, current, window, cx)
                    }
                    Err(error) => window.push_notification(
                        Notification::error(
                            this.tr_fmt("history.read_err", &[("error", &error.to_string())]),
                        ),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn open_growth_dialog(
        &mut self,
        target: PathBuf,
        root: PathBuf,
        dir: PathBuf,
        points: Vec<GrowthPoint>,
        current: Option<FolderSize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut rows: Vec<GrowthRow> = points
            .into_iter()
            .map(|p| GrowthRow {
                when: p.snapshot.scanned_at,
                size: p.size,
                comparable: history::relative_components(&p.snapshot.root, &root)
                    .is_some_and(|rel| rel.is_empty()),
                file: Some(p.snapshot.path),
            })
            .collect();
        let snapshot_count = rows.len();
        if let Some(size) = current {
            rows.push(GrowthRow {
                when: history::now_unix(),
                size: Some(size),
                file: None,
                comparable: false,
            });
        }
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.display().to_string());
        let title: SharedString = self.tr_fmt("history.title", &[("name", &name)]).into();
        let path_label: SharedString = self.display_fs_path(&target).into();
        let count_label: SharedString = self
            .tr_fmt(
                "history.count",
                &[("n", &self.count(snapshot_count as u64))],
            )
            .into();

        // Labels, computed here where the formatting helpers are.
        let sized: Vec<(i64, u64)> = rows
            .iter()
            .filter_map(|r| r.size.map(|s| (r.when, s.size)))
            .collect();
        let max = sized.iter().map(|(_, s)| *s).max().unwrap_or(0);
        let summary: Option<SharedString> = match (sized.first(), sized.last()) {
            (Some(&(from, first)), Some(&(to, last))) if sized.len() > 1 => {
                let delta = last as i64 - first as i64;
                let pct = self.signed_percent(delta, first);
                Some(
                    self.tr_fmt(
                        "history.summary",
                        &[
                            ("from", &self.date(from)),
                            ("to", &self.date(to)),
                            ("delta", &self.signed_bytes(delta)),
                            ("pct", &pct),
                        ],
                    )
                    .into(),
                )
            }
            _ => None,
        };
        let empty = sized.is_empty() || snapshot_count == 0;
        let empty_text: SharedString = self.tr("history.empty").into();
        let max_label: SharedString = self.bytes(max).into();
        let now_label = self.tr("history.now");
        let missing_label = self.tr("history.missing");

        struct Line {
            date: SharedString,
            size: SharedString,
            change: SharedString,
            grew: Option<bool>,
            fraction: f32,
            present: bool,
            is_now: bool,
            compare: Option<PathBuf>,
        }
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        let mut previous: Option<u64> = None;
        for row in &rows {
            let size = row.size.map(|s| s.size);
            let change = match (previous, size) {
                (Some(prev), Some(now)) => {
                    let delta = now as i64 - prev as i64;
                    Some(delta)
                }
                _ => None,
            };
            if size.is_some() {
                previous = size;
            }
            lines.push(Line {
                date: if row.file.is_none() {
                    now_label.into()
                } else {
                    self.date(row.when).into()
                },
                size: size
                    .map(|s| self.bytes(s))
                    .unwrap_or_else(|| missing_label.to_string())
                    .into(),
                change: change
                    .map(|d| self.signed_bytes(d))
                    .unwrap_or_default()
                    .into(),
                grew: change.filter(|d| *d != 0).map(|d| d > 0),
                fraction: match size {
                    Some(s) if max > 0 => (s as f64 / max as f64) as f32,
                    _ => 0.0,
                },
                present: size.is_some(),
                is_now: row.file.is_none(),
                compare: row
                    .file
                    .clone()
                    .filter(|_| row.comparable && self.tree.is_some()),
            });
        }
        let first_date = lines.first().map(|l| l.date.clone()).unwrap_or_default();
        let last_date = lines.last().map(|l| l.date.clone()).unwrap_or_default();
        let lines: Arc<[Line]> = lines.into();

        let col_date = self.tr("history.col_date");
        let col_size = self.tr("history.col_size");
        let col_change = self.tr("history.col_change");
        let compare_label = self.tr("history.compare");
        let open_label = self.tr("history.open_folder");
        let save_label = self.tr("history.save_now");
        let close_label = self.tr("action.close");
        let can_save = self.can_snapshot();
        let entity = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let border = theme.border;
            let bar = theme.primary;
            let now_bar = theme.success;
            let grew = theme.danger;
            let shrank = theme.success;

            let chart = h_flex()
                .w_full()
                .h(sp(150.))
                .items_end()
                .gap_px()
                .border_b_1()
                .border_color(border)
                .children(lines.iter().map(|line| {
                    let color = if line.is_now { now_bar } else { bar };
                    div()
                        .flex_1()
                        .min_w(px(2.))
                        .max_w(sp(36.))
                        .h_full()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .child(if line.present {
                            div()
                                .w_full()
                                .h(relative(line.fraction.max(0.01)))
                                .rounded_t_sm()
                                .bg(color)
                        } else {
                            div().w_full().h(px(2.)).bg(muted.opacity(0.4))
                        })
                }));

            let header = h_flex()
                .w_full()
                .gap_2()
                .px_2()
                .text_xs()
                .text_color(muted)
                .child(div().flex_1().child(col_date))
                .child(div().w(sp(110.)).text_right().child(col_size))
                .child(div().w(sp(110.)).text_right().child(col_change))
                .child(div().w(sp(90.)));
            let table_rows = lines.iter().enumerate().rev().map(|(i, line)| {
                let change_color = match line.grew {
                    Some(true) => grew,
                    Some(false) => shrank,
                    None => muted,
                };
                let compare = line.compare.clone().map(|file| {
                    let entity = entity.clone();
                    Button::new(("growth-compare", i))
                        .xsmall()
                        .ghost()
                        .label(compare_label)
                        .on_click(move |_, window, cx| {
                            let file = file.clone();
                            window.close_dialog(cx);
                            entity
                                .update(cx, |this, cx| this.compare_with_file(file, window, cx))
                                .ok();
                        })
                });
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .items_center()
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .when(line.is_now, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(line.date.clone()),
                    )
                    .child(
                        div()
                            .w(sp(110.))
                            .text_right()
                            .when(!line.present, |d| d.text_color(muted))
                            .child(line.size.clone()),
                    )
                    .child(
                        div()
                            .w(sp(110.))
                            .text_right()
                            .text_color(change_color)
                            .child(line.change.clone()),
                    )
                    .child(div().w(sp(90.)).flex().justify_end().children(compare))
            });

            let body = if empty {
                v_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(muted).child(path_label.clone()))
                    .child(div().text_sm().child(empty_text.clone()))
            } else {
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                div()
                                    .flex_1()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(path_label.clone()),
                            )
                            .child(count_label.clone()),
                    )
                    .when_some(summary.clone(), |d, summary| {
                        d.child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(summary),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(max_label.clone()))
                            .child(chart)
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(first_date.clone())
                                    .child(last_date.clone()),
                            ),
                    )
                    .child(
                        v_flex().gap_1().child(header).child(
                            v_flex()
                                .id("growth-rows")
                                .max_h(sp(220.))
                                .overflow_y_scroll()
                                .children(table_rows),
                        ),
                    )
            };

            let open_dir = dir.clone();
            let save_entity = entity.clone();
            dialog
                .title(title.clone())
                .w(sp(640.).to_pixels(window.rem_size()))
                .child(body)
                .footer(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .child(
                            Button::new("growth-open-folder")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label(open_label)
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&open_dir);
                                    fsops::open(&open_dir);
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .when(can_save, |d| {
                                    d.child(
                                        Button::new("growth-save")
                                            .outline()
                                            .label(save_label)
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                save_entity
                                                    .update(cx, |this, cx| {
                                                        this.save_snapshot_then_show(window, cx)
                                                    })
                                                    .ok();
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("growth-close")
                                        .outline()
                                        .label(close_label)
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        ),
                )
        });
    }

    /// *Save snapshot now* in the growth dialog: save, then show the
    /// updated chart.
    fn save_snapshot_then_show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_snapshot() || self.is_busy() {
            return;
        }
        if let Some(tree) = self.tree.clone() {
            self.write_snapshot(tree, true, true, window, cx);
        }
    }
}

impl SettingsView {
    /// The Snapshots card: automatic snapshots, retention and the folder.
    pub(super) fn render_snapshots_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let card_bg = theme.group_box;
        let (snapshots, lang) = {
            let app = self.owner.read(cx);
            (
                app.settings.snapshots.clone(),
                app.settings.locale.language.clone(),
            )
        };
        let tr = |key: &'static str| -> &'static str { fastdir_locale::t(&lang, key) };
        let caption = |text: String| div().text_xs().text_color(muted).child(text);
        let dir = snapshots.effective_dir();
        let dir_text = dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();

        let keep_ix = KEEP_CHOICES.iter().position(|n| *n == snapshots.keep_count);
        let age_ix = AGE_CHOICES
            .iter()
            .position(|n| *n == snapshots.max_age_days);
        let keep_radios: Vec<Radio> = KEEP_CHOICES
            .iter()
            .map(|n| {
                let label: SharedString = if *n == 0 {
                    tr("set.snapshots.keep_none").into()
                } else {
                    fastdir_locale::format_count(u64::from(*n), &lang).into()
                };
                Radio::new(("snap-keep", *n as usize)).label(label)
            })
            .collect();
        let age_labels = [
            tr("set.snapshots.age_90"),
            tr("set.snapshots.age_365"),
            tr("set.snapshots.age_none"),
        ];
        let age_radios: Vec<Radio> = age_labels
            .iter()
            .enumerate()
            .map(|(i, label)| Radio::new(("snap-age", i)).label(*label))
            .collect();

        let auto_owner = self.owner.clone();
        let keep_owner = self.owner.clone();
        let age_owner = self.owner.clone();

        v_flex()
            .px_5()
            .py_4()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .gap_4()
            .w_full()
            .child(
                div()
                    .pb_1()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("set.snapshots.title")),
            )
            .child(caption(fastdir_locale::fmt(
                &lang,
                "set.snapshots.caption",
                &[("dir", &dir_text)],
            )))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .w_full()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().child(tr("set.snapshots.auto")))
                            .child(caption(tr("set.snapshots.auto_cap").to_string())),
                    )
                    .child(
                        Switch::new("settings-snapshots-auto")
                            .checked(snapshots.auto_save)
                            .on_change(cx.listener(move |_, checked: &bool, _, cx| {
                                auto_owner.update(cx, |this, cx| {
                                    this.settings.snapshots.auto_save = *checked;
                                    this.settings.save();
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .child(div().text_xs().child(tr("set.snapshots.keep")))
            .child(
                RadioGroup::horizontal("settings-snapshots-keep")
                    .selected_index(keep_ix)
                    .children(keep_radios)
                    .on_change(cx.listener(move |_, ix: &usize, _, cx| {
                        if let Some(n) = KEEP_CHOICES.get(*ix).copied() {
                            keep_owner.update(cx, |this, cx| {
                                this.settings.snapshots.keep_count = n;
                                this.settings.save();
                                cx.notify();
                            });
                        }
                    })),
            )
            .child(div().text_xs().child(tr("set.snapshots.age")))
            .child(
                RadioGroup::horizontal("settings-snapshots-age")
                    .selected_index(age_ix)
                    .children(age_radios)
                    .on_change(cx.listener(move |_, ix: &usize, _, cx| {
                        if let Some(days) = AGE_CHOICES.get(*ix).copied() {
                            age_owner.update(cx, |this, cx| {
                                this.settings.snapshots.max_age_days = days;
                                this.settings.save();
                                cx.notify();
                            });
                        }
                    })),
            )
            .when_some(dir, |d, dir| {
                d.child(
                    h_flex().child(
                        Button::new("settings-snapshots-open")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label(tr("set.snapshots.open"))
                            .on_click(move |_, _, _| {
                                let _ = std::fs::create_dir_all(&dir);
                                fsops::open(&dir);
                            }),
                    ),
                )
            })
            .into_any_element()
    }
}

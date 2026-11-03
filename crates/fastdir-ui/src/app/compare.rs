//! Comparing the current scan with one saved earlier: loading the baseline,
//! building the diff off the UI thread, the comparison banner and the
//! "Removed" list's ordering.

use super::*;
use fastdir_core::diff::{self, ScanDiff};
use fastdir_core::export::{self, ImportedScan};

/// A finished comparison with the saved scan currently shown.
pub(super) struct Comparison {
    pub diff: Arc<ScanDiff>,
    /// Saved JSON the baseline was read from.
    pub baseline: PathBuf,
    /// Unix seconds reported by that document (`0` when unknown).
    pub baseline_at: i64,
    /// Indices into `diff.removed()` in the active sort order.
    pub removed_order: Vec<usize>,
}

impl FastDir {
    /// `Compare with…`: pick a saved scan of the current folder and show what
    /// changed. The current tree is the "new" side; the opened file is the
    /// baseline.
    pub(super) fn compare_with(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Ready) || self.tree.is_none() || self.is_busy() {
            return;
        }
        let prompt = self.tr("compare.pick");
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    this.start_comparison(path, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Read `path` in the background, then diff it against the current tree.
    fn start_comparison(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The picker is modal, but a scan can still have started underneath.
        if !matches!(self.phase, Phase::Ready) || self.is_busy() {
            return;
        }
        let Some(current) = self.tree.clone() else {
            return;
        };
        self.busy = Some(self.tr("compare.opening").into());
        cx.notify();
        let job = cx.background_executor().spawn(async move {
            let result = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| {
                    export::import_json_with_meta(std::io::BufReader::new(file))
                        .map_err(|e| e.to_string())
                });
            (path, result)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (path, result) = job.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(scan) => this.compute_comparison(scan, path, current, window, cx),
                Err(error) => {
                    this.busy = None;
                    window.push_notification(
                        Notification::error(this.tr_fmt(
                            "compare.err_body",
                            &[("path", &path.display().to_string()), ("error", &error)],
                        ))
                        .title(this.tr("compare.err_title")),
                        cx,
                    );
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Build the diff for `scan` against `current` on a background thread.
    fn compute_comparison(
        &mut self,
        scan: ImportedScan,
        baseline: PathBuf,
        current: Arc<RwLock<Tree>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ImportedScan { tree, scanned_at } = scan;
        self.busy = Some(self.tr("compare.computing").into());
        cx.notify();
        let new_tree = current.clone();
        let job = cx.background_executor().spawn(async move {
            let new_tree = new_tree.read();
            diff::build(&tree, &new_tree)
        });
        cx.spawn_in(window, async move |this, cx| {
            let diff = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                // A new scan or a different view replaced the tree meanwhile.
                if !this
                    .tree
                    .as_ref()
                    .is_some_and(|tree| Arc::ptr_eq(tree, &current))
                {
                    cx.notify();
                    return;
                }
                this.install_comparison(diff, baseline, scanned_at, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn install_comparison(
        &mut self,
        diff: ScanDiff,
        baseline: PathBuf,
        baseline_at: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mismatch = !diff.roots_match();
        let old_root = diff.old_root().to_path_buf();
        let new_root = diff.new_root().to_path_buf();
        let totals = *diff.totals();
        let diff = Arc::new(diff);
        let mut comparison = Comparison {
            removed_order: (0..diff.removed().len()).collect(),
            diff,
            baseline,
            baseline_at,
        };
        self.sort_removed_order(&mut comparison);
        let removed_count = comparison.removed_order.len();
        let diff = comparison.diff.clone();
        self.compare = Some(comparison);

        // "Grew the most" is the question that started the comparison.
        self.sort = (SortKey::Delta, true);
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.selected = None;
        self.removed_selected = (removed_count > 0).then_some(0);
        self.rebuild_rows();
        if self.selected.is_none() && !self.rows.is_empty() {
            self.selected = Some(0);
        }
        self.anchor = self.selected_path();
        self.settings.treemap.color_mode = TreemapColorMode::Change;
        self.settings.save();
        self.treemap.update(cx, |t, cx| {
            t.set_change(Some(diff.clone()), cx);
            t.invalidate(cx);
        });
        self.refresh_treemap_options(cx);

        let pct = if totals.old_size > 0 {
            self.signed_percent(totals.net(), totals.old_size)
        } else {
            "—".to_string()
        };
        let message = self.tr_fmt(
            "compare.done_body",
            &[
                ("old", &self.display_fs_path(&old_root)),
                ("new", &self.display_fs_path(&new_root)),
                ("delta", &self.signed_bytes(totals.net())),
                ("pct", &pct),
            ],
        );
        let message = if mismatch {
            format!(
                "{message}\n{}",
                self.tr_fmt(
                    "compare.mismatch",
                    &[("root", &self.display_fs_path(&old_root))],
                )
            )
        } else {
            message
        };
        window.push_notification(
            Notification::new()
                .message(message)
                .with_type(if mismatch {
                    NotificationType::Warning
                } else {
                    NotificationType::Info
                })
                .title(self.tr("compare.done_title")),
            cx,
        );
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Switch the treemap between change colours and the normal colours.
    /// Only available while a comparison is active.
    pub(super) fn toggle_treemap_change_colors(&mut self, cx: &mut Context<Self>) {
        if self.compare.is_none() {
            return;
        }
        self.settings.treemap.color_mode =
            if self.settings.treemap.color_mode == TreemapColorMode::Change {
                TreemapColorMode::Extension
            } else {
                TreemapColorMode::Change
            };
        self.settings.save();
        self.refresh_treemap_options(cx);
        cx.notify();
    }

    /// Stop comparing, restoring the normal columns and treemap colours.
    pub(super) fn clear_comparison(&mut self, cx: &mut Context<Self>) {
        if self.compare.take().is_none() {
            return;
        }
        self.removed_selected = None;
        self.removed_scroll.scroll_to_item(0, ScrollStrategy::Top);
        if self.tab == ListTab::Removed {
            self.tab = ListTab::Tree;
        }
        self.treemap.update(cx, |t, cx| t.set_change(None, cx));
        if self.settings.treemap.color_mode == TreemapColorMode::Change {
            self.settings.treemap.color_mode = TreemapColorMode::Extension;
            self.settings.save();
            self.refresh_treemap_options(cx);
        }
        if matches!(self.sort.0, SortKey::Delta | SortKey::Status) {
            self.sort = (SortKey::Size, true);
            self.settings.list.sort.column = SortKey::Size;
            self.settings.list.sort.descending = true;
            self.settings.save();
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Banner above the results: what the current tree is compared with, the
    /// root-mismatch warning and a button to stop comparing.
    pub(super) fn render_compare_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let compare = self.compare.as_ref()?;
        let theme = cx.theme();
        let when = if compare.baseline_at > 0 {
            self.date(compare.baseline_at)
        } else {
            "—".to_string()
        };
        let path = self.display_fs_path(&compare.baseline);
        let text = self.tr_fmt("compare.banner", &[("path", &path), ("when", &when)]);
        let warning = (!compare.diff.roots_match()).then(|| {
            self.tr_fmt(
                "compare.mismatch",
                &[("root", &self.display_fs_path(compare.diff.old_root()))],
            )
        });
        Some(
            h_flex()
                .flex_shrink_0()
                .w_full()
                .px_3()
                .py_1()
                .gap_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.primary.opacity(0.08))
                .child(
                    v_flex()
                        .flex_1()
                        .overflow_hidden()
                        .gap_1()
                        .child(
                            div()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(text),
                        )
                        .when_some(warning, |d, warning| {
                            d.child(
                                div()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(theme.warning)
                                    .child(warning),
                            )
                        }),
                )
                .child(
                    Button::new("compare-clear")
                        .small()
                        .outline()
                        .icon(IconName::Close)
                        .label(self.tr("compare.clear"))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_comparison(cx))),
                )
                .into_any_element(),
        )
    }

    /// `+1.2 GB` / `-340 MB`, using the configured size units.
    pub(super) fn signed_bytes(&self, delta: i64) -> String {
        if delta == 0 {
            return self.bytes(0);
        }
        let sign = if delta > 0 { "+" } else { "-" };
        format!("{sign}{}", self.bytes(delta.unsigned_abs()))
    }

    /// `+12.3%` / `-4.0%`, relative to `whole` (the old size).
    pub(super) fn signed_percent(&self, delta: i64, whole: u64) -> String {
        if whole == 0 {
            return "—".to_string();
        }
        let value = delta as f64 * 100.0 / whole as f64;
        let text = format!("{value:+.1}%");
        let sep = fastdir_locale::decimal_sep(&self.settings.locale.language);
        if sep == '.' {
            text
        } else {
            text.replace('.', &sep.to_string())
        }
    }

    /// Re-sort the removed list's index order for the current `removed_sort`.
    fn sort_removed_order(&self, comparison: &mut Comparison) {
        let diff = comparison.diff.clone();
        let removed = diff.removed();
        let (key, desc) = self.removed_sort;
        let mut order = std::mem::take(&mut comparison.removed_order);
        order.sort_by(|&a, &b| {
            let (a, b) = (&removed[a], &removed[b]);
            let ord = match key {
                SortKey::Name => a.path.to_lowercase().cmp(&b.path.to_lowercase()),
                SortKey::Disk => a.disk_size.cmp(&b.disk_size),
                SortKey::Modified => a.modified.cmp(&b.modified),
                // Every removed item is a loss, so "delta" and "size" agree.
                _ => a.size.cmp(&b.size),
            }
            .then_with(|| a.path.cmp(&b.path));
            if desc { ord.reverse() } else { ord }
        });
        comparison.removed_order = order;
    }

    /// Header click in the "Removed" tab.
    pub(super) fn set_removed_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.removed_sort.0 == key {
            self.removed_sort.1 = !self.removed_sort.1;
        } else {
            self.removed_sort = (key, key != SortKey::Name);
        }
        if let Some(mut comparison) = self.compare.take() {
            self.sort_removed_order(&mut comparison);
            self.compare = Some(comparison);
        }
        if let Some(ix) = self.removed_selected {
            self.removed_scroll
                .scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub(super) fn select_removed_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        if compare.removed_order.is_empty() {
            return;
        }
        let ix = ix.min(compare.removed_order.len() - 1);
        self.removed_selected = Some(ix);
        self.marked.clear();
        self.anchor = None;
        self.removed_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.sync_treemap_selection(cx);
        cx.notify();
    }
}

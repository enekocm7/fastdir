//! The status bar at the bottom of the window.

use super::*;

impl FastDir {
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let left: SharedString = match &self.phase {
            Phase::Home => self
                .tr_fmt("status.drives", &[("n", &self.count(self.drives.len() as u64))])
                .into(),
            Phase::Scanning {
                progress,
                started,
                expected,
                ..
            } => {
                let elapsed = started.elapsed();
                let files = progress.files.load(Ordering::Relaxed);
                let rate = files as f64 / elapsed.as_secs_f64().max(0.001);
                let bytes = progress.bytes.load(Ordering::Relaxed);
                let pct = expected.map(|e| (bytes as f64 / e as f64 * 100.0).min(99.0));
                let elapsed_s = format::duration(elapsed);
                match pct {
                    Some(pct) => self
                        .tr_fmt(
                            "status.scanning_pct",
                            &[
                                ("pct", &format!("{pct:.0}")),
                                ("rate", &self.count(rate as u64)),
                                ("elapsed", &elapsed_s),
                            ],
                        )
                        .into(),
                    None => self
                        .tr_fmt(
                            "status.scanning",
                            &[
                                ("rate", &self.count(rate as u64)),
                                ("elapsed", &elapsed_s),
                            ],
                        )
                        .into(),
                }
            }
            Phase::Ready if self.is_multi() => {
                // Folders inside a selected folder are not counted twice.
                let size: u64 = self.targets().iter().map(|t| t.size).sum();
                self.tr_fmt(
                    "status.selected",
                    &[("n", &self.count(self.marked.len() as u64)), ("size", &self.bytes(size))],
                )
                .into()
            }
            Phase::Ready => self
                .selected_fs_path()
                .map(|(p, fs, _)| {
                    let size = self
                        .tree
                        .as_ref()
                        .and_then(|t| t.read().get(&p).map(|n| n.size))
                        .unwrap_or(0);
                    format!("{}  ·  {}", self.display_fs_path(&fs), self.bytes(size))
                })
                .unwrap_or_default()
                .into(),
        };
        let right: SharedString = match (&self.phase, &self.tree) {
            (Phase::Scanning { progress, .. }, _) => progress.current.lock().clone().into(),
            (Phase::Ready, Some(t)) => {
                let t = t.read();
                let (elapsed, cancelled) = self.last_scan.unwrap_or_default();
                let summary = if t.is_drive_scan() {
                    let scanned = t.scanned_bytes();
                    let used = t.used_bytes();
                    let free = t.free_bytes().unwrap_or(0);
                    let unknown = t.unknown_bytes().unwrap_or(0);
                    let disk = t.scanned_disk_bytes();
                    let root = t.root();
                    let files = self.count(root.files as u64);
                    let folders = self.count(root.dirs as u64);
                    if unknown > 0 {
                        self.tr_fmt(
                            "status.drive_full",
                            &[
                                ("files", &files),
                                ("folders", &folders),
                                ("scanned", &self.bytes(scanned)),
                                ("used", &self.bytes(used)),
                                ("disk", &self.bytes(disk)),
                                ("free", &self.bytes(free)),
                                ("unknown", &self.bytes(unknown)),
                            ],
                        )
                    } else {
                        self.tr_fmt(
                            "status.drive",
                            &[
                                ("files", &files),
                                ("folders", &folders),
                                ("scanned", &self.bytes(scanned)),
                                ("used", &self.bytes(used)),
                                ("disk", &self.bytes(disk)),
                                ("free", &self.bytes(free)),
                            ],
                        )
                    }
                } else {
                    let drive = self
                        .drives
                        .iter()
                        .find(|d| t.root_path.starts_with(&d.path));
                    let suffix = drive
                        .map(|d| {
                            self.tr_fmt("status.free_suffix", &[("free", &self.bytes(d.free))])
                        })
                        .unwrap_or_default();
                    self.tr_fmt(
                        "status.dir",
                        &[
                            ("files", &self.count(t.root().files as u64)),
                            ("folders", &self.count(t.root().dirs as u64)),
                            ("size", &self.bytes(t.root().size)),
                            ("disk", &self.bytes(t.root().disk_size)),
                            ("suffix", &suffix),
                        ],
                    )
                };
                let suffix = if cancelled {
                    self.tr("status.incomplete")
                } else {
                    ""
                };
                format!(
                    "{summary}{}",
                    self.tr_fmt(
                        "status.scanned_in",
                        &[("dur", &format::duration(elapsed)), ("suffix", suffix)],
                    )
                )
                .into()
            }
            _ => "".into(),
        };
        h_flex()
            .h(sp(26.))
            .flex_shrink_0()
            .px_3()
            .gap_4()
            .justify_between()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(left),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(relative(0.6))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(right),
            )
    }
}

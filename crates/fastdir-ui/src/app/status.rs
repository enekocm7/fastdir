//! The status bar at the bottom of the window.

use super::*;

impl FastDir {
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let left: SharedString = match &self.phase {
            Phase::Home => format!("{} drives detected", self.drives.len()).into(),
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
                match pct {
                    Some(pct) => format!(
                        "Scanning… {:.0}% · {}/s · {}",
                        pct,
                        format::count(rate as u64),
                        format::duration(elapsed)
                    ),
                    None => format!(
                        "Scanning… {}/s · {}",
                        format::count(rate as u64),
                        format::duration(elapsed)
                    ),
                }
                .into()
            }
            Phase::Ready if self.is_multi() => {
                // Folders inside a selected folder are not counted twice.
                let size: u64 = self.targets().iter().map(|t| t.size).sum();
                format!(
                    "{} items selected  ·  {}",
                    format::count(self.marked.len() as u64),
                    self.bytes(size)
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
                    format!("{}  ·  {}", fs.display(), self.bytes(size))
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
                    if unknown > 0 {
                        format!(
                            "{} files · {} folders · {} scanned of {} used ({} on disk, {} free, {} unknown)",
                            format::count(t.root().files as u64),
                            format::count(t.root().dirs as u64),
                            self.bytes(scanned),
                            self.bytes(used),
                            self.bytes(disk),
                            self.bytes(free),
                            self.bytes(unknown),
                        )
                    } else {
                        format!(
                            "{} files · {} folders · {} scanned of {} used ({} on disk, {} free)",
                            format::count(t.root().files as u64),
                            format::count(t.root().dirs as u64),
                            self.bytes(scanned),
                            self.bytes(used),
                            self.bytes(disk),
                            self.bytes(free),
                        )
                    }
                } else {
                    let drive = self
                        .drives
                        .iter()
                        .find(|d| t.root_path.starts_with(&d.path));
                    format!(
                        "{} files · {} folders · {} ({} on disk){}",
                        format::count(t.root().files as u64),
                        format::count(t.root().dirs as u64),
                        self.bytes(t.root().size),
                        self.bytes(t.root().disk_size),
                        drive
                            .map(|d| format!(" · {} free", self.bytes(d.free)))
                            .unwrap_or_default(),
                    )
                };
                format!(
                    "{summary}{}",
                    format!(
                        " · scanned in {}{}",
                        format::duration(elapsed),
                        if cancelled { " (incomplete)" } else { "" }
                    )
                )
                .into()
            }
            _ => "".into(),
        };
        h_flex()
            .h(px(26.))
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

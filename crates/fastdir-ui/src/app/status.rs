//! The status bar at the bottom of the window.

use super::*;

impl FastDir {
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let left: SharedString = match &self.phase {
            Phase::Home => format!("{} drives detected", self.drives.len()).into(),
            Phase::Scanning { progress, .. } => format!(
                "Scanning… {} files, {}",
                format::count(progress.files.load(Ordering::Relaxed)),
                format::bytes(progress.bytes.load(Ordering::Relaxed))
            )
            .into(),
            Phase::Ready if self.is_multi() => {
                // Folders inside a selected folder are not counted twice.
                let size: u64 = self.targets().iter().map(|t| t.size).sum();
                format!(
                    "{} items selected  ·  {}",
                    format::count(self.marked.len() as u64),
                    format::bytes(size)
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
                    format!("{}  ·  {}", fs.display(), format::bytes(size))
                })
                .unwrap_or_default()
                .into(),
        };
        let right: SharedString = match (&self.phase, &self.tree) {
            (Phase::Ready, Some(t)) => {
                let t = t.read();
                let (elapsed, cancelled) = self.last_scan.unwrap_or_default();
                let drive = self
                    .drives
                    .iter()
                    .find(|d| t.root_path.starts_with(&d.path));
                format!(
                    "{} files · {} folders · {}{}{}",
                    format::count(t.root.files as u64),
                    format::count(t.root.dirs as u64),
                    format::bytes(t.root.size),
                    drive
                        .map(|d| format!(" · {} free", format::bytes(d.free)))
                        .unwrap_or_default(),
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
            .child(div().flex_shrink_0().child(right))
    }
}

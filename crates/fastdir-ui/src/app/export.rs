//! Export dialog: write the current scan (or the selected subtree) to a
//! CSV or JSON file, for reports and for comparing two scans.

use super::*;
use fastdir_core::export::{self, ExportFormat, ExportOptions};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;

/// Which part of the scan to export.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum ExportScope {
    #[default]
    Whole,
    Selected,
}

/// File format picked in the dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum ExportFileFormat {
    #[default]
    Csv,
    Json,
}

impl ExportFileFormat {
    fn core(self) -> ExportFormat {
        match self {
            Self::Csv => ExportFormat::Csv,
            Self::Json => ExportFormat::Json,
        }
    }
}

/// The dialog's draft, kept on [`FastDir`] so the dialog (rebuilt every
/// frame) stays in sync with the radio buttons and the switch.
#[derive(Clone, Copy, Debug)]
pub(super) struct ExportForm {
    pub scope: ExportScope,
    pub format: ExportFileFormat,
    pub include_files: bool,
}

impl Default for ExportForm {
    fn default() -> Self {
        Self {
            scope: ExportScope::Whole,
            format: ExportFileFormat::Csv,
            include_files: true,
        }
    }
}

impl FastDir {
    pub(super) fn open_export(
        &mut self,
        _: &ExportResults,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.phase, Phase::Ready) || self.tree.is_none() || self.is_busy() {
            return;
        }
        self.export_form = ExportForm {
            scope: if self.export_selection_valid() {
                ExportScope::Selected
            } else {
                ExportScope::Whole
            },
            format: ExportFileFormat::Csv,
            include_files: true,
        };
        let depth = cx.new(|cx| InputState::new(window, cx).placeholder("Empty = unlimited"));
        let entity = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _window, cx| {
            let theme = cx.theme();
            let form = entity
                .upgrade()
                .map(|e| e.read(cx).export_form)
                .unwrap_or_default();
            let (scope, valid) = entity
                .upgrade()
                .map(|e| {
                    let view = e.read(cx);
                    (view.export_form.scope, view.export_selection_valid())
                })
                .unwrap_or((ExportScope::Whole, false));
            let selected_label: SharedString = entity
                .upgrade()
                .and_then(|e| {
                    let view = e.read(cx);
                    let path = view.selected_path()?;
                    let tree = view.tree.as_ref()?.read();
                    Some(tree.fs_path(&path)?.display().to_string().into())
                })
                .unwrap_or_else(|| "Selected folder".into());
            let field = |label: &'static str| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label)
            };
            // Each control writes back to the draft on `FastDir`; the dialog
            // is rebuilt every frame, so it shows the new value at once.
            let scope_entity = entity.clone();
            let format_entity = entity.clone();
            let files_entity = entity.clone();
            let depth_export = depth.clone();
            let entity_export = entity.clone();
            dialog
                .title("Export results")
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(field("What to export"))
                        .child(
                            RadioGroup::horizontal("export-scope")
                                .selected_index(Some(match scope {
                                    ExportScope::Whole => 0,
                                    ExportScope::Selected => 1,
                                }))
                                .child(Radio::new("export-scope-whole").label("Whole scan"))
                                .child(
                                    Radio::new("export-scope-selected")
                                        .label(selected_label)
                                        .disabled(!valid),
                                )
                                .on_change(move |ix: &usize, _window, cx| {
                                    scope_entity
                                        .update(cx, |this, _| {
                                            this.export_form.scope = if *ix == 1 {
                                                ExportScope::Selected
                                            } else {
                                                ExportScope::Whole
                                            };
                                        })
                                        .ok();
                                }),
                        )
                        .child(field("File format"))
                        .child(
                            RadioGroup::horizontal("export-format")
                                .selected_index(Some(match form.format {
                                    ExportFileFormat::Csv => 0,
                                    ExportFileFormat::Json => 1,
                                }))
                                .child(Radio::new("export-format-csv").label("CSV (spreadsheet)"))
                                .child(
                                    Radio::new("export-format-json").label("JSON (re-importable)"),
                                )
                                .on_change(move |ix: &usize, _window, cx| {
                                    format_entity
                                        .update(cx, |this, _| {
                                            this.export_form.format = if *ix == 1 {
                                                ExportFileFormat::Json
                                            } else {
                                                ExportFileFormat::Csv
                                            };
                                        })
                                        .ok();
                                }),
                        )
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .w_full()
                                .gap_4()
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(div().text_xs().child("Include files"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("Off exports folders only."),
                                        ),
                                )
                                .child(
                                    Switch::new("export-include-files")
                                        .checked(form.include_files)
                                        .on_change(move |checked: &bool, _window, cx| {
                                            files_entity
                                                .update(cx, |this, _| {
                                                    this.export_form.include_files = *checked;
                                                })
                                                .ok();
                                        }),
                                ),
                        )
                        .child(field("Max depth (levels below the exported folder)"))
                        .child(Input::new(&depth).small()),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("export-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("export-ok")
                                .primary()
                                .icon(IconName::Download)
                                .label("Choose folder & Export")
                                .on_click(move |_, window, cx| {
                                    if do_export(
                                        entity_export.clone(),
                                        depth_export.clone(),
                                        window,
                                        cx,
                                    ) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    /// Whether "Selected folder" currently points at something exportable.
    fn export_selection_valid(&self) -> bool {
        !self.is_multi()
            && !self.selected_is_special()
            && self.selected_path().is_some_and(|p| !p.is_empty())
    }
}

/// Validate the dialog and start the export. Returns whether the dialog may
/// close: validation failures keep it open, otherwise a folder picker takes
/// over and the file is written on a background thread.
fn do_export(
    entity: WeakEntity<FastDir>,
    depth_input: Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(view) = entity.upgrade() else {
        return true;
    };
    let (form, depth_text) = (
        view.read(cx).export_form,
        depth_input.read(cx).value().to_string(),
    );
    let max_depth = match depth_text.trim() {
        "" => None,
        text => match text.parse::<usize>() {
            Ok(n) => Some(n),
            Err(_) => {
                window.push_notification(
                    Notification::warning("Max depth must be a whole number, or empty.")
                        .title("Export not started"),
                    cx,
                );
                return false;
            }
        },
    };
    let (tree, scope_path, base_name) = {
        let view = view.read(cx);
        let Some(tree) = view.tree.clone() else {
            return true;
        };
        let scope_path = match form.scope {
            ExportScope::Whole => Vec::new(),
            ExportScope::Selected => match view.selected_path() {
                Some(p) if view.export_selection_valid() => p,
                _ => {
                    window.push_notification(
                        Notification::warning("Select a folder first, or export the whole scan.")
                            .title("Export not started"),
                        cx,
                    );
                    return false;
                }
            },
        };
        let base_name = {
            let t = tree.read();
            match form.scope {
                ExportScope::Whole => t
                    .root_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "scan".to_string()),
                ExportScope::Selected => scope_path
                    .last()
                    .and_then(|_| t.get(&scope_path))
                    .map(|n| t.name(n).to_owned())
                    .unwrap_or_else(|| "folder".to_string()),
            }
        };
        (tree, scope_path, base_name)
    };
    let file_format = form.format.core();
    let options = ExportOptions {
        max_depth,
        include_files: form.include_files,
    };
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Export destination".into()),
    });
    view.update(cx, |_, cx| {
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let dest = dir.join(export_filename(&base_name, file_format));
            let job = cx.background_executor().spawn(async move {
                let t = tree.read();
                export::export_to_file(&t, &scope_path, file_format, &options, &dest)
                    .map(|rows| (rows, dest))
                    .map_err(|e| e.to_string())
            });
            let result = job.await;
            this.update_in(cx, |_, window, cx| match result {
                Ok((rows, dest)) => {
                    window.push_notification(
                        Notification::success(format!(
                            "Exported {} {} to {}",
                            format::count(rows as u64),
                            if rows == 1 { "row" } else { "rows" },
                            dest.display()
                        )),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(Notification::error(e).title("Export failed"), cx);
                }
            })
            .ok();
        })
        .detach();
    });
    true
}

/// `fastdir-<folder>-<unix>.csv`, safe for file systems.
fn export_filename(base_name: &str, format: ExportFormat) -> String {
    let mut stem: String = base_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if stem.is_empty() {
        stem = "scan".to_string();
    }
    stem.truncate(60);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("fastdir-{stem}-{now}.{}", format.extension())
}

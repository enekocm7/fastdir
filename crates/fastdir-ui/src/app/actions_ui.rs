//! User-defined actions: the context-menu *Run* submenu, confirmation,
//! execution queue, output dialog and the Settings editor.

use super::settings_ui::SettingsView;
use super::*;
use fastdir_core::actions::{ActionContext, CommandLine, RunOutput, matches_pattern, split_args};
use fastdir_settings::{ActionAppliesTo, ActionDef};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;

/// The *Run* submenu contents, snapshotted while the app is only being read.
pub(super) struct RunMenu {
    label: &'static str,
    items: Vec<(usize, String)>,
}

impl RunMenu {
    /// Build the submenu (needs `&mut App`, so it happens after the read).
    pub(super) fn append(
        self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut App,
        entity: WeakEntity<FastDir>,
    ) -> PopupMenu {
        let submenu = PopupMenu::build(window, cx, move |sub, _, _| {
            let mut sub = sub;
            for (ix, label) in self.items {
                let entity = entity.clone();
                sub = sub.item(PopupMenuItem::new(label).icon(IconName::Play).on_click(
                    move |_, window, cx| {
                        entity
                            .update(cx, |this, cx| this.run_action_ix(ix, window, cx))
                            .ok();
                    },
                ));
            }
            sub
        });
        menu.separator()
            .item(PopupMenuItem::submenu(self.label, submenu).icon(IconName::Play))
    }
}

/// Build the whole right-click menu: the fixed items, the dynamic *Run*
/// submenu in the middle, then the rest.
pub(super) fn build_context_menu(
    entity: Entity<FastDir>,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let (menu, run) = {
        let this = entity.read(cx);
        (this.context_menu_head(menu), this.run_menu_data())
    };
    let menu = match run {
        Some(run) => run.append(menu, window, cx, entity.downgrade()),
        None => menu,
    };
    let this = entity.read(cx);
    this.context_menu_tail(menu, cx)
}

/// One queued action for one selected item.
#[derive(Clone)]
pub(super) struct ActionRun {
    /// The action's configured name, for notifications and the output title.
    pub action_name: String,
    /// The item's name, shown in the queue.
    pub item: String,
    pub line: CommandLine,
    pub show_output: bool,
    pub refresh_after: bool,
    /// Folder to re-scan when `refresh_after` is set.
    pub target: PathBuf,
}

impl FastDir {
    /// Enabled actions that apply to one item, in config order.
    fn actions_for(
        &self,
        path: &std::path::Path,
        is_dir: bool,
        name: &str,
    ) -> Vec<(usize, ActionDef)> {
        self.settings
            .actions
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, action)| action.is_usable())
            .filter(|(_, action)| match action.applies_to {
                ActionAppliesTo::Dir => is_dir,
                ActionAppliesTo::File => !is_dir,
                ActionAppliesTo::Both => true,
            })
            .filter(|(_, action)| {
                action
                    .match_pattern
                    .as_deref()
                    .is_none_or(|pattern| matches_pattern(pattern, path, is_dir, name))
            })
            .collect()
    }

    /// Actions matching the current selection.
    pub(super) fn selected_actions(&self) -> Vec<(usize, ActionDef)> {
        if self.selected_is_special() {
            return Vec::new();
        }
        let Some((_, fs, is_dir)) = self.selected_fs_path() else {
            return Vec::new();
        };
        if !self.can_touch_selected() {
            return Vec::new();
        }
        let name = ActionContext::new(&fs, is_dir).name;
        self.actions_for(&fs, is_dir, &name)
    }

    /// Snapshot the *Run* submenu contents while the app can only be read.
    ///
    /// The menu items are built with click handlers rather than actions, so
    /// the submenu itself is created afterwards, once the `FastDir` read
    /// guard in the context-menu closure has been dropped (building a popup
    /// needs `&mut App`).
    pub(super) fn run_menu_data(&self) -> Option<RunMenu> {
        let actions = self.selected_actions();
        if actions.is_empty() {
            return None;
        }
        let mod_key = platform::mod_key();
        let items = actions
            .iter()
            .map(|(ix, action)| {
                let label = match action.shortcut.filter(|n| (1..=9).contains(n)) {
                    Some(n) => format!("{}   {mod_key}+{n}", action.name),
                    None => action.name.clone(),
                };
                (*ix, label)
            })
            .collect();
        Some(RunMenu {
            label: self.tr("menu.run"),
            items,
        })
    }

    /// Ctrl/Cmd+1 … 9: run the first matching action bound to that key.
    pub(super) fn run_action_shortcut(
        &mut self,
        n: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = self
            .selected_actions()
            .into_iter()
            .find(|(_, action)| action.shortcut == Some(n))
            .map(|(ix, _)| ix);
        if let Some(ix) = ix {
            self.run_action_ix(ix, window, cx);
        }
    }

    pub(super) fn run_action_ix(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.settings.actions.get(ix).cloned() else {
            return;
        };
        if !action.is_usable() {
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
        let runs: Vec<ActionRun> = targets
            .into_iter()
            .map(|target| {
                let ctx = ActionContext::new(&target.fs, target.is_dir);
                let line = CommandLine::expand(
                    &action.command,
                    &action.args,
                    &action.working_dir,
                    action.shell,
                    &ctx,
                );
                ActionRun {
                    action_name: action.name.clone(),
                    item: ctx.name.clone(),
                    line,
                    show_output: action.show_output,
                    refresh_after: action.refresh_after,
                    target: if target.is_dir { target.fs } else { ctx.parent },
                }
            })
            .collect();
        if action.confirm {
            self.confirm_actions(runs, window, cx);
        } else {
            self.enqueue_actions(runs, window, cx);
        }
    }

    /// Show every exact command line before anything runs.
    fn confirm_actions(
        &mut self,
        runs: Vec<ActionRun>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(first) = runs.first() else { return };
        let name = first.action_name.clone();
        let commands = runs
            .iter()
            .map(|run| run.line.display())
            .collect::<Vec<_>>()
            .join("\n");
        let body = self.tr_fmt("set.actions.confirm_body", &[("commands", &commands)]);
        let title = self.tr_fmt("set.actions.confirm_title", &[("name", &name)]);
        let ok = self.tr("menu.run");
        let cancel = self.tr("action.cancel");
        let entity = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            let runs = runs.clone();
            alert
                .title(title.clone())
                .description(body.clone())
                .confirm()
                .button_props(DialogButtonProps::default().ok_text(ok).cancel_text(cancel))
                .on_ok(move |_, window, cx| {
                    let runs = runs.clone();
                    entity
                        .update(cx, |this, cx| this.enqueue_actions(runs, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// Queue one operation per item, so multi-select actions run in order.
    fn enqueue_actions(
        &mut self,
        runs: Vec<ActionRun>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for run in runs {
            let label = self
                .tr_fmt(
                    "set.actions.op_label",
                    &[("name", &run.action_name), ("item", &run.item)],
                )
                .into();
            self.enqueue(Vec::new(), PendingKind::Action { run }, label, window, cx);
        }
    }

    pub(super) fn start_action_run(
        &mut self,
        label: SharedString,
        run: ActionRun,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No item/byte totals: the progress bar stays indeterminate.
        self.begin_op(label.to_string(), 0, 0, false, cx);
        let line = run.line.clone();
        let options = self.scan_options();
        let job = cx.background_executor().spawn(async move { line.run() });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            if run.refresh_after {
                let target = run.target.clone();
                let node = cx
                    .background_executor()
                    .spawn(async move { scanner::scan_subtree(&target, &options) })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    this.splice_refreshed(&run.target, node, window, cx);
                })
                .ok();
            }
            this.update_in(cx, |this, window, cx| {
                // Actions are not cancellable once spawned; keep the queue going.
                this._op_ticker = None;
                this.op = None;
                this.report_action_result(&run, result, window, cx);
                this.start_next_op(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Replace a folder's node with a fresh scan, keeping the cursor where it
    /// was when possible.
    fn splice_refreshed(
        &mut self,
        fs: &std::path::Path,
        mut node: Subtree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        node.root_mut().set_expanded(true);
        let path = self.tree.as_ref().and_then(|tree| tree.read().find(fs));
        let Some(path) = path else { return };
        if path.is_empty() {
            // The scan root: a fresh in-place splice is not supported, so
            // re-scan honestly.
            self.start_scan(fs.to_path_buf(), window, cx);
            return;
        }
        let select = self.tree.as_ref().and_then(|tree| {
            let mut t = tree.write();
            let parent: NodePath = path[..path.len() - 1].to_vec();
            t.remove(&path)?;
            t.insert(&parent, node)
        });
        let previous = self.selected_fs_path().map(|(_, fs, _)| fs);
        match previous {
            Some(previous) => self.tree_changed_keeping_selection(&previous, select, cx),
            None => self.tree_changed(select, cx),
        }
    }

    fn report_action_result(
        &mut self,
        run: &ActionRun,
        result: Result<RunOutput, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = run.action_name.clone();
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                window.push_notification(
                    Notification::error(error)
                        .title(self.tr_fmt("set.actions.failed_title", &[("name", &name)])),
                    cx,
                );
                return;
            }
        };
        if run.show_output {
            self.show_action_output(&name, &run.line.display(), &output, window, cx);
        }
        if !output.success {
            let code = output
                .code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into());
            let text = output.combined();
            let text = if text.is_empty() {
                self.tr("set.actions.no_output").to_string()
            } else {
                truncate_output(&text, 1_500)
            };
            let body = self.tr_fmt(
                "set.actions.failed_body",
                &[("code", &code), ("output", &text)],
            );
            window.push_notification(
                Notification::error(body)
                    .title(self.tr_fmt("set.actions.failed_title", &[("name", &name)])),
                cx,
            );
        }
    }

    fn show_action_output(
        &mut self,
        name: &str,
        command: &str,
        output: &RunOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.tr_fmt("set.actions.output_title", &[("name", name)]);
        let status = if output.success {
            self.tr_fmt("set.actions.done", &[("name", name)])
        } else {
            let code = output
                .code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into());
            self.tr_fmt("set.actions.exit_code", &[("code", &code)])
        };
        let text = output.combined();
        let text = if text.is_empty() {
            self.tr("set.actions.no_output").to_string()
        } else {
            truncate_output(&text, 40_000)
        };
        let body = format!("{command}\n\n{text}");
        let close = self.tr("action.close");
        window.open_dialog(cx, move |dialog, window, cx| {
            dialog
                .title(title.clone())
                .w(sp(760.).to_pixels(window.rem_size()))
                .child(
                    div()
                        .id("action-output")
                        .max_h(sp(420.))
                        .overflow_y_scroll()
                        .text_xs()
                        .whitespace_normal()
                        .child(SharedString::from(body.clone())),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(status.clone()),
                        )
                        .child(
                            Button::new("action-output-close")
                                .outline()
                                .label(close)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
        });
    }
}

/// Keep failure output readable; truncates on a character boundary.
fn truncate_output(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .take_while(|(i, _)| *i <= max)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("{}\n…", &text[..end])
}

// ---------------------------------------------------------------------------
// Settings → Actions
// ---------------------------------------------------------------------------

/// Draft of one action being edited inline in the Settings dialog.
pub(super) struct ActionEditor {
    /// Index in `Settings::actions`; `None` adds a new one.
    index: Option<usize>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<InputState>,
    working_dir: Entity<InputState>,
    pattern: Entity<InputState>,
    applies_to: ActionAppliesTo,
    shortcut: Option<u8>,
    confirm: bool,
    refresh_after: bool,
    show_output: bool,
    shell: bool,
    enabled: bool,
}

impl ActionEditor {
    pub(super) fn new(
        action: &ActionDef,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<SettingsView>,
    ) -> Self {
        let mut input =
            |value: String| cx.new(|cx| InputState::new(window, cx).default_value(value));
        Self {
            index,
            name: input(action.name.clone()),
            command: input(action.command.clone()),
            args: input(action.args.join(" ")),
            working_dir: input(action.working_dir.clone()),
            pattern: input(action.match_pattern.clone().unwrap_or_default()),
            applies_to: action.applies_to,
            shortcut: action.shortcut.filter(|n| (1..=9).contains(n)),
            confirm: action.confirm,
            refresh_after: action.refresh_after,
            show_output: action.show_output,
            shell: action.shell,
            enabled: action.enabled,
        }
    }

    fn to_def(&self, cx: &App) -> ActionDef {
        let pattern = self.pattern.read(cx).value().trim().to_string();
        ActionDef {
            name: self.name.read(cx).value().trim().to_string(),
            command: self.command.read(cx).value().trim().to_string(),
            args: split_args(&self.args.read(cx).value()),
            working_dir: self.working_dir.read(cx).value().trim().to_string(),
            applies_to: self.applies_to,
            match_pattern: (!pattern.is_empty()).then_some(pattern),
            confirm: self.confirm,
            refresh_after: self.refresh_after,
            show_output: self.show_output,
            shell: self.shell,
            shortcut: self.shortcut,
            enabled: self.enabled,
        }
    }
}

impl SettingsView {
    /// The Actions card: the list of configured commands plus the inline
    /// editor when one is open.
    pub(super) fn render_actions_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let card_bg = theme.group_box;
        let (actions, lang) = {
            let app = self.owner.read(cx);
            (
                app.settings.actions.clone(),
                app.settings.locale.language.clone(),
            )
        };
        let tr = |key: &'static str| -> &'static str { fastdir_locale::t(&lang, key) };
        let caption = |text: &'static str| div().text_xs().text_color(muted).child(text);

        let mut rows = v_flex().gap_2().w_full();
        if actions.is_empty() {
            rows = rows.child(caption(tr("set.actions.empty")));
        }
        for (ix, action) in actions.iter().enumerate() {
            let summary = if action.args.is_empty() {
                action.command.clone()
            } else {
                format!("{} {}", action.command, action.args.join(" "))
            };
            let switch_owner = self.owner.clone();
            let delete_owner = self.owner.clone();
            rows = rows.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .w_full()
                    .child(
                        Switch::new(("action-enabled", ix))
                            .checked(action.enabled)
                            .on_change(cx.listener(move |_, checked: &bool, _, cx| {
                                switch_owner.update(cx, |this, cx| {
                                    if let Some(action) = this.settings.actions.get_mut(ix) {
                                        action.enabled = *checked;
                                        this.settings.save();
                                        cx.notify();
                                    }
                                });
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(action.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(SharedString::from(summary)),
                            ),
                    )
                    .child(
                        Button::new(("action-edit", ix))
                            .ghost()
                            .small()
                            .icon(IconName::Pencil)
                            .tooltip(tr("set.actions.edit"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let action = this.owner.read(cx).settings.actions.get(ix).cloned();
                                if let Some(action) = action {
                                    this.action_editor =
                                        Some(ActionEditor::new(&action, Some(ix), window, cx));
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new(("action-delete", ix))
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip(tr("set.actions.delete"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this
                                    .action_editor
                                    .as_ref()
                                    .is_some_and(|draft| draft.index == Some(ix))
                                {
                                    this.action_editor = None;
                                }
                                delete_owner.update(cx, |this, cx| {
                                    if ix < this.settings.actions.len() {
                                        this.settings.actions.remove(ix);
                                        this.settings.save();
                                        cx.notify();
                                    }
                                });
                            })),
                    ),
            );
        }

        let presets = ActionDef::presets();
        let preset_owner = self.owner.clone();
        let preset_label = tr("set.actions.add_preset");
        let add = h_flex()
            .gap_2()
            .child(
                Button::new("action-add")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label(tr("set.actions.add"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.action_editor =
                            Some(ActionEditor::new(&ActionDef::default(), None, window, cx));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("action-add-preset")
                    .outline()
                    .small()
                    .label(preset_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for preset in presets.iter() {
                            let owner = preset_owner.clone();
                            let preset = preset.clone();
                            menu = menu.item(
                                gpui_kit::component::menu::PopupMenuItem::new(preset.name.clone())
                                    .icon(IconName::Plus)
                                    .on_click(move |_, _, cx| {
                                        let preset = preset.clone();
                                        owner.update(cx, |this, cx| {
                                            this.settings.actions.push(ActionDef {
                                                enabled: true,
                                                ..preset
                                            });
                                            this.settings.save();
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            );

        // The inline editor, when one is open.
        let mut editor: Option<AnyElement> = None;
        if let Some(draft) = &self.action_editor {
            let field = |label: &'static str, input: &Entity<InputState>| {
                v_flex()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xs().text_color(muted).child(label))
                    .child(Input::new(input).small())
            };
            let name_input = draft.name.clone();
            let command_input = draft.command.clone();
            let args_input = draft.args.clone();
            let dir_input = draft.working_dir.clone();
            let pattern_input = draft.pattern.clone();
            let applies_ix = match draft.applies_to {
                ActionAppliesTo::Dir => 0,
                ActionAppliesTo::File => 1,
                ActionAppliesTo::Both => 2,
            };
            let shortcut_ix = draft.shortcut.map_or(0, |n| n as usize);
            let (confirm, refresh, show_output, shell, enabled) = (
                draft.confirm,
                draft.refresh_after,
                draft.show_output,
                draft.shell,
                draft.enabled,
            );
            let save_owner = self.owner.clone();
            let lang_save = lang.clone();

            let flag_switch = |id: &'static str, checked: bool| Switch::new(id).checked(checked);

            editor = Some(
                v_flex()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(card_bg)
                    .child(
                        h_flex()
                            .gap_3()
                            .w_full()
                            .child(field(tr("set.actions.name"), &name_input))
                            .child(field(tr("set.actions.command"), &command_input)),
                    )
                    .child(field(tr("set.actions.args"), &args_input))
                    .child(caption(tr("set.actions.args_cap")))
                    .child(
                        h_flex()
                            .gap_3()
                            .w_full()
                            .child(field(tr("set.actions.working_dir"), &dir_input))
                            .child(field(tr("set.actions.match"), &pattern_input)),
                    )
                    .child(caption(tr("set.actions.working_dir_cap")))
                    .child(caption(tr("set.actions.match_cap")))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().child(tr("set.actions.applies")))
                            .child(
                                RadioGroup::horizontal("action-applies")
                                    .selected_index(Some(applies_ix))
                                    .child(Radio::new("action-dir").label(tr("set.actions.dir")))
                                    .child(Radio::new("action-file").label(tr("set.actions.file")))
                                    .child(Radio::new("action-both").label(tr("set.actions.both")))
                                    .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                        if let Some(draft) = this.action_editor.as_mut() {
                                            draft.applies_to = match ix {
                                                0 => ActionAppliesTo::Dir,
                                                1 => ActionAppliesTo::File,
                                                _ => ActionAppliesTo::Both,
                                            };
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().child(tr("set.actions.shortcut")))
                            .child(
                                RadioGroup::horizontal("action-shortcut")
                                    .selected_index(Some(shortcut_ix))
                                    .child(
                                        Radio::new("action-shortcut-none")
                                            .label(tr("set.actions.none")),
                                    )
                                    .children((1u8..=9).map(|n| {
                                        Radio::new(("action-shortcut", n as usize))
                                            .label(n.to_string())
                                    }))
                                    .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                        if let Some(draft) = this.action_editor.as_mut() {
                                            draft.shortcut = match ix {
                                                1..=9 => Some(*ix as u8),
                                                _ => None,
                                            };
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_4()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(flag_switch("action-confirm", confirm).on_change(
                                        cx.listener(|this, checked: &bool, _, cx| {
                                            if let Some(draft) = this.action_editor.as_mut() {
                                                draft.confirm = *checked;
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                    .child(caption(tr("set.actions.confirm"))),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(flag_switch("action-refresh", refresh).on_change(
                                        cx.listener(|this, checked: &bool, _, cx| {
                                            if let Some(draft) = this.action_editor.as_mut() {
                                                draft.refresh_after = *checked;
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                    .child(caption(tr("set.actions.refresh"))),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(flag_switch("action-output", show_output).on_change(
                                        cx.listener(|this, checked: &bool, _, cx| {
                                            if let Some(draft) = this.action_editor.as_mut() {
                                                draft.show_output = *checked;
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                    .child(caption(tr("set.actions.show_output"))),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(flag_switch("action-shell", shell).on_change(
                                        cx.listener(|this, checked: &bool, _, cx| {
                                            if let Some(draft) = this.action_editor.as_mut() {
                                                draft.shell = *checked;
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                    .child(caption(tr("set.actions.shell"))),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(flag_switch("action-enabled-edit", enabled).on_change(
                                        cx.listener(|this, checked: &bool, _, cx| {
                                            if let Some(draft) = this.action_editor.as_mut() {
                                                draft.enabled = *checked;
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                    .child(caption(tr("set.actions.enabled"))),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("action-editor-cancel")
                                    .outline()
                                    .small()
                                    .label(tr("action.cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.action_editor = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("action-editor-save")
                                    .primary()
                                    .small()
                                    .label(tr("set.actions.save"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let Some(draft) = this.action_editor.as_ref() else {
                                            return;
                                        };
                                        let def = draft.to_def(cx);
                                        let index = draft.index;
                                        if def.name.is_empty() || def.command.is_empty() {
                                            window.push_notification(
                                                Notification::warning(fastdir_locale::t(
                                                    &lang_save,
                                                    "set.actions.need_name",
                                                )),
                                                cx,
                                            );
                                            return;
                                        }
                                        save_owner.update(cx, |app, cx| {
                                            match index {
                                                Some(ix) if ix < app.settings.actions.len() => {
                                                    app.settings.actions[ix] = def;
                                                }
                                                _ => app.settings.actions.push(def),
                                            }
                                            app.settings.save();
                                            cx.notify();
                                        });
                                        this.action_editor = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_4()
            .w_full()
            .px_5()
            .py_4()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .child(
                div()
                    .pb_1()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("set.actions.title")),
            )
            .child(caption(tr("set.actions.caption")))
            .child(rows)
            .child(add)
            .children(editor)
            .into_any_element()
    }
}

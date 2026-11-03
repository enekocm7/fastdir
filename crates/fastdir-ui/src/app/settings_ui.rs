//! The Settings dialog: appearance, scanning, treemap, file list, file
//! operations, startup, updates and config-file tools.

use super::actions_ui::ActionEditor;
use super::*;
use fastdir_core::integration;
use fastdir_settings::{
    DateFormat, DeleteAction, PercentBase, SizeUnit, SortSettings, StartupMode, ThemePreference,
    TreemapColorMode, TreemapPalette, UpdateChannel, UpdateFrequency, config_path,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;

pub(super) struct SettingsView {
    pub(super) owner: Entity<FastDir>,
    _owner_sub: Subscription,
    excluded_input: Entity<InputState>,
    thread_input: Entity<InputState>,
    cushion_input: Entity<InputState>,
    min_cell_input: Entity<InputState>,
    scale_input: Entity<InputState>,
    /// Action being added or edited inline in the Actions card.
    pub(super) action_editor: Option<ActionEditor>,
}

impl SettingsView {
    fn new(
        owner: Entity<FastDir>,
        excluded_input: Entity<InputState>,
        thread_input: Entity<InputState>,
        cushion_input: Entity<InputState>,
        min_cell_input: Entity<InputState>,
        scale_input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&owner, |_, _, cx| cx.notify());
        Self {
            owner,
            _owner_sub: sub,
            excluded_input,
            thread_input,
            cushion_input,
            min_cell_input,
            scale_input,
            action_editor: None,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (theme, scan, treemap, list, ops, startup_mode, integration, updates, busy) = {
            let app = self.owner.read(cx);
            (
                app.settings.appearance.theme,
                app.settings.scan.clone(),
                app.settings.treemap.clone(),
                app.settings.list.clone(),
                app.settings.ops.clone(),
                app.settings.startup.mode,
                app.settings.integration.clone(),
                app.settings.updates.clone(),
                app.checking || app.updating,
            )
        };
        let menu_state = integration::context_menu_state();
        let machine_managed = matches!(menu_state, integration::ContextMenuState::Machine);
        let theme_ix = match theme {
            ThemePreference::System => 0,
            ThemePreference::Light => 1,
            ThemePreference::Dark => 2,
        };
        let palette_ix = match treemap.palette {
            TreemapPalette::Default => 0,
            TreemapPalette::ColorBlind => 1,
            TreemapPalette::Mono => 2,
        };
        let color_mode_ix = match treemap.color_mode {
            TreemapColorMode::Extension => 0,
            TreemapColorMode::Age => 1,
            TreemapColorMode::Category => 2,
            TreemapColorMode::Change => 3,
        };
        let size_unit_ix = match list.size_unit {
            SizeUnit::Binary => 0,
            SizeUnit::Decimal => 1,
        };
        let percent_base_ix = match list.percent_base {
            PercentBase::Parent => 0,
            PercentBase::Total => 1,
        };
        let date_format_ix = match list.date_format {
            DateFormat::Absolute => 0,
            DateFormat::Relative => 1,
        };
        let default_delete_ix = match ops.default_delete {
            DeleteAction::Recycle => 0,
            DeleteAction::Permanent => 1,
        };
        let startup_ix = match startup_mode {
            StartupMode::Drives => 0,
            StartupMode::Last => 1,
            StartupMode::Empty => 2,
        };
        let channel_ix = match updates.channel {
            UpdateChannel::Stable => 0,
            UpdateChannel::Prerelease => 1,
        };
        let frequency_ix = match updates.frequency {
            UpdateFrequency::Start => 0,
            UpdateFrequency::Daily => 1,
            UpdateFrequency::Weekly => 2,
        };
        let skipped = updates.skipped_version.clone();
        let last_dest = ops.last_move_dest.clone();
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        // Every label resolves to `&'static str`, so these can be moved into
        // the `'static` dialog/button closures below.
        let lang_setting: String = self.owner.read(cx).settings.locale.language.clone();
        let tr = |key: &'static str| -> &'static str { fastdir_locale::t(&lang_setting, key) };
        // Precompute `'static` strings + owned language clones for the
        // `'static` closures below (`open_alert_dialog`, `cx.listener`).
        let reset_title = tr("set.reset.title");
        let reset_body = tr("set.reset.body");
        let reset_ok = tr("set.reset.ok");
        let reset_cancel = tr("action.cancel");
        let fail_title = tr("set.integration.fail_title");
        let lang_for_menu_fail: String = lang_setting.clone();
        let lang_current = fastdir_locale::resolve(&lang_setting);
        // `system` first, then every supported language by code.
        let lang_codes: Vec<&'static str> = {
            let mut codes = vec!["system"];
            codes.extend(fastdir_locale::SUPPORTED.iter().map(|(c, _)| *c));
            codes
        };
        let lang_selected = if lang_setting == "system" {
            0
        } else {
            lang_codes
                .iter()
                .position(|c| *c == lang_current)
                .unwrap_or(0)
        };
        let lang_names: Vec<&'static str> = lang_codes
            .iter()
            .map(|c| {
                if *c == "system" {
                    tr("set.language.system")
                } else {
                    fastdir_locale::native_name(c)
                }
            })
            .collect();

        let theme_owner = self.owner.clone();
        let auto_owner = self.owner.clone();
        let check_owner = self.owner.clone();
        let clear_owner = self.owner.clone();
        let skip_hidden_owner = self.owner.clone();
        let skip_windows_owner = self.owner.clone();
        let junctions_owner = self.owner.clone();
        let cross_owner = self.owner.clone();
        let threads_owner = self.owner.clone();
        let thread_owner = self.owner.clone();
        let thread_input = self.thread_input.clone();
        let partial_owner = self.owner.clone();
        let owners_owner = self.owner.clone();
        let excluded_owner = self.owner.clone();
        let excluded_input = self.excluded_input.clone();
        let tm_show_owner = self.owner.clone();
        let tm_free_owner = self.owner.clone();
        let tm_unknown_owner = self.owner.clone();
        let tm_disk_owner = self.owner.clone();
        let palette_owner = self.owner.clone();
        let color_mode_owner = self.owner.clone();
        let cushion_owner = self.owner.clone();
        let cushion_strength_owner = self.owner.clone();
        let cushion_input = self.cushion_input.clone();
        let borders_owner = self.owner.clone();
        let min_cell_owner = self.owner.clone();
        let min_cell_input = self.min_cell_input.clone();
        let size_unit_owner = self.owner.clone();
        let percent_owner = self.owner.clone();
        let date_owner = self.owner.clone();
        let age_tint_owner = self.owner.clone();
        let col_items_owner = self.owner.clone();
        let col_files_owner = self.owner.clone();
        let col_folders_owner = self.owner.clone();
        let col_modified_owner = self.owner.clone();
        let col_disk_owner = self.owner.clone();
        let col_owner_owner = self.owner.clone();
        let confirm_recycle_owner = self.owner.clone();
        let confirm_permanent_owner = self.owner.clone();
        let confirm_move_owner = self.owner.clone();
        let keep_view_owner = self.owner.clone();
        let hide_empty_owner = self.owner.clone();
        let default_delete_owner = self.owner.clone();
        let dest_owner = self.owner.clone();
        let startup_owner = self.owner.clone();
        let menu_owner = self.owner.clone();
        let single_owner = self.owner.clone();
        let scale_owner = self.owner.clone();
        let scale_input = self.scale_input.clone();
        let channel_owner = self.owner.clone();
        let frequency_owner = self.owner.clone();
        let reset_owner = self.owner.clone();
        let lang_owner = self.owner.clone();

        let card_bg = cx.theme().group_box;
        // Each group of settings sits in its own padded, bordered card so the
        // groups read as distinct blocks while scrolling.
        let section = || {
            v_flex()
                .px_5()
                .py_4()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(card_bg)
        };
        let title = |text: &'static str| {
            div()
                .pb_1()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let caption = |text: String| div().text_xs().text_color(muted).child(text);

        let actions_section = self.render_actions_section(cx);

        v_flex()
            .id("settings-scroll")
            // Track the main window: most of its height, leaving room for the
            // dialog's title bar and footer.
            .max_h((window.viewport_size().height * 0.72).max(px(360.)))
            .w_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .gap_4()
                    .w_full()
                    // Room on the right so the scrollbar never covers the cards.
                    .pl_1()
                    .pr_4()
                    .py_1()
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.language.title")))
                            .child(caption(tr("set.language.caption").to_string()))
                            .child({
                                let radios: Vec<Radio> = lang_codes
                                    .iter()
                                    .zip(lang_names.iter())
                                    .map(|(code, name)| {
                                        // Just under half the row, so the
                                        // wrapping group lays out two columns.
                                        Radio::new(*code).label(*name).w(relative(0.47))
                                    })
                                    .collect();
                                RadioGroup::horizontal("settings-language")
                                    .selected_index(Some(lang_selected))
                                    .children(radios)
                                    .on_change(cx.listener(
                                        move |_, ix: &usize, window, cx| {
                                            if let Some(code) = lang_codes.get(*ix) {
                                                let code = *code;
                                                lang_owner.update(cx, |this, cx| {
                                                    this.set_language(code, window, cx)
                                                });
                                            }
                                        },
                                    ))
                            }),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.appearance.title")))
                            .child(div().text_xs().text_color(muted).child(tr("set.theme.label")))
                            .child(
                                RadioGroup::horizontal("settings-theme")
                                    .selected_index(Some(theme_ix))
                                    .child(Radio::new("theme-system").label(tr("set.theme.system")))
                                    .child(Radio::new("theme-light").label(tr("set.theme.light")))
                                    .child(Radio::new("theme-dark").label(tr("set.theme.dark")))
                                    .on_change(cx.listener(move |_, ix: &usize, window, cx| {
                                        let theme = match ix {
                                            1 => ThemePreference::Light,
                                            2 => ThemePreference::Dark,
                                            _ => ThemePreference::System,
                                        };
                                        theme_owner.update(cx, |this, cx| {
                                            this.set_theme_preference(theme, window, cx)
                                        });
                                    })),
                            )
                            .child(
                                div().text_xs().text_color(muted).child(
                                    tr("set.theme.caption"),
                                ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.scale.label")))
                                    .child(caption(
                                        tr("set.scale.caption")
                                            .to_string(),
                                    ))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .w_full()
                                            .child(
                                                div().flex_1().child(Input::new(&scale_input)),
                                            )
                                            .child(
                                                Button::new("settings-scale-apply")
                                                    .outline()
                                                    .small()
                                                    .label(tr("action.apply"))
                                                    .on_click(cx.listener(
                                                        move |_, _, window, cx| {
                                                            let text = scale_input
                                                                .read(cx)
                                                                .value()
                                                                .to_string();
                                                            scale_owner.update(cx, |this, cx| {
                                                                if let Ok(v) =
                                                                    text.trim().parse::<u16>()
                                                                {
                                                                    if (50..=200).contains(&v) {
                                                                        this.settings.appearance.ui_scale =
                                                                            v as f32 / 100.0;
                                                                        this.settings.save();
                                                                        this.apply_ui_scale(
                                                                            window, cx,
                                                                        );
                                                                    }
                                                                }
                                                                cx.notify();
                                                            });
                                                        },
                                                    )),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.scan.title")))
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.scan.excluded")))
                                    .child(caption(
                                        tr("set.scan.excluded_cap")
                                            .to_string(),
                                    ))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .w_full()
                                            .child(
                                                div().flex_1().child(Input::new(&excluded_input)),
                                            )
                                            .child(
                                                Button::new("settings-excluded-apply")
                                                    .outline()
                                                    .small()
                                                    .label(tr("action.apply"))
                                                    .on_click(cx.listener(
                                                        move |_, _, _window, cx| {
                                                            let text = excluded_input
                                                                .read(cx)
                                                                .value()
                                                                .to_string();
                                                            excluded_owner.update(cx, |this, cx| {
                                                                this.settings.scan.excluded = text
                                                                    .split(',')
                                                                    .map(|s| s.trim().to_string())
                                                                    .filter(|s| !s.is_empty())
                                                                    .collect();
                                                                this.settings.save();
                                                                cx.notify();
                                                            });
                                                        },
                                                    )),
                                            ),
                                    ),
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
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.scan.skip_hidden")),
                                            )
                                            .child(caption(
                                                tr("set.scan.skip_hidden_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-skip-hidden")
                                            .checked(scan.skip_hidden_system)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    skip_hidden_owner.update(cx, |this, cx| {
                                                        this.settings.scan.skip_hidden_system =
                                                            *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.scan.skip_windows")),
                                            )
                                            .child(caption(
                                                tr("set.scan.skip_windows_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-skip-windows")
                                            .checked(scan.skip_windows_dir)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    skip_windows_owner.update(cx, |this, cx| {
                                                        this.settings.scan.skip_windows_dir =
                                                            *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.scan.junctions")),
                                            )
                                            .child(caption(
                                                tr("set.scan.junctions_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-follow-junctions")
                                            .checked(scan.follow_junctions)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    junctions_owner.update(cx, |this, cx| {
                                                        this.settings.scan.follow_junctions =
                                                            *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div().text_xs().child(tr("set.scan.cross")),
                                            )
                                            .child(caption(
                                                tr("set.scan.cross_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-cross-volumes")
                                            .checked(scan.cross_volumes)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    cross_owner.update(cx, |this, cx| {
                                                        this.settings.scan.cross_volumes = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div().text_xs().child(tr("set.scan.auto_threads")),
                                            )
                                            .child(caption(
                                                tr("set.scan.auto_threads_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-auto-threads")
                                            .checked(scan.thread_count.is_none())
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    threads_owner.update(cx, |this, cx| {
                                                        if *checked {
                                                            this.settings.scan.thread_count = None;
                                                        } else {
                                                            this.settings
                                                                .scan
                                                                .thread_count
                                                                .get_or_insert(
                                                                    scanner::default_thread_count()
                                                                        .clamp(1, 64)
                                                                        as u16,
                                                                );
                                                        }
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.scan.threads")))
                                    .child(caption(
                                        tr("set.scan.threads_cap").to_string(),
                                    ))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .w_full()
                                            .child(
                                                div().flex_1().child(Input::new(&thread_input)),
                                            )
                                            .child(
                                                Button::new("settings-threads-apply")
                                                    .outline()
                                                    .small()
                                                    .label(tr("action.apply"))
                                                    .on_click(cx.listener(
                                                        move |_, _, _window, cx| {
                                                            let text = thread_input
                                                                .read(cx)
                                                                .value()
                                                                .to_string();
                                                            thread_owner.update(cx, |this, cx| {
                                                                if let Ok(n) =
                                                                    text.trim().parse::<u16>()
                                                                {
                                                                    this.settings.scan.thread_count =
                                                                        Some(n.clamp(1, 64));
                                                                    this.settings.save();
                                                                }
                                                                cx.notify();
                                                            });
                                                        },
                                                    )),
                                            ),
                                    ),
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
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.scan.partial")),
                                            )
                                            .child(caption(
                                                tr("set.scan.partial_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-partial")
                                            .checked(scan.show_partial_results)
                                            .on_change(cx.listener(
                                                move |_, _checked: &bool, _window, cx| {
                                                    partial_owner.update(cx, |this, cx| {
                                                        this.toggle_partial_results(cx)
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div().text_xs().child(tr("set.scan.owners")),
                                            )
                                            .child(caption(
                                                tr("set.scan.owners_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-collect-owners")
                                            .checked(scan.collect_owners)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    owners_owner.update(cx, |this, cx| {
                                                        this.settings.scan.collect_owners =
                                                            *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.tree.title")))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_xs().child(tr("set.tree.show")))
                                            .child(caption(
                                                tr("set.tree.show_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-tm-show")
                                            .checked(treemap.show)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    tm_show_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.show = *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(div().text_xs().child(tr("set.tree.free")))
                                            .child(caption(
                                                tr("set.tree.free_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-tm-free")
                                            .checked(treemap.show_free_space)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    tm_free_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.show_free_space =
                                                            *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(div().text_xs().child(tr("set.tree.unknown")))
                                            .child(caption(
                                                tr("set.tree.unknown_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-tm-unknown")
                                            .checked(treemap.show_unknown)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    tm_unknown_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.show_unknown =
                                                            *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
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
                                            .child(
                                                div().text_xs().child(tr("set.tree.disk")),
                                            )
                                            .child(caption(
                                                tr("set.tree.disk_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-tm-disk")
                                            .checked(treemap.disk_size)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    tm_disk_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.disk_size = *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.tree.palette")))
                                    .child(
                                        RadioGroup::horizontal("settings-palette")
                                            .selected_index(Some(palette_ix))
                                            .child(Radio::new("palette-default").label(tr("set.tree.palette_default")))
                                            .child(
                                                Radio::new("palette-color-blind")
                                                    .label(tr("set.tree.palette_colorblind")),
                                            )
                                            .child(Radio::new("palette-mono").label(tr("set.tree.palette_mono")))
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let palette = match ix {
                                                        1 => TreemapPalette::ColorBlind,
                                                        2 => TreemapPalette::Mono,
                                                        _ => TreemapPalette::Default,
                                                    };
                                                    palette_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.palette = palette;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.tree.colorby")))
                                    .child(
                                        RadioGroup::horizontal("settings-color-mode")
                                            .selected_index(Some(color_mode_ix))
                                            .child(
                                                Radio::new("colormode-extension")
                                                    .label(tr("set.tree.colorby_ext")),
                                            )
                                            .child(Radio::new("colormode-age").label(tr("set.tree.colorby_age")))
                                            .child(
                                                Radio::new("colormode-category")
                                                    .label(tr("set.tree.colorby_category")),
                                            )
                                            .child(
                                                Radio::new("colormode-change")
                                                    .label(tr("set.tree.colorby_change")),
                                            )
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let mode = match ix {
                                                        1 => TreemapColorMode::Age,
                                                        2 => TreemapColorMode::Category,
                                                        3 => TreemapColorMode::Change,
                                                        _ => TreemapColorMode::Extension,
                                                    };
                                                    color_mode_owner.update(cx, |this, cx| {
                                                        // Change colours need a baseline.
                                                        if mode == TreemapColorMode::Change
                                                            && this.compare.is_none()
                                                        {
                                                            return;
                                                        }
                                                        this.settings.treemap.color_mode = mode;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        tr("set.tree.colorby_cap").to_string(),
                                    )),
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
                                            .child(div().text_xs().child(tr("set.tree.cushion")))
                                            .child(caption(
                                                tr("set.tree.cushion_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-cushion")
                                            .checked(treemap.cushion)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    cushion_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.cushion = *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.tree.strength")))
                                    .child(caption(tr("set.tree.strength_cap").to_string()))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .w_full()
                                            .child(
                                                div().flex_1().child(Input::new(&cushion_input)),
                                            )
                                            .child(
                                                Button::new("settings-cushion-apply")
                                                    .outline()
                                                    .small()
                                                    .label(tr("action.apply"))
                                                    .on_click(cx.listener(
                                                        move |_, _, _window, cx| {
                                                            let text = cushion_input
                                                                .read(cx)
                                                                .value()
                                                                .to_string();
                                                            cushion_strength_owner.update(
                                                                cx,
                                                                |this, cx| {
                                                                    if let Ok(v) = text
                                                                        .trim()
                                                                        .parse::<f32>()
                                                                    {
                                                                        if v.is_finite() {
                                                                            this.settings.treemap.cushion_strength =
                                                                                v.clamp(0.0, 2.0);
                                                                            this.settings.save();
                                                                            this.refresh_treemap_options(
                                                                                cx,
                                                                            );
                                                                        }
                                                                    }
                                                                    cx.notify();
                                                                },
                                                            );
                                                        },
                                                    )),
                                            ),
                                    ),
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
                                            .child(div().text_xs().child(tr("set.tree.borders")))
                                            .child(caption(
                                                tr("set.tree.borders_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-borders")
                                            .checked(treemap.borders)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    borders_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.borders = *checked;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.tree.mincell")))
                                    .child(caption(
                                        tr("set.tree.mincell_cap")
                                            .to_string(),
                                    ))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .w_full()
                                            .child(
                                                div().flex_1().child(Input::new(&min_cell_input)),
                                            )
                                            .child(
                                                Button::new("settings-mincell-apply")
                                                    .outline()
                                                    .small()
                                                    .label(tr("action.apply"))
                                                    .on_click(cx.listener(
                                                        move |_, _, _window, cx| {
                                                            let text = min_cell_input
                                                                .read(cx)
                                                                .value()
                                                                .to_string();
                                                            min_cell_owner.update(cx, |this, cx| {
                                                                if let Ok(v) =
                                                                    text.trim().parse::<f32>()
                                                                {
                                                                    if v.is_finite() {
                                                                        this.settings.treemap.min_cell =
                                                                            v.clamp(0.5, 16.0);
                                                                        this.settings.save();
                                                                        this.refresh_treemap_options(
                                                                            cx,
                                                                        );
                                                                    }
                                                                }
                                                                cx.notify();
                                                            });
                                                        },
                                                    )),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.list.title")))
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.list.sizeunit")))
                                    .child(
                                        RadioGroup::horizontal("settings-size-unit")
                                            .selected_index(Some(size_unit_ix))
                                            .child(Radio::new("size-binary").label(tr("set.list.binary")))
                                            .child(Radio::new("size-decimal").label(tr("set.list.decimal")))
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let unit = match ix {
                                                        1 => SizeUnit::Decimal,
                                                        _ => SizeUnit::Binary,
                                                    };
                                                    size_unit_owner.update(cx, |this, cx| {
                                                        this.settings.list.size_unit = unit;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        tr("set.list.binary_cap")
                                            .to_string(),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.list.pctbase")))
                                    .child(
                                        RadioGroup::horizontal("settings-percent-base")
                                            .selected_index(Some(percent_base_ix))
                                            .child(
                                                Radio::new("percent-parent")
                                                    .label(tr("set.list.parent")),
                                            )
                                            .child(
                                                Radio::new("percent-total").label(tr("set.list.total")),
                                            )
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let base = match ix {
                                                        1 => PercentBase::Total,
                                                        _ => PercentBase::Parent,
                                                    };
                                                    percent_owner.update(cx, |this, cx| {
                                                        this.settings.list.percent_base = base;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        tr("set.list.pctbase_cap").to_string(),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.list.datefmt")))
                                    .child(
                                        RadioGroup::horizontal("settings-date-format")
                                            .selected_index(Some(date_format_ix))
                                            .child(Radio::new("date-absolute").label(tr("set.list.absolute")))
                                            .child(Radio::new("date-relative").label(tr("set.list.relative")))
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let format = match ix {
                                                        1 => DateFormat::Relative,
                                                        _ => DateFormat::Absolute,
                                                    };
                                                    date_owner.update(cx, |this, cx| {
                                                        this.settings.list.date_format = format;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        tr("set.list.datefmt_cap")
                                            .to_string(),
                                    )),
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
                                            .child(div().text_xs().child(tr("set.list.age_tint")))
                                            .child(caption(
                                                tr("set.list.age_tint_cap").to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-age-tint")
                                            .checked(list.age_tint)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    age_tint_owner.update(cx, |this, cx| {
                                                        this.settings.list.age_tint = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.items_col")))
                                    .child(
                                        Switch::new("settings-col-items")
                                            .checked(list.show_items)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_items_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_items = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.files_col")))
                                    .child(
                                        Switch::new("settings-col-files")
                                            .checked(list.show_files)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_files_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_files = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.folders_col")))
                                    .child(
                                        Switch::new("settings-col-folders")
                                            .checked(list.show_folders)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_folders_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_folders = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.modified_col")))
                                    .child(
                                        Switch::new("settings-col-modified")
                                            .checked(list.show_modified)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_modified_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_modified = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.disk_col")))
                                    .child(
                                        Switch::new("settings-col-disk")
                                            .checked(list.show_disk)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_disk_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_disk = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.owner_col")))
                                    .child(
                                        Switch::new("settings-col-owner")
                                            .checked(list.show_owner)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    col_owner_owner.update(cx, |this, cx| {
                                                        this.settings.list.show_owner = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.list.hide_empty")))
                                    .child(
                                        Switch::new("settings-hide-empty")
                                            .checked(list.hide_empty_folders)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    hide_empty_owner.update(cx, |this, cx| {
                                                        this.settings.list.hide_empty_folders = *checked;
                                                        this.settings.save();
                                                        this.rebuild_rows();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.ops.title")))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        div()
                                            .text_xs()
                                            .child(fastdir_locale::fmt(
                                                &lang_setting,
                                                "set.ops.confirm_recycle",
                                                &[(
                                                    "trash",
                                                    fastdir_locale::trash_name(&lang_setting),
                                                )],
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-confirm-recycle")
                                            .checked(ops.confirm_recycle)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    confirm_recycle_owner.update(cx, |this, cx| {
                                                        this.settings.ops.confirm_recycle = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        div().text_xs().child(tr("set.ops.confirm_perm")),
                                    )
                                    .child(
                                        Switch::new("settings-confirm-permanent")
                                            .checked(ops.confirm_permanent)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    confirm_permanent_owner.update(
                                                        cx,
                                                        |this, cx| {
                                                            this.settings.ops.confirm_permanent =
                                                                *checked;
                                                            this.settings.save();
                                                            cx.notify();
                                                        },
                                                    );
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child(tr("set.ops.confirm_move")))
                                    .child(
                                        Switch::new("settings-confirm-move")
                                            .checked(ops.confirm_move)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    confirm_move_owner.update(cx, |this, cx| {
                                                        this.settings.ops.confirm_move = *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_center()
                                            .w_full()
                                            .gap_4()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.ops.keep_view")),
                                            )
                                            .child(
                                                Switch::new("settings-keep-view")
                                                    .checked(ops.keep_selection_after_delete)
                                                    .on_change(cx.listener(
                                                        move |_, checked: &bool, _window, cx| {
                                                            keep_view_owner.update(cx, |this, cx| {
                                                                this.settings.ops.keep_selection_after_delete = *checked;
                                                                this.settings.save();
                                                                cx.notify();
                                                            });
                                                        },
                                                    )),
                                            ),
                                    )
                                    .child(caption(tr("set.ops.keep_view_cap").to_string())),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.ops.deletekey")))
                                    .child(
                                        RadioGroup::horizontal("settings-default-delete")
                                            .selected_index(Some(default_delete_ix))
                                            .child(Radio::new("delete-recycle").label(tr("set.ops.recycle")))
                                            .child(
                                                Radio::new("delete-permanent")
                                                    .label(tr("set.ops.perm")),
                                            )
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let action = match ix {
                                                        1 => DeleteAction::Permanent,
                                                        _ => DeleteAction::Recycle,
                                                    };
                                                    default_delete_owner.update(cx, |this, cx| {
                                                        this.settings.ops.default_delete = action;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        tr("set.ops.deletekey_cap").to_string(),
                                    )),
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
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.ops.lastdest")),
                                            )
                                            .child(caption(
                                                last_dest
                                                    .as_deref()
                                                    .unwrap_or(tr("set.ops.notset"))
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Button::new("settings-clear-dest")
                                            .ghost()
                                            .small()
                                            .label(tr("action.clear"))
                                            .disabled(last_dest.is_none())
                                            .on_click(cx.listener(
                                                move |_, _, _window, cx| {
                                                    dest_owner.update(cx, |this, cx| {
                                                        this.settings.ops.last_move_dest = None;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(actions_section)
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.startup.title")))
                            .child(div().text_xs().text_color(muted).child(tr("set.startup.showonlaunch")))
                            .child(
                                RadioGroup::horizontal("settings-startup")
                                    .selected_index(Some(startup_ix))
                                    .child(Radio::new("startup-drives").label(tr("set.startup.drives")))
                                    .child(
                                        Radio::new("startup-last").label(tr("set.startup.last")),
                                    )
                                    .child(Radio::new("startup-empty").label(tr("set.startup.empty")))
                                    .on_change(cx.listener(
                                        move |_, ix: &usize, _window, cx| {
                                            let mode = match ix {
                                                1 => StartupMode::Last,
                                                2 => StartupMode::Empty,
                                                _ => StartupMode::Drives,
                                            };
                                            startup_owner.update(cx, |this, cx| {
                                                this.settings.startup.mode = mode;
                                                this.settings.save();
                                                cx.notify();
                                            });
                                        },
                                    )),
                            )
                            .child(caption(
                                tr("set.startup.hint").to_string(),
                            )),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.integration.title")))
                            .when(cfg!(windows), |d| {
                                let lang_fail: String = lang_for_menu_fail.clone();
                                d.child(
                                    h_flex()
                                        .justify_between()
                                        .items_center()
                                        .w_full()
                                        .gap_4()
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .child(tr("set.integration.ctxmenu")),
                                                )
                                                .child({
                                                    let ctx_cap: &'static str = if machine_managed {
                                                        tr("set.integration.machine")
                                                    } else {
                                                        tr("set.integration.ctxmenu_cap")
                                                    };
                                                    caption(ctx_cap.to_string())
                                                }),
                                        )
                                        .child(
                                            Switch::new("settings-context-menu")
                                                .checked(integration.context_menu)
                                                .disabled(machine_managed)
                                                .on_change(cx.listener(
                                                    move |_, checked: &bool, window, cx| {
                                                        match integration::set_context_menu(
                                                            *checked,
                                                        ) {
                                                            Ok(()) => {
                                                                menu_owner.update(cx, |this, cx| {
                                                                    this.settings.integration.context_menu =
                                                                        *checked;
                                                                    this.settings.save();
                                                                    cx.notify();
                                                                });
                                                            }
                                                            Err(e) => {
                                                                window.push_notification(
                                                                    Notification::error(
                                                                        fastdir_locale::fmt(
                                                                            &lang_fail,
                                                                            "set.integration.ctxmenu_fail",
                                                                            &[(
                                                                                "error",
                                                                                &e.to_string(),
                                                                            )],
                                                                        ),
                                                                    )
                                                                    .title(fail_title),
                                                                    cx,
                                                                );
                                                                cx.notify();
                                                            }
                                                        }
                                                    },
                                                )),
                                        ),
                                )
                            })
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div().text_xs().child(tr("set.integration.single")),
                                            )
                                            .child(caption(
                                                tr("set.integration.single_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-single-instance")
                                            .checked(integration.single_instance)
                                            .on_change(cx.listener(
                                                move |_, checked: &bool, _window, cx| {
                                                    single_owner.update(cx, |this, cx| {
                                                        this.settings.integration.single_instance =
                                                            *checked;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.updates.title")))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .child(tr("set.updates.auto")),
                                            )
                                            .child(caption(
                                                tr("set.updates.auto_cap")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Switch::new("settings-auto-update")
                                            .checked(updates.auto_check)
                                            .on_change(cx.listener(
                                                move |_, _checked: &bool, _window, cx| {
                                                    auto_owner.update(cx, |this, cx| {
                                                        this.toggle_auto_update_check(cx)
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.updates.channel")))
                                    .child(
                                        RadioGroup::horizontal("settings-channel")
                                            .selected_index(Some(channel_ix))
                                            .child(Radio::new("channel-stable").label(tr("set.updates.stable")))
                                            .child(
                                                Radio::new("channel-prerelease")
                                                    .label(tr("set.updates.prerelease")),
                                            )
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let channel = match ix {
                                                        1 => UpdateChannel::Prerelease,
                                                        _ => UpdateChannel::Stable,
                                                    };
                                                    channel_owner.update(cx, |this, cx| {
                                                        this.settings.updates.channel = channel;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child(tr("set.updates.freq")))
                                    .child(
                                        RadioGroup::horizontal("settings-frequency")
                                            .selected_index(Some(frequency_ix))
                                            .child(
                                                Radio::new("frequency-start")
                                                    .label(tr("set.updates.everystart")),
                                            )
                                            .child(Radio::new("frequency-daily").label(tr("set.updates.daily")))
                                            .child(Radio::new("frequency-weekly").label(tr("set.updates.weekly")))
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let frequency = match ix {
                                                        1 => UpdateFrequency::Daily,
                                                        2 => UpdateFrequency::Weekly,
                                                        _ => UpdateFrequency::Start,
                                                    };
                                                    frequency_owner.update(cx, |this, cx| {
                                                        this.settings.updates.frequency = frequency;
                                                        this.settings.save();
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Button::new("settings-check")
                                            .outline()
                                            .small()
                                            .icon(IconName::RefreshCw)
                                            .label(tr("set.updates.checknow"))
                                            .disabled(busy)
                                            .on_click(cx.listener(
                                                move |_, _, window, cx| {
                                                    check_owner.update(cx, |this, cx| {
                                                        this.check_for_updates(true, window, cx)
                                                    });
                                                },
                                            )),
                                    )
                                    .when_some(skipped, |this, version| {
                                        this.child(
                                            div().text_xs().text_color(muted).child(
                                                fastdir_locale::fmt(
                                                    &lang_setting,
                                                    "set.updates.skipping",
                                                    &[("version", &version)],
                                                ),
                                            ),
                                        )
                                        .child(
                                            Button::new("settings-clear-skip")
                                                .ghost()
                                                .small()
                                                .label(tr("set.updates.clearskip"))
                                                .on_click(cx.listener(
                                                    move |_, _, window, cx| {
                                                        clear_owner.update(cx, |this, cx| {
                                                            this.settings
                                                                .updates
                                                                .skipped_version = None;
                                                            this.settings.save();
                                                            this.check_for_updates(
                                                                true, window, cx,
                                                            );
                                                            cx.notify();
                                                        });
                                                    },
                                                )),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title(tr("set.advanced.title")))
                            .when_some(config_path(), |this, path| {
                                this.child(caption(fastdir_locale::fmt(
                                    &lang_setting,
                                    "set.advanced.savedin",
                                    &[("path", &path.display().to_string())],
                                )))
                            })
                            .child(
                                h_flex().gap_2().items_center().child(
                                    Button::new("settings-open-file")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label(tr("set.advanced.openfile"))
                                        .disabled(config_path().is_none())
                                        .on_click(cx.listener(move |_, _, _window, _cx| {
                                            if let Some(path) = config_path() {
                                                fsops::reveal(&path);
                                            }
                                        })),
                                ).child(
                                    Button::new("settings-reset")
                                        .outline()
                                        .small()
                                        .label(tr("set.advanced.reset"))
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            let entity = reset_owner.downgrade();
                                            window.open_alert_dialog(
                                                cx,
                                                move |alert, _, _| {
                                                    let entity = entity.clone();
                                                    alert
                                                        .title(reset_title)
                                                        .description(
                                                            reset_body,
                                                        )
                                                        .confirm()
                                                        .button_props(
                                                            DialogButtonProps::default()
                                                                .ok_text(reset_ok)
                                                                .ok_variant(ButtonVariant::Danger)
                                                                .cancel_text(reset_cancel),
                                                        )
                                                        .on_ok(move |_, window, cx| {
                                                            entity
                                                                .update(cx, |this, cx| {
                                                                    this.reset_settings(window, cx)
                                                                })
                                                                .ok();
                                                            true
                                                        })
                                                },
                                            );
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

impl FastDir {
    /// Restore every setting to its default, keeping the window geometry and
    /// divider positions, then re-apply the live theme, scale and treemap options.
    pub(super) fn reset_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_state = self.settings.window;
        let panels = self.settings.panels.clone();
        self.settings = Settings::default();
        self.settings.window = window_state;
        self.settings.panels = panels;
        let sort = SortSettings::default();
        self.sort = (sort.column, sort.descending);
        self.home_drives = self.settings.startup.mode != StartupMode::Empty;
        self.settings.save();
        let lang = self.lang();
        self.treemap.update(cx, |t, cx| t.set_language(lang, cx));
        let placeholder = self.tr("search.placeholder");
        self.search.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        self.apply_theme(window, cx);
        self.apply_ui_scale(window, cx);
        self.refresh_treemap_options(cx);
        cx.notify();
    }

    pub(super) fn open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let excluded_text = self.settings.scan.excluded.join(", ");
        let thread_text = self
            .settings
            .scan
            .thread_count
            .map(|n| n.to_string())
            .unwrap_or_default();
        let cushion_text = self.settings.treemap.cushion_strength.to_string();
        let min_cell_text = self.settings.treemap.min_cell.to_string();
        let scale_text = format!(
            "{:.0}",
            self.settings.appearance.ui_scale.clamp(0.5, 2.0) * 100.0
        );
        let excluded_input = cx.new(|cx| InputState::new(window, cx).default_value(excluded_text));
        let thread_input = cx.new(|cx| InputState::new(window, cx).default_value(thread_text));
        let cushion_input = cx.new(|cx| InputState::new(window, cx).default_value(cushion_text));
        let min_cell_input = cx.new(|cx| InputState::new(window, cx).default_value(min_cell_text));
        let scale_input = cx.new(|cx| InputState::new(window, cx).default_value(scale_text));
        let owner = cx.entity();
        let dlg_title = self.tr("set.title");
        let dlg_close = self.tr("action.close");
        let view = cx.new(|cx| {
            SettingsView::new(
                owner,
                excluded_input,
                thread_input,
                cushion_input,
                min_cell_input,
                scale_input,
                cx,
            )
        });
        window.open_dialog(cx, move |dialog, window, _| {
            // Scale with the main window, within readable bounds.
            let width = (window.viewport_size().width * 0.7).clamp(px(680.), px(1000.));
            dialog.title(dlg_title).w(width).child(view.clone()).footer(
                h_flex().w_full().justify_end().child(
                    Button::new("settings-close")
                        .outline()
                        .label(dlg_close)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
        });
    }
}

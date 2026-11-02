//! The Settings dialog: appearance, scanning, treemap, file list, file
//! operations, startup, updates and config-file tools.

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
    owner: Entity<FastDir>,
    _owner_sub: Subscription,
    excluded_input: Entity<InputState>,
    thread_input: Entity<InputState>,
    cushion_input: Entity<InputState>,
    min_cell_input: Entity<InputState>,
    scale_input: Entity<InputState>,
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
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let theme_owner = self.owner.clone();
        let auto_owner = self.owner.clone();
        let check_owner = self.owner.clone();
        let clear_owner = self.owner.clone();
        let skip_hidden_owner = self.owner.clone();
        let junctions_owner = self.owner.clone();
        let cross_owner = self.owner.clone();
        let threads_owner = self.owner.clone();
        let thread_owner = self.owner.clone();
        let thread_input = self.thread_input.clone();
        let partial_owner = self.owner.clone();
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
        let col_items_owner = self.owner.clone();
        let col_files_owner = self.owner.clone();
        let col_folders_owner = self.owner.clone();
        let col_modified_owner = self.owner.clone();
        let col_disk_owner = self.owner.clone();
        let confirm_recycle_owner = self.owner.clone();
        let confirm_permanent_owner = self.owner.clone();
        let confirm_move_owner = self.owner.clone();
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

        v_flex()
            .id("settings-scroll")
            .max_h(px(560.))
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
                            .child(title("Appearance"))
                            .child(div().text_xs().text_color(muted).child("Theme"))
                            .child(
                                RadioGroup::horizontal("settings-theme")
                                    .selected_index(Some(theme_ix))
                                    .child(Radio::new("theme-system").label("System"))
                                    .child(Radio::new("theme-light").label("Light"))
                                    .child(Radio::new("theme-dark").label("Dark"))
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
                                    "System follows the OS light / dark setting.",
                                ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child("Interface scale"))
                                    .child(caption(
                                        "Percent of normal size (50-200). Separate from OS display scaling."
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
                                                    .label("Apply")
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
                                                                            window,
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
                            .child(title("Scanning"))
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child("Excluded names"))
                                    .child(caption(
                                        "Comma-separated names to skip (`*` and `?` wildcards)."
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
                                                    .label("Apply")
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
                                                    .child("Skip hidden and system files"),
                                            )
                                            .child(caption(
                                                "Also skips dot-files on Unix.".to_string(),
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
                                                    .child("Follow junctions and symlinks"),
                                            )
                                            .child(caption(
                                                "May double-count files; each target is entered once."
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
                                                div().text_xs().child("Cross into other volumes"),
                                            )
                                            .child(caption(
                                                "Follow mount points on other drives.".to_string(),
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
                                                div().text_xs().child("Automatic thread count"),
                                            )
                                            .child(caption(
                                                "Off lets you fix the number of scan threads."
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
                                    .child(div().text_xs().child("Scan threads"))
                                    .child(caption(
                                        "Used by the next scan (1-64).".to_string(),
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
                                                    .label("Apply")
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
                                                    .child("Show partial results while scanning"),
                                            )
                                            .child(caption(
                                                "Finished folders and a treemap preview appear before the scan ends."
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
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title("Treemap"))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_xs().child("Show treemap"))
                                            .child(caption(
                                                "Off gives a list-only layout.".to_string(),
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
                                            .child(div().text_xs().child("Show free space"))
                                            .child(caption(
                                                "Adds the <Free space> block.".to_string(),
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
                                            .child(div().text_xs().child("Show unknown"))
                                            .child(caption(
                                                "Adds the <Unknown> block.".to_string(),
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
                                                div().text_xs().child("Size cells by disk usage"),
                                            )
                                            .child(caption(
                                                "Use allocated bytes instead of file sizes."
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
                                    .child(div().text_xs().child("Palette"))
                                    .child(
                                        RadioGroup::horizontal("settings-palette")
                                            .selected_index(Some(palette_ix))
                                            .child(Radio::new("palette-default").label("Default"))
                                            .child(
                                                Radio::new("palette-color-blind")
                                                    .label("Color-blind"),
                                            )
                                            .child(Radio::new("palette-mono").label("Mono"))
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
                                    .child(div().text_xs().child("Colour by"))
                                    .child(
                                        RadioGroup::horizontal("settings-color-mode")
                                            .selected_index(Some(color_mode_ix))
                                            .child(
                                                Radio::new("colormode-extension")
                                                    .label("Extension"),
                                            )
                                            .child(Radio::new("colormode-age").label("Age"))
                                            .on_change(cx.listener(
                                                move |_, ix: &usize, _window, cx| {
                                                    let mode = match ix {
                                                        1 => TreemapColorMode::Age,
                                                        _ => TreemapColorMode::Extension,
                                                    };
                                                    color_mode_owner.update(cx, |this, cx| {
                                                        this.settings.treemap.color_mode = mode;
                                                        this.settings.save();
                                                        this.refresh_treemap_options(cx);
                                                        cx.notify();
                                                    });
                                                },
                                            )),
                                    )
                                    .child(caption(
                                        "Extension list keeps extension colours.".to_string(),
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
                                            .child(div().text_xs().child("Cushion shading"))
                                            .child(caption(
                                                "Pillow shading, or flat rectangles.".to_string(),
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
                                    .child(div().text_xs().child("Cushion strength"))
                                    .child(caption("0.0-2.0.".to_string()))
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
                                                    .label("Apply")
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
                                            .child(div().text_xs().child("Cell borders"))
                                            .child(caption(
                                                "Leave a 1px gap between cells.".to_string(),
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
                                    .child(div().text_xs().child("Smallest cell"))
                                    .child(caption(
                                        "Stop subdividing below this size, in pixels (0.5-16)."
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
                                                    .label("Apply")
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
                            .child(title("File list"))
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child("Size unit"))
                                    .child(
                                        RadioGroup::horizontal("settings-size-unit")
                                            .selected_index(Some(size_unit_ix))
                                            .child(Radio::new("size-binary").label("Binary"))
                                            .child(Radio::new("size-decimal").label("Decimal"))
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
                                        "Binary uses 1024-based sizes, Decimal 1000-based."
                                            .to_string(),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child("Percentage base"))
                                    .child(
                                        RadioGroup::horizontal("settings-percent-base")
                                            .selected_index(Some(percent_base_ix))
                                            .child(
                                                Radio::new("percent-parent")
                                                    .label("Parent folder"),
                                            )
                                            .child(
                                                Radio::new("percent-total").label("Whole scan"),
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
                                        "What the % column is relative to.".to_string(),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .w_full()
                                    .child(div().text_xs().child("Date format"))
                                    .child(
                                        RadioGroup::horizontal("settings-date-format")
                                            .selected_index(Some(date_format_ix))
                                            .child(Radio::new("date-absolute").label("Absolute"))
                                            .child(Radio::new("date-relative").label("Relative"))
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
                                        "Absolute timestamps vs. relative ones (3 days ago)."
                                            .to_string(),
                                    )),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(div().text_xs().child("Items column"))
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
                                    .child(div().text_xs().child("Files column"))
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
                                    .child(div().text_xs().child("Folders column"))
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
                                    .child(div().text_xs().child("Last change column"))
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
                                    .child(div().text_xs().child("Size on disk column"))
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
                            ),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title("File operations"))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .w_full()
                                    .gap_4()
                                    .child(
                                        div().text_xs().child(
                                            format!(
                                                "Confirm moving to the {}",
                                                platform::trash_name()
                                            ),
                                        ),
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
                                        div().text_xs().child("Confirm permanent deletion"),
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
                                    .child(div().text_xs().child("Confirm moving files"))
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
                                    .child(div().text_xs().child("Delete key does"))
                                    .child(
                                        RadioGroup::horizontal("settings-default-delete")
                                            .selected_index(Some(default_delete_ix))
                                            .child(Radio::new("delete-recycle").label("Recycle"))
                                            .child(
                                                Radio::new("delete-permanent")
                                                    .label("Delete permanently"),
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
                                        "Shift+Delete always does the other.".to_string(),
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
                                                    .child("Last move destination"),
                                            )
                                            .child(caption(
                                                last_dest
                                                    .as_deref()
                                                    .unwrap_or("Not set yet")
                                                    .to_string(),
                                            )),
                                    )
                                    .child(
                                        Button::new("settings-clear-dest")
                                            .ghost()
                                            .small()
                                            .label("Clear")
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
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title("Startup"))
                            .child(div().text_xs().text_color(muted).child("Show on launch"))
                            .child(
                                RadioGroup::horizontal("settings-startup")
                                    .selected_index(Some(startup_ix))
                                    .child(Radio::new("startup-drives").label("Drive overview"))
                                    .child(
                                        Radio::new("startup-last").label("Re-scan last path"),
                                    )
                                    .child(Radio::new("startup-empty").label("Open nothing"))
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
                                "A `fastdir <path>` argument always wins.".to_string(),
                            )),
                    )
                    .child(
                        section()
                            .gap_4()
                            .w_full()
                            .child(title("Integration"))
                            .when(cfg!(windows), |d| {
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
                                                        .child("Explorer context menu"),
                                                )
                                                .child(caption(
                                                    if machine_managed {
                                                        "Installed for all users; change it from the installer."
                                                            .to_string()
                                                    } else {
                                                        "Right-click any folder or drive \u{2192} Analyze with FastDir."
                                                            .to_string()
                                                    },
                                                )),
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
                                                                    Notification::error(format!(
                                                                        "Could not update the context menu: {e}"
                                                                    ))
                                                                    .title("Integration failed"),
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
                                                div().text_xs().child("Single instance"),
                                            )
                                            .child(caption(
                                                "Second launches send their folder to this window. Takes effect on next launch."
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
                            .child(title("Updates"))
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
                                                    .child("Check for updates automatically"),
                                            )
                                            .child(caption(
                                                "Looks for a newer release on GitHub when FastDir starts."
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
                                    .child(div().text_xs().child("Release channel"))
                                    .child(
                                        RadioGroup::horizontal("settings-channel")
                                            .selected_index(Some(channel_ix))
                                            .child(Radio::new("channel-stable").label("Stable"))
                                            .child(
                                                Radio::new("channel-prerelease")
                                                    .label("Pre-releases"),
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
                                    .child(div().text_xs().child("Check frequency"))
                                    .child(
                                        RadioGroup::horizontal("settings-frequency")
                                            .selected_index(Some(frequency_ix))
                                            .child(
                                                Radio::new("frequency-start")
                                                    .label("Every start"),
                                            )
                                            .child(Radio::new("frequency-daily").label("Daily"))
                                            .child(Radio::new("frequency-weekly").label("Weekly"))
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
                                            .label("Check now")
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
                                            div().text_xs().text_color(muted).child(format!(
                                                "Skipping v{version}"
                                            )),
                                        )
                                        .child(
                                            Button::new("settings-clear-skip")
                                                .ghost()
                                                .small()
                                                .label("Clear skipped version")
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
                            .child(title("Advanced"))
                            .when_some(config_path(), |this, path| {
                                this.child(caption(format!(
                                    "Settings are saved in {}",
                                    path.display()
                                )))
                            })
                            .child(
                                h_flex().gap_2().items_center().child(
                                    Button::new("settings-open-file")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label("Open settings file")
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
                                        .label("Reset to defaults")
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            let entity = reset_owner.downgrade();
                                            window.open_alert_dialog(
                                                cx,
                                                move |alert, _, _| {
                                                    let entity = entity.clone();
                                                    alert
                                                        .title("Reset all settings?")
                                                        .description(
                                                            "Every setting returns to its default. The window position and panel sizes are kept.",
                                                        )
                                                        .confirm()
                                                        .button_props(
                                                            DialogButtonProps::default()
                                                                .ok_text("Reset")
                                                                .ok_variant(ButtonVariant::Danger)
                                                                .cancel_text("Cancel"),
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
        self.apply_theme(window, cx);
        self.apply_ui_scale(window);
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
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Settings")
                .w(px(680.))
                .child(view.clone())
                .footer(
                    h_flex().w_full().justify_end().child(
                        Button::new("settings-close")
                            .outline()
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
        });
    }
}

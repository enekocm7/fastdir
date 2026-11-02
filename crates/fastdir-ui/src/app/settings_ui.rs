//! The Settings dialog: theme choice and the updater options.

use super::*;
use fastdir_settings::config_path;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::switch::Switch;

pub(super) struct SettingsView {
    owner: Entity<FastDir>,
    _owner_sub: Subscription,
}

impl SettingsView {
    fn new(owner: Entity<FastDir>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&owner, |_, _, cx| cx.notify());
        Self {
            owner,
            _owner_sub: sub,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (theme, auto, skipped, busy) = {
            let app = self.owner.read(cx);
            (
                app.settings.theme,
                app.settings.auto_check_updates,
                app.settings.skipped_version.clone(),
                app.checking || app.updating,
            )
        };
        let selected = match theme {
            ThemePreference::System => 0,
            ThemePreference::Light => 1,
            ThemePreference::Dark => 2,
        };
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;

        let theme_owner = self.owner.clone();
        let auto_owner = self.owner.clone();
        let check_owner = self.owner.clone();
        let clear_owner = self.owner.clone();

        v_flex()
            .gap_5()
            .w_full()
            .child(
                v_flex()
                    .gap_2()
                    .w_full()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Appearance"),
                    )
                    .child(div().text_xs().text_color(muted).child("Theme"))
                    .child(
                        RadioGroup::horizontal("settings-theme")
                            .selected_index(Some(selected))
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
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("System follows the Windows light / dark setting."),
                    ),
            )
            .child(div().h(px(1.)).w_full().bg(border))
            .child(
                v_flex()
                    .gap_3()
                    .w_full()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Updates"),
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
                                    .child(div().text_xs().child("Check for updates automatically"))
                                    .child(div().text_xs().text_color(muted).child(
                                        "Looks for a newer release on GitHub when FastDir starts.",
                                    )),
                            )
                            .child(Switch::new("settings-auto-update").checked(auto).on_change(
                                cx.listener(move |_, checked: &bool, window, cx| {
                                    auto_owner.update(cx, |this, cx| {
                                        this.settings.auto_check_updates = *checked;
                                        this.settings.save();
                                        if *checked {
                                            this.check_for_updates(false, window, cx);
                                        }
                                        cx.notify();
                                    });
                                }),
                            )),
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
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        check_owner.update(cx, |this, cx| {
                                            this.check_for_updates(true, window, cx)
                                        });
                                    })),
                            )
                            .when_some(skipped, |this, version| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("Skipping v{version}")),
                                )
                                .child(
                                    Button::new("settings-clear-skip")
                                        .ghost()
                                        .small()
                                        .label("Clear skipped version")
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            clear_owner.update(cx, |this, cx| {
                                                this.settings.skipped_version = None;
                                                this.settings.save();
                                                this.check_for_updates(true, window, cx);
                                                cx.notify();
                                            });
                                        })),
                                )
                            }),
                    ),
            )
            .when_some(config_path(), |this, path| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Settings are saved in {}", path.display())),
                )
            })
    }
}

impl FastDir {
    pub(super) fn open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity();
        let view = cx.new(|cx| SettingsView::new(owner, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Settings")
                .w(px(560.))
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

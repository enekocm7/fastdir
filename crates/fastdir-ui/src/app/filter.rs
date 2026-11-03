//! The search box and the size / extension / date filters.

use super::*;

/// The text in the filter controls. It is only parsed when the user changes
/// something, so a partially typed date never disturbs the current view.
#[derive(Clone, Debug, Default)]
pub(super) struct FilterForm {
    pub query: String,
    pub min_size: String,
    pub extension: String,
    pub modified_after: String,
    pub modified_before: String,
}

impl FilterForm {
    /// Number of attribute filters set (the search text is not counted).
    fn active_count(&self) -> usize {
        [
            &self.min_size,
            &self.extension,
            &self.modified_after,
            &self.modified_before,
        ]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .count()
    }
}

impl FastDir {
    /// The search box changed: keep the attribute filters and re-filter live.
    pub(super) fn set_filter_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.filter_form.query == query {
            return;
        }
        self.filter_form.query = query;
        // A malformed attribute filter cannot happen here (the dialog only
        // accepts valid values), so an error just leaves the old view alone.
        let _ = self.rebuild_filter(cx);
    }

    /// Parse the form and rebuild the tree with the result.
    fn rebuild_filter(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let lang = self.settings.locale.language.clone();
        let form = &self.filter_form;
        let filter = Filter::parse_in(
            &form.query,
            &form.min_size,
            &form.extension,
            &form.modified_after,
            &form.modified_before,
            &lang,
        )?;
        self.filter = filter;
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.anchor = None;
        self.rebuild_rows();
        self.sync_treemap_selection(cx);
        cx.notify();
        Ok(())
    }

    /// Apply the dialog. Returns whether it may close.
    pub(super) fn apply_filter_form(
        &mut self,
        form: FilterForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let lang = self.settings.locale.language.clone();
        let rejected = self.tr("filter.rejected");
        let filter = match Filter::parse_in(
            &form.query,
            &form.min_size,
            &form.extension,
            &form.modified_after,
            &form.modified_before,
            &lang,
        ) {
            Ok(filter) => filter,
            Err(error) => {
                window.push_notification(Notification::warning(error).title(rejected), cx);
                return false;
            }
        };
        self.filter_form = form;
        self.filter = filter;
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.anchor = None;
        self.rebuild_rows();
        self.sync_treemap_selection(cx);
        cx.notify();
        true
    }

    /// Remove the search text and every attribute filter.
    pub(super) fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_form = FilterForm::default();
        self.filter = Filter::default();
        self.set_selected_owner(None, cx);
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.anchor = None;
        self.rebuild_rows();
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    pub(super) fn focus_search(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.phase, Phase::Ready) {
            return;
        }
        self.leave_treemap(cx);
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(super) fn open_filters(
        &mut self,
        _: &OpenFilters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.phase, Phase::Ready) {
            return;
        }
        let form = self.filter_form.clone();
        let ex_size = self.tr("filter.ex_size");
        let ex_ext = self.tr("filter.ex_ext");
        let date_ph = self.tr("filter.date_ph");
        let min_size = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(ex_size)
                .default_value(form.min_size)
        });
        let extension = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(ex_ext)
                .default_value(form.extension)
        });
        let after = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(date_ph)
                .default_value(form.modified_after)
        });
        let before = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(date_ph)
                .default_value(form.modified_before)
        });
        let title = self.tr("filter.title");
        let f_min = self.tr("filter.min_size");
        let f_ext = self.tr("filter.ext");
        let f_after = self.tr("filter.after");
        let f_before = self.tr("filter.before");
        let f_hint = self.tr("filter.hint");
        let f_clear = self.tr("action.clear");
        let f_cancel = self.tr("action.cancel");
        let f_apply = self.tr("action.apply");
        let entity = cx.entity().downgrade();
        let (min_apply, ext_apply) = (min_size.clone(), extension.clone());
        let (after_apply, before_apply) = (after.clone(), before.clone());
        let apply = Rc::new(move |window: &mut Window, cx: &mut App| -> bool {
            let (min, ext) = (min_apply.read(cx).value(), ext_apply.read(cx).value());
            let (after, before) = (after_apply.read(cx).value(), before_apply.read(cx).value());
            entity
                .update(cx, |this, cx| {
                    let form = FilterForm {
                        query: this.filter_form.query.clone(),
                        min_size: min.to_string(),
                        extension: ext.to_string(),
                        modified_after: after.to_string(),
                        modified_before: before.to_string(),
                    };
                    this.apply_filter_form(form, window, cx)
                })
                .unwrap_or(true)
        });

        let entity = cx.entity().downgrade();
        let (min_clear, ext_clear) = (min_size.clone(), extension.clone());
        let (after_clear, before_clear) = (after.clone(), before.clone());
        window.open_dialog(cx, move |dialog, window, cx| {
            let ok = apply.clone();
            let theme = cx.theme();
            let field = |label: &'static str, input: &Entity<InputState>| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(Input::new(input).small())
            };
            dialog
                .title(title)
                .w(sp(480.).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_3()
                        .child(field(f_min, &min_size))
                        .child(field(f_ext, &extension))
                        .child(
                            h_flex()
                                .gap_3()
                                .child(div().flex_1().child(field(f_after, &after)))
                                .child(div().flex_1().child(field(f_before, &before))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(f_hint),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child({
                            let (min, ext) = (min_clear.clone(), ext_clear.clone());
                            let (after, before) = (after_clear.clone(), before_clear.clone());
                            let entity = entity.clone();
                            Button::new("filters-clear")
                                .outline()
                                .label(f_clear)
                                .on_click(move |_, window, cx| {
                                    for input in [&min, &ext, &after, &before] {
                                        input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                    entity
                                        .update(cx, |this, cx| this.clear_filters(window, cx))
                                        .ok();
                                    window.close_dialog(cx);
                                })
                        })
                        .child(
                            Button::new("filters-cancel")
                                .outline()
                                .label(f_cancel)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("filters-apply")
                                .primary()
                                .label(f_apply)
                                .on_click(move |_, window, cx| {
                                    if ok(window, cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    pub(super) fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let ready = matches!(self.phase, Phase::Ready);
        let active = self.filter_form.active_count();
        let label = if active > 0 {
            self.tr_fmt("filter.active", &[("n", &active.to_string())])
        } else {
            self.tr("filter.plain").to_string()
        };
        h_flex()
            .gap_1()
            .flex_shrink_0()
            .child(
                div().w(sp(240.)).child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .disabled(!ready)
                        .prefix(
                            Icon::new(IconName::Search)
                                .xsmall()
                                .text_color(theme.muted_foreground),
                        ),
                ),
            )
            .child(
                Button::new("filters")
                    .ghost()
                    .small()
                    .icon(IconName::Funnel)
                    .label(label)
                    .tooltip(self.mod_tip("filter.tooltip"))
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_filters(&OpenFilters, window, cx)
                    })),
            )
    }
}

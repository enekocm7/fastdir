//! The deletion basket: collect files and folders while browsing (even across
//! folders, tabs and filters), review them in one dialog, then recycle or
//! delete everything in a single confirmed step.
//!
//! The collection itself lives in `fastdir_core::basket`; this module adds
//! and removes the current selection, keeps the basket in step with the tree
//! and renders the review dialog.

use super::*;
use fastdir_core::basket::BasketItem;

impl FastDir {
    /// What the basket stores for the node at `path`.
    fn basket_item(tree: &Tree, path: &NodePath) -> Option<BasketItem> {
        let node = tree.get(path)?;
        if node.is_special() || path.is_empty() {
            return None;
        }
        Some(BasketItem {
            path: tree.fs_path(path)?,
            is_dir: node.is_dir(),
            size: node.size,
            items: if node.is_dir() { node.items() + 1 } else { 1 },
        })
    }

    /// Whether every selected item is already in the basket (so the
    /// shortcut and the menu entry take them out instead).
    pub(super) fn selection_in_basket(&self) -> bool {
        let targets = self.targets();
        !targets.is_empty() && targets.iter().all(|t| self.basket.contains(&t.fs))
    }

    /// Ctrl+B: add the selection to the basket, or take it out again when
    /// all of it is already there.
    pub(super) fn toggle_basket(
        &mut self,
        _: &AddToBasket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.phase, Phase::Ready) {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        if self.targets().is_empty() {
            // Nothing to stage: open the review dialog instead of announcing
            // a no-op add.
            drop(tree);
            self.show_basket(&ShowBasket, window, cx);
            return;
        }
        let removing = self.selection_in_basket();
        {
            let t = tree.read();
            for target in self.targets() {
                if removing {
                    self.basket.remove(&target.fs);
                } else if let Some(item) = Self::basket_item(&t, &target.path) {
                    self.basket.add(item);
                }
            }
        }
        let key = if removing {
            "basket.removed"
        } else {
            "basket.added"
        };
        let msg = self.tr_basket(key);
        window.push_notification(msg.clone(), cx);
        self.announce(msg, cx);
        cx.notify();
    }

    /// A basket string with `{n}` (items) and `{size}` (bytes) filled in.
    fn tr_basket(&self, key: &'static str) -> String {
        let n = self.count(self.basket.effective().len() as u64);
        let size = self.bytes(self.basket.total_size());
        self.tr_fmt(key, &[("n", &n), ("size", &size)])
    }

    /// Drop basket items that left the scan and refresh the sizes of the
    /// rest. Called whenever the tree changed or a scan finished.
    pub(super) fn sync_basket(&mut self) {
        if self.basket.is_empty() {
            return;
        }
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let t = tree.read();
        self.basket.sync(|fs| {
            let path = t.find(fs)?;
            let node = t.get(&path)?;
            if path.is_empty() || node.is_special() {
                return None;
            }
            Some((node.size, if node.is_dir() { node.items() + 1 } else { 1 }))
        });
    }

    /// The basket as file operations see it: nothing inside another basket
    /// folder, and only what the current scan still contains.
    pub(super) fn basket_targets(&self) -> Vec<Target> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let t = tree.read();
        self.basket
            .effective()
            .into_iter()
            .filter_map(|item| {
                let path = t.find(&item.path)?;
                let node = t.get(&path)?;
                Some(Target {
                    fs: item.path.clone(),
                    is_dir: node.is_dir(),
                    size: node.size,
                    items: if node.is_dir() { node.items() + 1 } else { 1 },
                    path,
                })
            })
            .collect()
    }

    /// Recycle or delete everything in the basket, after the usual
    /// confirmation (which shows the item count and total size).
    pub(super) fn delete_basket(
        &mut self,
        permanent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.basket_targets();
        self.confirm_delete_targets(targets, permanent, window, cx);
    }

    /// Toolbar button: opens the review dialog and, once something is
    /// collected, shows the item count and total size.
    pub(super) fn render_basket_button(&self, cx: &mut Context<Self>) -> Button {
        let ready = matches!(self.phase, Phase::Ready);
        let tip: SharedString = self.mod_tip("tb.basket_tip").into();
        let button = Button::new("basket")
            .small()
            .ghost()
            .icon(IconName::Inbox)
            .tooltip(tip.clone())
            .accessibility_label(tip)
            .disabled(!ready)
            .on_click(cx.listener(|this, _, w, cx| this.show_basket(&ShowBasket, w, cx)));
        if self.basket.is_empty() {
            button
        } else {
            button.label(format!(
                "{} \u{b7} {}",
                self.count(self.basket.effective().len() as u64),
                self.bytes(self.basket.total_size())
            ))
        }
    }

    /// Open the basket review dialog.
    pub(super) fn show_basket(
        &mut self,
        _: &ShowBasket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.phase, Phase::Ready) {
            return;
        }
        let entity = cx.entity().downgrade();
        let title = self.tr("basket.title");
        let close = self.tr("action.close");
        let clear = self.tr("basket.clear");
        let recycle = self.tr_fmt("dlg.title_recycle", &[("trash", self.trash_name())]);
        let delete = self.tr("dlg.ok_delete");
        window.open_dialog(cx, move |dialog, window, cx| {
            // Rebuilt on every render, so removing an item updates the list.
            let Some(view) = entity.upgrade() else {
                return dialog;
            };
            let this = view.read(cx);
            let theme = cx.theme();
            let (muted, hover, border) = (theme.muted_foreground, theme.list_hover, theme.border);
            let empty = this.basket.is_empty();
            let summary: SharedString = this.tr_basket("basket.summary").into();
            let covered_note = this.tr("basket.covered");
            let remove_label = this.tr("basket.remove");
            let rows: Vec<AnyElement> = this
                .basket
                .items()
                .iter()
                .enumerate()
                .map(|(ix, item)| {
                    let covered = this.basket.is_covered(&item.path);
                    let fs = item.path.clone();
                    let remove_entity = entity.clone();
                    h_flex()
                        .id(("basket-row", ix))
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .items_center()
                        .rounded_sm()
                        .hover(|s| s.bg(hover))
                        .child(
                            Icon::new(if item.is_dir {
                                IconName::Folder
                            } else {
                                IconName::File
                            })
                            .small()
                            .text_color(muted),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_xs()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(SharedString::from(
                                            this.display_fs_path(&item.path),
                                        )),
                                )
                                .when(covered, |d| {
                                    d.child(div().text_xs().text_color(muted).child(covered_note))
                                }),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(this.bytes(item.size))),
                        )
                        .child(
                            Button::new(("basket-remove", ix))
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .tooltip(remove_label)
                                .accessibility_label(remove_label)
                                .on_click(move |_, window, cx| {
                                    let fs = fs.clone();
                                    remove_entity
                                        .update(cx, |this, cx| {
                                            this.basket.remove(&fs);
                                            cx.notify();
                                        })
                                        .ok();
                                    window.refresh();
                                }),
                        )
                        .into_any_element()
                })
                .collect();
            let empty_text = this.mod_tip("basket.empty");
            let recycle = recycle.clone();
            let (clear_entity, recycle_entity, delete_entity) =
                (entity.clone(), entity.clone(), entity.clone());
            dialog
                .title(title)
                .w(sp(680.).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(if empty {
                            div()
                                .py_4()
                                .text_xs()
                                .text_color(muted)
                                .child(empty_text)
                                .into_any_element()
                        } else {
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(summary)
                                .into_any_element()
                        })
                        .when(!empty, |d| {
                            d.child(
                                v_flex()
                                    .id("basket-list")
                                    .max_h(sp(360.))
                                    .overflow_y_scroll()
                                    .border_1()
                                    .border_color(border)
                                    .rounded_md()
                                    .p_1()
                                    .children(rows),
                            )
                        }),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_between()
                        .child(
                            Button::new("basket-clear")
                                .outline()
                                .icon(IconName::Close)
                                .label(clear)
                                .disabled(empty)
                                .on_click(move |_, window, cx| {
                                    clear_entity
                                        .update(cx, |this, cx| {
                                            this.basket.clear();
                                            cx.notify();
                                        })
                                        .ok();
                                    window.refresh();
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("basket-close")
                                        .outline()
                                        .label(close)
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("basket-recycle")
                                        .primary()
                                        .icon(IconName::Recycle)
                                        .label(recycle)
                                        .disabled(empty)
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            recycle_entity
                                                .update(cx, |this, cx| {
                                                    this.delete_basket(false, window, cx)
                                                })
                                                .ok();
                                        }),
                                )
                                .child(
                                    Button::new("basket-delete")
                                        .danger()
                                        .icon(IconName::Trash)
                                        .label(delete)
                                        .disabled(empty)
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            delete_entity
                                                .update(cx, |this, cx| {
                                                    this.delete_basket(true, window, cx)
                                                })
                                                .ok();
                                        }),
                                ),
                        ),
                )
        });
    }
}

//! The directory list and extension list: sorting, rows and rendering.

use super::*;

impl FastDir {
    pub(super) fn child_order(node: &Node, sort: (SortKey, bool)) -> Vec<u32> {
        let mut idx: Vec<u32> = (0..node.children.len() as u32).collect();
        let (key, desc) = sort;
        if key == SortKey::Size && desc {
            return idx; // children are stored in this order
        }
        let c = &node.children;
        idx.sort_by(|&a, &b| {
            let (a, b) = (&c[a as usize], &c[b as usize]);
            let ord = match key {
                SortKey::Name => b
                    .is_dir()
                    .cmp(&a.is_dir())
                    .reverse()
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Disk => a.disk_size.cmp(&b.disk_size),
                SortKey::Items => a.items().cmp(&b.items()),
                SortKey::Files => a.files.cmp(&b.files),
                SortKey::Dirs => a.dirs.cmp(&b.dirs),
                SortKey::Modified => a.modified.cmp(&b.modified),
            };
            if desc { ord.reverse() } else { ord }
        });
        if key == SortKey::Name && desc {
            // Keep folders first even when names are descending.
            idx.sort_by_key(|&i| !c[i as usize].is_dir());
        }
        idx
    }

    pub(super) fn rebuild_rows(&mut self) {
        let selected_path = self.selected_path();
        self.rows.clear();
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let tree = tree.read();
        fn walk(
            node: &Node,
            path: &mut NodePath,
            depth: u16,
            rows: &mut Vec<Row>,
            sort: (SortKey, bool),
        ) {
            rows.push(Row {
                path: path.clone(),
                depth,
            });
            if node.expanded && node.is_dir() {
                for i in FastDir::child_order(node, sort) {
                    path.push(i);
                    walk(&node.children[i as usize], path, depth + 1, rows, sort);
                    path.pop();
                }
            }
        }
        let mut path = Vec::new();
        walk(&tree.root, &mut path, 0, &mut self.rows, self.sort);
        self.ext_rows = (0..tree.exts.len() as u16)
            .filter(|&i| tree.exts[i as usize].files > 0)
            .collect();
        if let Some(sel) = selected_path {
            self.selected = self.rows.iter().position(|r| r.path == sel).or(Some(0));
        }
    }

    pub(super) fn set_expanded(
        &mut self,
        ix: usize,
        expanded: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let Some(row) = self.rows.get(ix) else { return };
        {
            let mut t = tree.write();
            let Some(n) = t.get_mut(&row.path) else {
                return;
            };
            if !n.is_dir() || n.children.is_empty() {
                return;
            }
            n.expanded = expanded.unwrap_or(!n.expanded);
        }
        self.selected = Some(ix);
        self.rebuild_rows();
        cx.notify();
    }

    pub(super) fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort.0 == key {
            self.sort.1 = !self.sort.1;
        } else {
            self.sort = (key, key != SortKey::Name);
        }
        self.rebuild_rows();
        if let Some(ix) = self.selected {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub(super) fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(tree) = self.tree.clone() else {
            return Vec::new();
        };
        let tree = tree.read();
        let theme = cx.theme();
        let total = tree.root.size.max(1);
        let selected = self.selected;
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let even = theme.list_even;
        let track = theme.muted;
        let danger = theme.danger;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = self.rows.get(ix) else {
                continue;
            };
            let Some(node) = tree.get(&row.path) else {
                continue;
            };
            let parent_size = if row.path.is_empty() {
                node.size
            } else {
                tree.get(&row.path[..row.path.len() - 1])
                    .map(|p| p.size)
                    .unwrap_or(node.size)
            };
            let frac = if parent_size == 0 {
                0.0
            } else {
                node.size as f32 / parent_size as f32
            };
            let is_sel = if self.marked.is_empty() {
                selected == Some(ix)
            } else {
                self.marked.contains(&row.path)
            };
            let is_dir = node.is_dir();
            let has_children = is_dir && !node.children.is_empty();
            let depth = row.depth as f32;
            let bar_color = rgb(DEPTH_COLORS[row.depth as usize % DEPTH_COLORS.len()]);
            let icon = if node.is_special() {
                let (r, g, b) = treemap::node_color(node);
                Icon::new(IconName::HardDrive)
                    .xsmall()
                    .text_color(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            } else if !is_dir {
                let (r, g, b) = treemap::ext_color(node.ext);
                Icon::new(IconName::File)
                    .xsmall()
                    .text_color(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            } else if row.path.is_empty() {
                Icon::new(IconName::HardDrive).xsmall().text_color(muted)
            } else if node.expanded {
                Icon::new(IconName::FolderOpen)
                    .xsmall()
                    .text_color(rgb(0xe0b04a))
            } else {
                Icon::new(IconName::Folder)
                    .xsmall()
                    .text_color(rgb(0xe0b04a))
            };
            let chevron = div()
                .id(("chev", ix))
                .size(px(16.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .when(has_children, |d| {
                    d.hover(|s| s.bg(track))
                        .child(
                            Icon::new(if node.expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall()
                            .text_color(muted),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.focus.focus(window, cx);
                                this.set_expanded(ix, None, cx);
                            }),
                        )
                });
            let num = |w: f32, s: String| {
                div()
                    .w(px(w))
                    .flex_shrink_0()
                    .px_2()
                    .text_right()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(s)
            };
            let name_color = if node.error { danger } else { fg };
            let el = h_flex()
                .id(("row", ix))
                .h(px(ROW_H))
                .w_full()
                .text_xs()
                .text_color(fg)
                .when(ix % 2 == 1 && !is_sel, |d| d.bg(even))
                .when(is_sel, |d| {
                    d.bg(active).border_l_2().border_color(active_border)
                })
                .when(!is_sel, |d| {
                    d.border_l_2()
                        .border_color(transparent_black())
                        .hover(|s| s.bg(hover))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        this.focus.focus(window, cx);
                        let mode = ClickMode::from_modifiers(&ev.modifiers);
                        this.click_row(ix, mode, cx);
                        if ev.click_count == 2 && mode == ClickMode::Plain {
                            this.set_expanded(ix, None, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| {
                        this.focus.focus(window, cx);
                        this.click_row(ix, ClickMode::Context, cx);
                    }),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w(px(220.))
                        .overflow_hidden()
                        .gap_1()
                        .pl(px(4. + depth * 16.))
                        .child(chevron)
                        .child(icon)
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(name_color)
                                .when(is_dir, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(SharedString::from(node.name.to_string())),
                        )
                        .when(node.hard_link, |d| {
                            d.child(div().flex_shrink_0().text_color(muted).child("hard link"))
                        }),
                )
                .child(
                    div().w(px(COL_BAR)).flex_shrink_0().px_2().child(
                        div()
                            .h(px(10.))
                            .w_full()
                            .rounded_sm()
                            .bg(track)
                            .overflow_hidden()
                            .child(div().h_full().w(relative(frac)).bg(bar_color)),
                    ),
                )
                .child(num(COL_PCT, format::percent(node.size, parent_size)))
                .child(num(COL_SIZE, format::bytes(node.size)).font_weight(FontWeight::MEDIUM))
                .child(num(COL_DISK, format::bytes(node.disk_size)))
                .child(
                    num(
                        COL_ITEMS,
                        if is_dir {
                            format::count(node.items())
                        } else {
                            String::new()
                        },
                    )
                    .text_color(muted),
                )
                .child(
                    num(
                        COL_FILES,
                        if is_dir {
                            format::count(node.files as u64)
                        } else {
                            String::new()
                        },
                    )
                    .text_color(muted),
                )
                .child(
                    num(
                        COL_DIRS,
                        if is_dir {
                            format::count(node.dirs as u64)
                        } else {
                            String::new()
                        },
                    )
                    .text_color(muted),
                )
                .child(num(COL_DATE, format::date(node.modified)).text_color(muted));
            let _ = total;
            out.push(el.into_any_element());
        }
        out
    }

    pub(super) fn click_row(&mut self, ix: usize, mode: ClickMode, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        let path = row.path.clone();
        self.apply_click(path, ix, mode, cx);
        if self.selected == Some(ix) {
            self.highlight_ext_of_row(ix, cx);
        }
    }

    /// Highlight one extension in the treemap (`None` clears it).
    pub(super) fn set_selected_ext(&mut self, ext: Option<u16>, cx: &mut Context<Self>) {
        if self.selected_ext == ext {
            return;
        }
        self.selected_ext = ext;
        self.treemap.update(cx, |t, cx| t.set_highlight(ext, cx));
        cx.notify();
    }

    /// Clicking an extension row toggles its treemap highlight.
    pub(super) fn toggle_ext(&mut self, ext: u16, cx: &mut Context<Self>) {
        let ext = if self.selected_ext == Some(ext) {
            None
        } else {
            Some(ext)
        };
        self.set_selected_ext(ext, cx);
    }

    /// A selected file row also highlights that file's extension.
    pub(super) fn highlight_ext_of_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let ext = self.tree.as_ref().and_then(|tree| {
            let tree = tree.read();
            let row = self.rows.get(ix)?;
            let node = tree.get(&row.path)?;
            (!node.is_dir() && node.ext != NO_EXT).then_some(node.ext)
        });
        if let Some(ext) = ext {
            self.set_selected_ext(Some(ext), cx);
        }
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (key, desc) = self.sort;
        let head = |id: &'static str,
                    label: &'static str,
                    k: SortKey,
                    w: Option<f32>,
                    right: bool,
                    cx: &mut Context<Self>| {
            let active = key == k;
            let arrow = if desc {
                IconName::ArrowDown
            } else {
                IconName::ArrowUp
            };
            h_flex()
                .id(id)
                .h_full()
                .px_2()
                .gap_1()
                .cursor_pointer()
                .when_some(w, |d, w| d.w(px(w)).flex_shrink_0())
                .when(w.is_none(), |d| d.flex_1().min_w(px(220.)).pl(px(24.)))
                .when(right, |d| d.justify_end())
                .hover(|s| s.bg(cx.theme().list_hover))
                .when(active, |d| d.text_color(cx.theme().foreground))
                .child(label)
                .when(active, |d| d.child(Icon::new(arrow).xsmall()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sort(k, cx)))
        };
        h_flex()
            .h(px(28.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.list_head)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(head("h-name", "Name", SortKey::Name, None, false, cx))
            .child(
                div()
                    .w(px(COL_BAR))
                    .flex_shrink_0()
                    .px_2()
                    .child("Subtree %"),
            )
            .child(head("h-pct", "%", SortKey::Size, Some(COL_PCT), true, cx))
            .child(head(
                "h-size",
                "Size",
                SortKey::Size,
                Some(COL_SIZE),
                true,
                cx,
            ))
            .child(head(
                "h-disk",
                "Size on disk",
                SortKey::Disk,
                Some(COL_DISK),
                true,
                cx,
            ))
            .child(head(
                "h-items",
                "Items",
                SortKey::Items,
                Some(COL_ITEMS),
                true,
                cx,
            ))
            .child(head(
                "h-files",
                "Files",
                SortKey::Files,
                Some(COL_FILES),
                true,
                cx,
            ))
            .child(head(
                "h-dirs",
                "Folders",
                SortKey::Dirs,
                Some(COL_DIRS),
                true,
                cx,
            ))
            .child(head(
                "h-date",
                "Last Change",
                SortKey::Modified,
                Some(COL_DATE),
                true,
                cx,
            ))
    }

    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        let entity = cx.entity();
        v_flex().size_full().child(self.render_header(cx)).child(
            div()
                .id("list-body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list("rows", count, cx.processor(Self::render_rows))
                        .size_full()
                        .track_scroll(&self.list_scroll),
                )
                .child(Scrollbar::vertical(&self.list_scroll))
                .context_menu(move |menu, _, cx| entity.read(cx).build_context_menu(menu, cx)),
        )
    }

    pub(super) fn render_ext_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(tree) = self.tree.clone() else {
            return Vec::new();
        };
        let tree = tree.read();
        let theme = cx.theme();
        let total = tree.root.size.max(1);
        let muted = theme.muted_foreground;
        let even = theme.list_even;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let selected = self.selected_ext;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(&ext) = self.ext_rows.get(ix) else {
                continue;
            };
            let Some(e) = tree.exts.get(ext as usize) else {
                continue;
            };
            let (r, g, b) = treemap::ext_color(ext);
            let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
            let is_sel = selected == Some(ext);
            out.push(
                h_flex()
                    .id(("ext", ix))
                    .h(px(ROW_H))
                    .w_full()
                    .px_2()
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .when(ix % 2 == 1 && !is_sel, |d| d.bg(even))
                    .when(is_sel, |d| {
                        d.bg(active).border_l_2().border_color(active_border)
                    })
                    .when(!is_sel, |d| {
                        d.border_l_2()
                            .border_color(transparent_black())
                            .hover(|s| s.bg(hover))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.focus.focus(window, cx);
                            this.toggle_ext(ext, cx);
                        }),
                    )
                    .child(div().size(px(10.)).rounded_sm().flex_shrink_0().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(50.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(SharedString::from(e.name.to_string())),
                    )
                    .child(
                        div()
                            .w(px(70.))
                            .text_right()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format::bytes(e.bytes)),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .text_right()
                            .text_color(muted)
                            .child(format::percent(e.bytes, total)),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .text_right()
                            .text_color(muted)
                            .child(format::count(e.files)),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    pub(super) fn render_ext_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.ext_rows.len();
        v_flex()
            .size_full()
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(10.)))
                    .child(div().flex_1().min_w(px(50.)).child("Extension"))
                    .child(div().w(px(70.)).text_right().child("Bytes"))
                    .child(div().w(px(48.)).text_right().child("%"))
                    .child(div().w(px(64.)).text_right().child("Files")),
            )
            .child(
                div()
                    .id("ext-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("exts", count, cx.processor(Self::render_ext_rows))
                            .size_full()
                            .track_scroll(&self.ext_scroll),
                    )
                    .child(Scrollbar::vertical(&self.ext_scroll)),
            )
    }
}

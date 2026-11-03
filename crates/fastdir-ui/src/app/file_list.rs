//! The directory list and extension list: sorting, rows and rendering.

use super::*;

/// Sort the flat top-files list in place. Name comparisons stay
/// case-insensitive, like the tree.
fn sort_top_files(files: &mut [FileEntry], sort: (SortKey, bool)) {
    let (key, desc) = sort;
    files.sort_unstable_by(|a, b| {
        let ord = match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Disk => a.disk_size.cmp(&b.disk_size),
            SortKey::Modified => a.modified.cmp(&b.modified),
            _ => a.size.cmp(&b.size),
        }
        .then_with(|| a.name.cmp(&b.name));
        if desc { ord.reverse() } else { ord }
    });
}

impl FastDir {
    pub(super) fn child_order(tree: &Tree, node: &Node, sort: (SortKey, bool)) -> Vec<u32> {
        let c = tree.children(node);
        let mut idx: Vec<u32> = (0..c.len() as u32).collect();
        let (key, desc) = sort;
        if key == SortKey::Size && desc {
            return idx; // children are stored in this order
        }
        idx.sort_by(|&a, &b| {
            let (a, b) = (&c[a as usize], &c[b as usize]);
            let ord = match key {
                SortKey::Name => b.is_dir().cmp(&a.is_dir()).reverse().then_with(|| {
                    tree.name(a)
                        .to_lowercase()
                        .cmp(&tree.name(b).to_lowercase())
                }),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Disk => a.disk_size.cmp(&b.disk_size),
                SortKey::Items => a.items().cmp(&b.items()),
                SortKey::Files => a.files.cmp(&b.files),
                SortKey::Dirs => a.dirs.cmp(&b.dirs),
                SortKey::Modified => a.modified().cmp(&b.modified()),
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
        let selected_path = self.selected_tree_path();
        self.rows.clear();
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let tree = tree.read();
        /// Add a node and its expanded descendants.
        fn walk(
            tree: &Tree,
            node: &Node,
            path: &mut NodePath,
            depth: u16,
            rows: &mut Vec<Row>,
            sort: (SortKey, bool),
        ) {
            rows.push(Row {
                path: path.clone(),
                depth,
                matched: false,
            });
            if node.expanded() && node.is_dir() {
                let children = tree.children(node);
                for i in FastDir::child_order(tree, node, sort) {
                    path.push(i);
                    walk(tree, &children[i as usize], path, depth + 1, rows, sort);
                    path.pop();
                }
            }
        }
        /// Keep every match plus the ancestors needed to reach it. Returns
        /// whether this subtree contributed a row.
        fn walk_filtered(
            tree: &Tree,
            node: &Node,
            path: &mut NodePath,
            depth: u16,
            rows: &mut Vec<Row>,
            sort: (SortKey, bool),
            filter: &Filter,
        ) -> bool {
            let start = rows.len();
            let mut any = false;
            if node.is_dir() {
                let children = tree.children(node);
                for i in FastDir::child_order(tree, node, sort) {
                    path.push(i);
                    any |= walk_filtered(
                        tree,
                        &children[i as usize],
                        path,
                        depth + 1,
                        rows,
                        sort,
                        filter,
                    );
                    path.pop();
                }
            }
            let matched = filter.matches(tree.name(node), node, tree.ext_name(node));
            if matched || any {
                rows.insert(
                    start,
                    Row {
                        path: path.clone(),
                        depth,
                        matched,
                    },
                );
                true
            } else {
                false
            }
        }
        let mut path = Vec::new();
        if self.filter.is_active() {
            walk_filtered(
                &tree,
                tree.root(),
                &mut path,
                0,
                &mut self.rows,
                self.sort,
                &self.filter,
            );
            // Start on the first actual match, not on an ancestor shown for
            // context.
            let previous = selected_path
                .and_then(|sel| self.rows.iter().position(|r| r.path == sel && r.matched));
            self.selected = previous.or_else(|| self.rows.iter().position(|r| r.matched));
        } else {
            walk(&tree, tree.root(), &mut path, 0, &mut self.rows, self.sort);
            self.selected = selected_path
                .and_then(|sel| self.rows.iter().position(|r| r.path == sel))
                .or_else(|| (!self.rows.is_empty()).then_some(0));
        }
        self.ext_rows = (0..tree.exts.len() as u16)
            .filter(|&i| tree.exts[i as usize].files > 0)
            .collect();
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
            if !n.is_dir() || !n.has_children() {
                return;
            }
            n.set_expanded(expanded.unwrap_or(!n.expanded()));
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
        self.settings.list.sort.column = self.sort.0;
        self.settings.list.sort.descending = self.sort.1;
        // Legacy top-level mirror for files that predate `[list]`.
        self.settings.sort = self.settings.list.sort;
        self.settings.save();
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
        let selected = self.selected;
        let filtering = self.filter.is_active();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let primary = theme.primary;
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
            let show = &self.settings.list;
            let (show_items, show_files, show_folders, show_modified, show_disk) = (
                show.show_items,
                show.show_files,
                show.show_folders,
                show.show_modified,
                show.show_disk,
            );
            let size_s = self.bytes(node.size);
            let disk_s = self.bytes(node.disk_size);
            let pct_s = self.percent_of(&tree, &row.path, node);
            let date_s = self.date(node.modified());
            // The subtree bar is always parent-relative (it shows the share
            // of the folder above, like WinDirStat).
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
            let has_children = is_dir && node.has_children();
            // While filtering, ancestors of a match are shown expanded even
            // if the user had collapsed them before.
            let expanded = node.expanded()
                || (filtering
                    && self
                        .rows
                        .get(ix + 1)
                        .is_some_and(|next| next.depth > row.depth));
            let depth = row.depth as f32;
            let bar_color = rgb(DEPTH_COLORS[row.depth as usize % DEPTH_COLORS.len()]);
            let palette = palette_of(self.settings.treemap.palette);
            let icon = if node.is_special() {
                let (r, g, b) = treemap::node_color_with(palette, node);
                Icon::new(IconName::HardDrive)
                    .xsmall()
                    .text_color(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            } else if !is_dir {
                let (r, g, b) = treemap::ext_color_with(palette, node.ext);
                Icon::new(IconName::File)
                    .xsmall()
                    .text_color(rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32))
            } else if row.path.is_empty() {
                Icon::new(IconName::HardDrive).xsmall().text_color(muted)
            } else if expanded {
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
                            Icon::new(if expanded {
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
            let name_color = if node.error() {
                danger
            } else if filtering && row.matched {
                primary
            } else {
                fg
            };
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
                                .when(is_dir || row.matched, |d| d.font_weight(FontWeight::MEDIUM))
                                .when(filtering && row.matched, |d| {
                                    d.font_weight(FontWeight::SEMIBOLD)
                                })
                                .child(SharedString::from(self.display_name(tree.name(node)).to_string())),
                        )
                        .when(node.hard_link(), |d| {
                            d.child(div().flex_shrink_0().text_color(muted).child(self.tr("list.hard_link")))
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
                .child(num(COL_PCT, pct_s))
                .child(num(COL_SIZE, size_s).font_weight(FontWeight::MEDIUM))
                .when(show_disk, |d| d.child(num(COL_DISK, disk_s)))
                .when(show_items, |d| {
                    d.child(
                        num(
                            COL_ITEMS,
                            if is_dir {
                                self.count(node.items())
                            } else {
                                String::new()
                            },
                        )
                        .text_color(muted),
                    )
                })
                .when(show_files, |d| {
                    d.child(
                        num(
                            COL_FILES,
                            if is_dir {
                                self.count(node.files as u64)
                            } else {
                                String::new()
                            },
                        )
                        .text_color(muted),
                    )
                })
                .when(show_folders, |d| {
                    d.child(
                        num(
                            COL_DIRS,
                            if is_dir {
                                self.count(node.dirs as u64)
                            } else {
                                String::new()
                            },
                        )
                        .text_color(muted),
                    )
                })
                .when(show_modified, |d| {
                    d.child(num(COL_DATE, date_s).text_color(muted))
                });
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
        let show = &self.settings.list;
        let (show_items, show_files, show_folders, show_modified, show_disk) = (
            show.show_items,
            show.show_files,
            show.show_folders,
            show.show_modified,
            show.show_disk,
        );
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
            .child(head("h-name", self.tr("list.col_name"), SortKey::Name, None, false, cx))
            .child(
                div()
                    .w(px(COL_BAR))
                    .flex_shrink_0()
                    .px_2()
                    .child(self.tr("list.col_subtree")),
            )
            .child(head("h-pct", self.tr("list.col_pct"), SortKey::Size, Some(COL_PCT), true, cx))
            .child(head(
                "h-size",
                self.tr("list.col_size"),
                SortKey::Size,
                Some(COL_SIZE),
                true,
                cx,
            ))
            .when(show_disk, |d| {
                d.child(head(
                    "h-disk",
                    self.tr("list.col_disk"),
                    SortKey::Disk,
                    Some(COL_DISK),
                    true,
                    cx,
                ))
            })
            .when(show_items, |d| {
                d.child(head(
                    "h-items",
                    self.tr("list.col_items"),
                    SortKey::Items,
                    Some(COL_ITEMS),
                    true,
                    cx,
                ))
            })
            .when(show_files, |d| {
                d.child(head(
                    "h-files",
                    self.tr("list.col_files"),
                    SortKey::Files,
                    Some(COL_FILES),
                    true,
                    cx,
                ))
            })
            .when(show_folders, |d| {
                d.child(head(
                    "h-dirs",
                    self.tr("list.col_dirs"),
                    SortKey::Dirs,
                    Some(COL_DIRS),
                    true,
                    cx,
                ))
            })
            .when(show_modified, |d| {
                d.child(head(
                    "h-date",
                    self.tr("list.col_date"),
                    SortKey::Modified,
                    Some(COL_DATE),
                    true,
                    cx,
                ))
            })
    }

    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(self.render_tabs(cx))
            .child(match self.tab {
                ListTab::Tree => self.render_tree_table(cx).into_any_element(),
                ListTab::TopFiles => self.render_top_table(cx).into_any_element(),
            })
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let primary = theme.primary;
        let border = theme.border;
        let head = theme.list_head;
        let tree_tab = self.tab == ListTab::Tree;
        let top_tab = self.tab == ListTab::TopFiles;
        let tab = |id: &'static str,
                   label: String,
                   active: bool,
                   which: ListTab,
                   cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .h_full()
                .px_3()
                .cursor_pointer()
                .text_xs()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if active { fg } else { muted })
                .when(active, |d| d.border_b_2().border_color(primary))
                .when(!active, |d| {
                    d.border_b_2()
                        .border_color(transparent_black())
                        .hover(|s| s.text_color(fg))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(which, cx)))
                .child(label)
        };
        h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .w_full()
            .gap_1()
            .px_1()
            .border_b_1()
            .border_color(border)
            .bg(head)
            .child(tab(
                "tab-tree",
                self.tr("list.tree_tab").to_string(),
                tree_tab,
                ListTab::Tree,
                cx,
            ))
            .child(tab(
                "tab-top-files",
                self.tr_fmt("list.top_tab", &[("n", &self.count(self.top_files.len() as u64))]),
                top_tab,
                ListTab::TopFiles,
                cx,
            ))
    }

    fn render_tree_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        let entity = cx.entity();
        let theme = cx.theme();
        let body: AnyElement = if count == 0 {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(if self.filter.is_active() {
                    self.tr("list.no_match")
                } else {
                    self.tr("list.empty")
                })
                .into_any_element()
        } else {
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
                .context_menu(move |menu, _, cx| entity.read(cx).build_context_menu(menu, cx))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .child(body)
    }

    /// Switch between the tree and the flat largest-files list.
    pub(super) fn set_tab(&mut self, tab: ListTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.marked.clear();
        if tab == ListTab::TopFiles {
            if self.top_stale {
                self.refresh_top_files();
            }
            if self.top_selected.is_none() && !self.top_files.is_empty() {
                self.top_selected = Some(0);
            }
        }
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    /// Recompute the bounded "largest files" list from the current tree.
    pub(super) fn refresh_top_files(&mut self) {
        let limit = self.settings.list.top_files_limit.clamp(1, 100_000);
        let mut files = self
            .tree
            .as_ref()
            .map(|tree| tree.read().top_files(limit))
            .unwrap_or_default();
        sort_top_files(&mut files, self.top_sort);
        self.top_files = files;
        self.top_selected = match self.top_selected {
            Some(ix) if ix < self.top_files.len() => Some(ix),
            _ if !self.top_files.is_empty() => Some(0),
            _ => None,
        };
        self.top_stale = false;
    }

    fn set_top_limit(&mut self, limit: usize, cx: &mut Context<Self>) {
        self.settings.list.top_files_limit = limit;
        self.settings.save();
        self.refresh_top_files();
        cx.notify();
    }

    pub(super) fn set_top_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.top_sort.0 == key {
            self.top_sort.1 = !self.top_sort.1;
        } else {
            self.top_sort = (key, key != SortKey::Name);
        }
        let selected = self
            .top_selected
            .and_then(|ix| self.top_files.get(ix))
            .map(|f| f.path.clone());
        sort_top_files(&mut self.top_files, self.top_sort);
        self.top_selected = selected
            .and_then(|path| self.top_files.iter().position(|f| f.path == path))
            .or_else(|| (!self.top_files.is_empty()).then_some(0));
        if let Some(ix) = self.top_selected {
            self.top_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub(super) fn select_top_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.top_files.is_empty() {
            return;
        }
        let ix = ix.min(self.top_files.len() - 1);
        self.top_selected = Some(ix);
        self.marked.clear();
        self.anchor = None;
        self.top_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    /// Show a top-files entry in its place in the directory tree.
    pub(super) fn reveal_top_file(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.top_files.get(ix).cloned() else {
            return;
        };
        if self.filter.is_active() {
            self.clear_filters(window, cx);
        }
        self.tab = ListTab::Tree;
        self.marked.clear();
        self.reveal_in_list(entry.path, ClickMode::Plain, cx);
        self.focus.focus(window, cx);
    }

    fn render_top_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (key, desc) = self.top_sort;
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
                .when(w.is_none(), |d| d.flex_1().min_w(px(220.)).pl(px(8.)))
                .when(right, |d| d.justify_end())
                .hover(|s| s.bg(cx.theme().list_hover))
                .when(active, |d| d.text_color(cx.theme().foreground))
                .child(label)
                .when(active, |d| d.child(Icon::new(arrow).xsmall()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_top_sort(k, cx)))
        };
        let limit = self.settings.list.top_files_limit.clamp(1, 100_000);
        let entity = cx.entity().downgrade();
        let top_labels: Vec<(usize, String)> = [100usize, 500, 1000, 5000]
            .iter()
            .map(|&n| {
                (
                    n,
                    self.tr_fmt("list.top_n", &[("n", &self.count(n as u64))]),
                )
            })
            .collect();
        let top_button = self.tr_fmt("list.top_n", &[("n", &self.count(limit as u64))]);
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
            .child(head("top-name", self.tr("list.col_name"), SortKey::Name, None, false, cx))
            .child(head(
                "top-size",
                self.tr("list.col_size"),
                SortKey::Size,
                Some(COL_SIZE),
                true,
                cx,
            ))
            .child(head(
                "top-disk",
                self.tr("list.col_disk"),
                SortKey::Disk,
                Some(COL_DISK),
                true,
                cx,
            ))
            .child(head(
                "top-date",
                self.tr("list.col_date"),
                SortKey::Modified,
                Some(COL_DATE),
                true,
                cx,
            ))
            .child(
                Button::new("top-limit")
                    .ghost()
                    .small()
                    .label(top_button)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (n, label) in top_labels.iter() {
                            let n = *n;
                            let entity = entity.clone();
                            menu = menu.item(
                                gpui_kit::component::menu::PopupMenuItem::new(label.clone())
                                    .checked(limit == n)
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| this.set_top_limit(n, cx)).ok();
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

    fn render_top_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.top_files.len();
        let entity = cx.entity();
        let theme = cx.theme();
        let body: AnyElement = if count == 0 {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(self.tr("list.no_files"))
                .into_any_element()
        } else {
            div()
                .id("top-body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list("top-rows", count, cx.processor(Self::render_top_rows))
                        .size_full()
                        .track_scroll(&self.top_scroll),
                )
                .child(Scrollbar::vertical(&self.top_scroll))
                .context_menu(move |menu, _, cx| entity.read(cx).build_context_menu(menu, cx))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(self.render_top_header(cx))
            .child(body)
    }

    fn render_top_rows(
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
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let even = theme.list_even;
        let selected = self.top_selected;
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
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(entry) = self.top_files.get(ix) else {
                continue;
            };
            let is_sel = selected == Some(ix);
            let (r, g, b) = treemap::ext_color(entry.ext);
            let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
            let parent = tree
                .fs_path(&entry.path)
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .map(|p| {
                    p.strip_prefix(&tree.root_path)
                        .unwrap_or(&p)
                        .display()
                        .to_string()
                })
                .unwrap_or_default();
            out.push(
                h_flex()
                    .id(("top-row", ix))
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
                            this.select_top_row(ix, cx);
                            if ev.click_count == 2 {
                                this.reveal_top_file(ix, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, window, cx| {
                            this.focus.focus(window, cx);
                            this.select_top_row(ix, cx);
                        }),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(220.))
                            .overflow_hidden()
                            .gap_2()
                            .pl_2()
                            .child(Icon::new(IconName::File).xsmall().text_color(color))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(entry.name.to_string())),
                            )
                            .when(!parent.is_empty(), |d| {
                                d.child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(muted)
                                        .child(parent),
                                )
                            }),
                    )
                    .child(num(COL_SIZE, format::bytes(entry.size)).font_weight(FontWeight::MEDIUM))
                    .child(num(COL_DISK, format::bytes(entry.disk_size)))
                    .child(num(COL_DATE, self.date(entry.modified)).text_color(muted))
                    .into_any_element(),
            );
        }
        out
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
        let total = tree.root().size.max(1);
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
            let (r, g, b) = treemap::ext_color_with(
                palette_of(self.settings.treemap.palette),
                ext,
            );
            let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
            let is_sel = selected == Some(ext);
            let bytes_s = self.bytes(e.bytes);
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
                            .child(bytes_s),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .text_right()
                            .text_color(muted)
                            .child(format::percent_in(
                                e.bytes,
                                total,
                                &self.settings.locale.language,
                            )),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .text_right()
                            .text_color(muted)
                            .child(self.count(e.files)),
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
                    .child(div().flex_1().min_w(px(50.)).child(self.tr("list.ext_col")))
                    .child(div().w(px(70.)).text_right().child(self.tr("list.bytes_col")))
                    .child(div().w(px(48.)).text_right().child(self.tr("list.col_pct")))
                    .child(div().w(px(64.)).text_right().child(self.tr("list.files_col"))),
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

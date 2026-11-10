//! The directory list and extension list: sorting, rows and rendering.

use super::*;

fn change_rank(diff: Option<&ScanDiff>, node: &Node) -> i64 {
    diff.and_then(|diff| diff.change(node))
        .map(|change| change.delta)
        .unwrap_or(0)
}

fn change_status_rank(diff: Option<&ScanDiff>, node: &Node) -> u8 {
    match diff.and_then(|diff| diff.change(node)) {
        Some(change) => change.status.rank(),
        None => 3, // unchanged sorts after everything that moved
    }
}

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
            SortKey::Owner => a.owner.cmp(&b.owner),
            _ => a.size.cmp(&b.size),
        }
        .then_with(|| a.name.cmp(&b.name));
        if desc { ord.reverse() } else { ord }
    });
}

impl FastDir {
    pub(super) fn child_order(
        tree: &Tree,
        diff: Option<&ScanDiff>,
        node: &Node,
        sort: (SortKey, bool),
    ) -> Vec<u32> {
        let c = tree.children(node);
        let mut idx: Vec<u32> = (0..c.len() as u32).collect();
        let (key, desc) = sort;
        if key == SortKey::Size && desc {
            return idx; // children are stored in this order
        }
        idx.sort_by(|&ia, &ib| {
            let (a, b) = (&c[ia as usize], &c[ib as usize]);
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
                SortKey::Owner => tree
                    .owner_name(tree.child_owner(node, ia as usize))
                    .unwrap_or("")
                    .to_lowercase()
                    .cmp(
                        &tree
                            .owner_name(tree.child_owner(node, ib as usize))
                            .unwrap_or("")
                            .to_lowercase(),
                    ),
                SortKey::Delta => change_rank(diff, a).cmp(&change_rank(diff, b)),
                SortKey::Status => change_status_rank(diff, a).cmp(&change_status_rank(diff, b)),
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
        let diff = self.compare.as_ref().map(|compare| compare.diff.clone());
        let diff = diff.as_deref();
        let tree = tree.read();
        let hide_empty = self.settings.list.hide_empty_folders;
        /// A folder is "empty" when nothing below it is a file (so a folder
        /// holding only empty folders counts too). Unreadable folders are
        /// kept: their contents are unknown. The root is never hidden.
        fn hidden(node: &Node, depth: u16, hide_empty: bool) -> bool {
            hide_empty && depth > 0 && node.is_dir() && !node.error() && node.files == 0
        }
        /// Add a node and its expanded descendants.
        #[allow(clippy::too_many_arguments)]
        fn walk(
            tree: &Tree,
            diff: Option<&ScanDiff>,
            node: &Node,
            path: &mut NodePath,
            depth: u16,
            rows: &mut Vec<Row>,
            sort: (SortKey, bool),
            hide_empty: bool,
        ) {
            if hidden(node, depth, hide_empty) {
                return;
            }
            rows.push(Row {
                path: path.clone(),
                depth,
                matched: false,
            });
            if node.expanded() && node.is_dir() {
                let children = tree.children(node);
                for i in FastDir::child_order(tree, diff, node, sort) {
                    path.push(i);
                    walk(
                        tree,
                        diff,
                        &children[i as usize],
                        path,
                        depth + 1,
                        rows,
                        sort,
                        hide_empty,
                    );
                    path.pop();
                }
            }
        }
        /// Keep every match plus the ancestors needed to reach it. Returns
        /// whether this subtree contributed a row.
        #[allow(clippy::too_many_arguments)]
        fn walk_filtered(
            tree: &Tree,
            diff: Option<&ScanDiff>,
            node: &Node,
            path: &mut NodePath,
            depth: u16,
            rows: &mut Vec<Row>,
            sort: (SortKey, bool),
            filter: &Filter,
            owner_filter: Option<u16>,
            hide_empty: bool,
        ) -> bool {
            if hidden(node, depth, hide_empty) {
                return false;
            }
            let start = rows.len();
            let mut any = false;
            if node.is_dir() {
                let children = tree.children(node);
                for i in FastDir::child_order(tree, diff, node, sort) {
                    path.push(i);
                    any |= walk_filtered(
                        tree,
                        diff,
                        &children[i as usize],
                        path,
                        depth + 1,
                        rows,
                        sort,
                        filter,
                        owner_filter,
                        hide_empty,
                    );
                    path.pop();
                }
            }
            let owner_ok = owner_filter.is_none_or(|want| tree.owner_at(path) == want);
            let matched = owner_ok && filter.matches(tree.name(node), node, tree.ext_name(node));
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
        let owner_filter = self.selected_owner;
        if self.filter.is_active() || owner_filter.is_some() {
            walk_filtered(
                &tree,
                diff,
                tree.root(),
                &mut path,
                0,
                &mut self.rows,
                self.sort,
                &self.filter,
                owner_filter,
                hide_empty,
            );
            // Start on the first actual match, not on an ancestor shown for
            // context.
            let previous = selected_path
                .and_then(|sel| self.rows.iter().position(|r| r.path == sel && r.matched));
            self.selected = previous.or_else(|| self.rows.iter().position(|r| r.matched));
        } else {
            walk(
                &tree,
                diff,
                tree.root(),
                &mut path,
                0,
                &mut self.rows,
                self.sort,
                hide_empty,
            );
            self.selected = selected_path
                .and_then(|sel| self.rows.iter().position(|r| r.path == sel))
                .or_else(|| (!self.rows.is_empty()).then_some(0));
        }
        self.rebuild_ext_rows(&tree);
    }

    /// Rebuild the extension panel rows for the current grouping mode.
    fn rebuild_ext_rows(&mut self, tree: &Tree) {
        self.ext_rows.clear();
        self.owner_rows.clear();
        match self.settings.list.ext_grouping {
            ExtGrouping::Owner if tree.has_owners() => {
                let (key, desc) = self.owner_sort;
                let mut rows = tree.owner_totals();
                rows.sort_unstable_by(|a, b| {
                    let ord = match key {
                        SortKey::Name => tree
                            .owner_name(a.owner)
                            .unwrap_or("")
                            .to_lowercase()
                            .cmp(&tree.owner_name(b.owner).unwrap_or("").to_lowercase()),
                        SortKey::Disk => a.disk_size.cmp(&b.disk_size),
                        SortKey::Files => a.files.cmp(&b.files),
                        _ => a.size.cmp(&b.size),
                    };
                    if desc { ord.reverse() } else { ord }
                });
                self.owner_rows = rows;
                self.ext_rows = self
                    .owner_rows
                    .iter()
                    .map(|total| ExtRow::Owner { owner: total.owner })
                    .collect();
            }
            ExtGrouping::Extension => {
                self.ext_rows = (0..tree.exts.len() as u16)
                    .filter(|&i| tree.exts[i as usize].files > 0)
                    .map(|ext| ExtRow::Extension { ext })
                    .collect();
            }
            ExtGrouping::Category => {
                let totals = tree.category_totals();
                let mut categories: Vec<(FileCategory, u64)> = FileCategory::ALL
                    .into_iter()
                    .filter(|cat| totals[cat.index()].files > 0)
                    .map(|cat| (cat, totals[cat.index()].bytes))
                    .collect();
                categories.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                for (category, _) in categories {
                    self.ext_rows.push(ExtRow::Category(category));
                    if self.expanded_categories.contains(&category) {
                        self.ext_rows.extend(
                            tree.exts
                                .iter()
                                .enumerate()
                                .filter(|(_, e)| e.files > 0 && e.category == category)
                                .map(|(ext, _)| ExtRow::Extension { ext: ext as u16 }),
                        );
                    }
                }
            }
            // The owner view needs owners; a saved setting pointing at it on a
            // scan without them falls back to the extension list.
            ExtGrouping::Owner => {
                self.ext_rows = (0..tree.exts.len() as u16)
                    .filter(|&i| tree.exts[i as usize].files > 0)
                    .map(|ext| ExtRow::Extension { ext })
                    .collect();
            }
        }
    }

    /// Switch the extension panel between the flat extension list and the
    /// category breakdown.
    pub(super) fn set_ext_grouping(&mut self, grouping: ExtGrouping, cx: &mut Context<Self>) {
        if self.settings.list.ext_grouping == grouping {
            return;
        }
        self.settings.list.ext_grouping = grouping;
        self.settings.save();
        // A highlight from the other view would keep dimming the treemap
        // with no visible row selected, so clear it.
        self.set_selected_ext(None, cx);
        if let Some(tree) = self.tree.clone() {
            let tree = tree.read();
            self.rebuild_ext_rows(&tree);
        }
        self.ext_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Sort the per-user totals view by one of its columns.
    pub(super) fn set_owner_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.owner_sort.0 == key {
            self.owner_sort.1 = !self.owner_sort.1;
        } else {
            self.owner_sort = (key, key != SortKey::Name);
        }
        if let Some(tree) = self.tree.clone() {
            let tree = tree.read();
            self.rebuild_ext_rows(&tree);
        }
        self.ext_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Expand or collapse one category in the category view.
    pub(super) fn toggle_category_expanded(
        &mut self,
        category: FileCategory,
        cx: &mut Context<Self>,
    ) {
        if !self.expanded_categories.remove(&category) {
            self.expanded_categories.insert(category);
        }
        if let Some(tree) = self.tree.clone() {
            let tree = tree.read();
            self.rebuild_ext_rows(&tree);
        }
        cx.notify();
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
            // Status reads best from "needs attention" downwards; everything
            // else opens on its largest/newest value.
            self.sort = (key, !matches!(key, SortKey::Name | SortKey::Status));
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
        let age_now = treemap::RenderOptions::default().now;
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
            let show_owner = show.show_owner && tree.has_owners();
            let owner_s = if show_owner {
                tree.owner_name(tree.owner_at(&row.path))
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            let size_s = self.bytes(node.size);
            let disk_s = self.bytes(node.disk_size);
            let pct_s = self.percent_of(&tree, &row.path, node);
            let date_s = self.date(node.modified());
            let comparing = self.compare.is_some();
            let change = self
                .compare
                .as_ref()
                .and_then(|compare| compare.diff.change(node))
                .copied();
            let (delta_s, delta_pct_s, status_s, delta_color) = match change {
                Some(change) => {
                    let color = if change.delta > 0 {
                        rgb(0xe74c3c)
                    } else {
                        rgb(0x2ecc71)
                    };
                    (
                        self.signed_bytes(change.delta),
                        match change.fraction() {
                            Some(_) => self.signed_percent(change.delta, change.old_size),
                            None => "—".to_string(),
                        },
                        self.change_status_label(change.status).to_string(),
                        Some(color),
                    )
                }
                None => (
                    String::new(),
                    String::new(),
                    if comparing {
                        self.tr("change.same").to_string()
                    } else {
                        String::new()
                    },
                    None,
                ),
            };
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
            let date_tint = if self.settings.list.age_tint {
                let (r, g, b) = treemap::age_color(node.modified(), age_now, palette);
                Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
            } else {
                None
            };
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
                .size(sp(16.))
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
            // Screen readers announce name, size and share; the tree
            // semantics (level, expanded, selected) are exposed natively.
            let a11y_name = format!(
                "{}, {}, {}",
                self.display_name(tree.name(node)),
                size_s,
                pct_s
            );
            let el = h_flex()
                .id(("row", ix))
                .role(Role::TreeItem)
                .aria_label(a11y_name)
                .aria_level(row.depth as usize + 1)
                .aria_selected(is_sel)
                .when(has_children, |d| d.aria_expanded(expanded))
                .h(sp(ROW_H))
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
                            // Pseudo-items have no children; double-clicking
                            // `<Unknown>` explains it, `<Shadow copies>` opens
                            // System Protection.
                            let path = this.rows.get(ix).map(|r| r.path.clone());
                            if !path.is_some_and(|p| this.open_pseudo_item(&p, window, cx)) {
                                this.set_expanded(ix, None, cx);
                            }
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
                        .min_w(sp(220.))
                        .overflow_hidden()
                        .gap_1()
                        .pl(sp(4. + depth * 16.))
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
                                .child(SharedString::from(
                                    self.display_name(tree.name(node)).to_string(),
                                )),
                        )
                        .when(node.hard_link(), |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(muted)
                                    .child(self.tr("list.hard_link")),
                            )
                        }),
                )
                .child(
                    div().w(sp(COL_BAR)).flex_shrink_0().px_2().child(
                        div()
                            .h(sp(10.))
                            .w_full()
                            .rounded_sm()
                            .bg(track)
                            .overflow_hidden()
                            .child(div().h_full().w(relative(frac)).bg(bar_color)),
                    ),
                )
                .child(num(COL_PCT, pct_s))
                .child(num(COL_SIZE, size_s).font_weight(FontWeight::MEDIUM))
                .when(comparing, |d| {
                    d.child(
                        num(COL_DELTA, delta_s)
                            .font_weight(FontWeight::MEDIUM)
                            .when_some(delta_color, |d, color| d.text_color(color)),
                    )
                    .child(
                        num(COL_DELTA_PCT, delta_pct_s)
                            .text_color(muted)
                            .when_some(delta_color, |d, color| d.text_color(color)),
                    )
                    .child(num(COL_STATUS, status_s).text_color(muted))
                })
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
                    d.child(match date_tint {
                        Some(color) => num(COL_DATE, date_s).text_color(rgb(color)),
                        None => num(COL_DATE, date_s).text_color(muted),
                    })
                })
                .when(show_owner, |d| {
                    d.child(
                        div()
                            .w(px(COL_OWNER))
                            .flex_shrink_0()
                            .px_2()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(muted)
                            .child(owner_s),
                    )
                });
            out.push(el.into_any_element());
        }
        out
    }

    /// Localized label for a comparison status.
    pub(super) fn change_status_label(&self, status: ChangeStatus) -> &'static str {
        match status {
            ChangeStatus::Added => self.tr("change.new"),
            ChangeStatus::Grown => self.tr("change.grew"),
            ChangeStatus::Shrunk => self.tr("change.shrank"),
        }
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
        if self.selected_ext == ext
            && self.selected_category.is_none()
            && self.selected_owner.is_none()
        {
            return;
        }
        self.selected_ext = ext;
        self.selected_category = None;
        self.treemap.update(cx, |t, cx| t.set_highlight(ext, cx));
        if self.selected_owner.take().is_some() {
            // The owner view was filtering the list; clearing it re-filters.
            self.rebuild_rows();
        }
        cx.notify();
    }

    /// Highlight every file in one category in the treemap (`None` clears it).
    pub(super) fn set_selected_category(
        &mut self,
        category: Option<FileCategory>,
        cx: &mut Context<Self>,
    ) {
        if self.selected_category == category
            && self.selected_ext.is_none()
            && self.selected_owner.is_none()
        {
            return;
        }
        self.selected_category = category;
        self.selected_ext = None;
        let index = category.map(|c| c.index() as u8);
        self.treemap
            .update(cx, |t, cx| t.set_highlight_category(index, cx));
        if self.selected_owner.take().is_some() {
            self.rebuild_rows();
        }
        cx.notify();
    }

    /// Highlight every file of one owner in the treemap and filter the list
    /// to that account. `None` clears both.
    pub(super) fn set_selected_owner(&mut self, owner: Option<u16>, cx: &mut Context<Self>) {
        if self.selected_owner == owner
            && self.selected_ext.is_none()
            && self.selected_category.is_none()
        {
            return;
        }
        self.selected_owner = owner;
        self.selected_ext = None;
        self.selected_category = None;
        self.treemap
            .update(cx, |t, cx| t.set_highlight_owner(owner, cx));
        self.rebuild_rows();
        cx.notify();
    }

    /// Clicking a user row toggles their treemap highlight and list filter.
    pub(super) fn toggle_owner(&mut self, owner: u16, cx: &mut Context<Self>) {
        let owner = if self.selected_owner == Some(owner) {
            None
        } else {
            Some(owner)
        };
        self.set_selected_owner(owner, cx);
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

    /// Clicking a category row toggles its treemap highlight.
    pub(super) fn toggle_category(&mut self, category: FileCategory, cx: &mut Context<Self>) {
        let category = if self.selected_category == Some(category) {
            None
        } else {
            Some(category)
        };
        self.set_selected_category(category, cx);
    }

    /// A selected file row also highlights that file's extension (or its
    /// category, when the treemap is coloured by category).
    pub(super) fn highlight_ext_of_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected_owner.is_some() {
            // The owner view is highlighting and filtering; clicking one of
            // its rows should not drop that filter.
            return;
        }
        let info = self.tree.as_ref().and_then(|tree| {
            let tree = tree.read();
            let row = self.rows.get(ix)?;
            let node = tree.get(&row.path)?;
            (!node.is_dir() && node.ext != NO_EXT)
                .then_some((node.ext, tree.ext_category(node.ext)))
        });
        if let Some((ext, category)) = info {
            if self.settings.treemap.color_mode == TreemapColorMode::Category {
                self.set_selected_category(Some(category), cx);
            } else {
                self.set_selected_ext(Some(ext), cx);
            }
        }
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let comparing = self.compare.is_some();
        let (key, desc) = self.sort;
        let show = &self.settings.list;
        let (show_items, show_files, show_folders, show_modified, show_disk) = (
            show.show_items,
            show.show_files,
            show.show_folders,
            show.show_modified,
            show.show_disk,
        );
        let show_owner = show.show_owner
            && self
                .tree
                .as_ref()
                .is_some_and(|tree| tree.read().has_owners());
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
                .when(w.is_none(), |d| d.flex_1().min_w(sp(220.)).pl(sp(24.)))
                .when(right, |d| d.justify_end())
                .hover(|s| s.bg(cx.theme().list_hover))
                .when(active, |d| d.text_color(cx.theme().foreground))
                .child(label)
                .when(active, |d| d.child(Icon::new(arrow).xsmall()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sort(k, cx)))
        };
        h_flex()
            .h(sp(28.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.list_head)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(head(
                "h-name",
                self.tr("list.col_name"),
                SortKey::Name,
                None,
                false,
                cx,
            ))
            .child(
                div()
                    .w(sp(COL_BAR))
                    .flex_shrink_0()
                    .px_2()
                    .child(self.tr("list.col_subtree")),
            )
            .child(head(
                "h-pct",
                self.tr("list.col_pct"),
                SortKey::Size,
                Some(COL_PCT),
                true,
                cx,
            ))
            .child(head(
                "h-size",
                self.tr("list.col_size"),
                SortKey::Size,
                Some(COL_SIZE),
                true,
                cx,
            ))
            .when(comparing, |d| {
                d.child(head(
                    "h-delta",
                    self.tr("list.col_delta"),
                    SortKey::Delta,
                    Some(COL_DELTA),
                    true,
                    cx,
                ))
                .child(
                    div()
                        .w(sp(COL_DELTA_PCT))
                        .flex_shrink_0()
                        .px_2()
                        .text_right()
                        .child(self.tr("list.col_delta_pct")),
                )
                .child(head(
                    "h-status",
                    self.tr("list.col_status"),
                    SortKey::Status,
                    Some(COL_STATUS),
                    true,
                    cx,
                ))
            })
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
            .when(show_owner, |d| {
                d.child(head(
                    "h-owner",
                    self.tr("list.col_owner"),
                    SortKey::Owner,
                    Some(COL_OWNER),
                    false,
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
                ListTab::Removed => self.render_removed_table(cx).into_any_element(),
                ListTab::Duplicates => self.render_duplicates_table(cx),
                ListTab::Cleanup => self.render_cleanup_table(cx),
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
        let removed_tab = self.tab == ListTab::Removed;
        let dups_tab = self.tab == ListTab::Duplicates;
        let cleanup_tab = self.tab == ListTab::Cleanup;
        let removed_count = self
            .compare
            .as_ref()
            .map(|compare| compare.diff.removed().len())
            .unwrap_or(0);
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
            .h(sp(30.))
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
                self.tr_fmt(
                    "list.top_tab",
                    &[("n", &self.count(self.top_files.len() as u64))],
                ),
                top_tab,
                ListTab::TopFiles,
                cx,
            ))
            .when(removed_count > 0, |d| {
                d.child(tab(
                    "tab-removed",
                    self.tr_fmt(
                        "list.removed_tab",
                        &[("n", &self.count(removed_count as u64))],
                    ),
                    removed_tab,
                    ListTab::Removed,
                    cx,
                ))
            })
            .child(tab(
                "tab-duplicates",
                self.duplicates_tab_label(),
                dups_tab,
                ListTab::Duplicates,
                cx,
            ))
            .child(tab(
                "tab-cleanup",
                self.cleanup_tab_label(),
                cleanup_tab,
                ListTab::Cleanup,
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
                .context_menu(move |menu, window, cx| {
                    actions_ui::build_context_menu(entity.clone(), menu, window, cx)
                })
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
        match tab {
            ListTab::TopFiles => {
                if self.top_stale {
                    self.refresh_top_files();
                }
                if self.top_selected.is_none() && !self.top_files.is_empty() {
                    self.top_selected = Some(0);
                }
            }
            ListTab::Removed => {
                let count = self
                    .compare
                    .as_ref()
                    .map(|compare| compare.removed_order.len())
                    .unwrap_or(0);
                if self.removed_selected.is_none() && count > 0 {
                    self.removed_selected = Some(0);
                }
            }
            ListTab::Duplicates => {
                if self.dups.selected.is_none() && !self.dups.rows.is_empty() {
                    self.dups.selected = Some(0);
                }
            }
            ListTab::Cleanup => {
                if matches!(self.cleanup.state, cleanup_ui::CleanupState::Idle) {
                    self.start_cleanup_search(cx);
                }
            }
            ListTab::Tree => {}
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
        let show_owner = self.settings.list.show_owner
            && self
                .tree
                .as_ref()
                .is_some_and(|tree| tree.read().has_owners());
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
                .when(w.is_none(), |d| d.flex_1().min_w(sp(220.)).pl(sp(8.)))
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
            .h(sp(28.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.list_head)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(head(
                "top-name",
                self.tr("list.col_name"),
                SortKey::Name,
                None,
                false,
                cx,
            ))
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
            .when(show_owner, |d| {
                d.child(head(
                    "top-owner",
                    self.tr("list.col_owner"),
                    SortKey::Owner,
                    Some(COL_OWNER),
                    false,
                    cx,
                ))
            })
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
                                        entity
                                            .update(cx, |this, cx| this.set_top_limit(n, cx))
                                            .ok();
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
                .context_menu(move |menu, window, cx| {
                    actions_ui::build_context_menu(entity.clone(), menu, window, cx)
                })
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
        let show_owner = self.settings.list.show_owner && tree.has_owners();
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
                    .h(sp(ROW_H))
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
                            .min_w(sp(220.))
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
                    .when(show_owner, |d| {
                        d.child(
                            div()
                                .w(px(COL_OWNER))
                                .flex_shrink_0()
                                .px_2()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(muted)
                                .child(SharedString::from(
                                    tree.owner_name(entry.owner).unwrap_or("").to_string(),
                                )),
                        )
                    })
                    .into_any_element(),
            );
        }
        out
    }

    /// Header of the "Removed" list: still sortable, but every row is a loss.
    fn render_removed_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (key, desc) = self.removed_sort;
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
                .when(w.is_none(), |d| d.flex_1().min_w(sp(220.)).pl(sp(8.)))
                .when(right, |d| d.justify_end())
                .hover(|s| s.bg(cx.theme().list_hover))
                .when(active, |d| d.text_color(cx.theme().foreground))
                .child(label)
                .when(active, |d| d.child(Icon::new(arrow).xsmall()))
                .on_click(cx.listener(move |this, _, _, cx| this.set_removed_sort(k, cx)))
        };
        h_flex()
            .h(sp(28.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.list_head)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(head(
                "removed-name",
                self.tr("list.col_name"),
                SortKey::Name,
                None,
                false,
                cx,
            ))
            .child(head(
                "removed-size",
                self.tr("list.col_size"),
                SortKey::Size,
                Some(COL_SIZE),
                true,
                cx,
            ))
            .child(head(
                "removed-disk",
                self.tr("list.col_disk"),
                SortKey::Disk,
                Some(COL_DISK),
                true,
                cx,
            ))
            .child(head(
                "removed-date",
                self.tr("list.col_date"),
                SortKey::Modified,
                Some(COL_DATE),
                true,
                cx,
            ))
            .child(
                div()
                    .w(sp(COL_STATUS))
                    .flex_shrink_0()
                    .px_2()
                    .text_right()
                    .child(self.tr("list.col_status")),
            )
    }

    fn render_removed_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self
            .compare
            .as_ref()
            .map(|compare| compare.removed_order.len())
            .unwrap_or(0);
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
                .child(self.tr("list.removed_empty"))
                .into_any_element()
        } else {
            div()
                .id("removed-body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    uniform_list(
                        "removed-rows",
                        count,
                        cx.processor(Self::render_removed_rows),
                    )
                    .size_full()
                    .track_scroll(&self.removed_scroll),
                )
                .child(Scrollbar::vertical(&self.removed_scroll))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .child(self.render_removed_header(cx))
            .child(body)
    }

    fn render_removed_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(compare) = self.compare.as_ref() else {
            return Vec::new();
        };
        let removed = compare.diff.removed();
        let order = &compare.removed_order;
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let even = theme.list_even;
        let selected = self.removed_selected;
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
            let Some(&entry_ix) = order.get(ix) else {
                continue;
            };
            let Some(entry) = removed.get(entry_ix) else {
                continue;
            };
            let is_sel = selected == Some(ix);
            let icon = if entry.is_dir {
                Icon::new(IconName::Folder)
                    .xsmall()
                    .text_color(rgb(0xe0b04a))
            } else {
                Icon::new(IconName::File).xsmall().text_color(muted)
            };
            out.push(
                h_flex()
                    .id(("removed-row", ix))
                    .h(sp(ROW_H))
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
                        cx.listener(move |this, _, window, cx| {
                            this.focus.focus(window, cx);
                            this.select_removed_row(ix, cx);
                        }),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(sp(220.))
                            .overflow_hidden()
                            .gap_2()
                            .pl_2()
                            .child(icon)
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(muted)
                                    .child(SharedString::from(entry.path.clone())),
                            ),
                    )
                    .child(num(COL_SIZE, self.bytes(entry.size)))
                    .child(num(COL_DISK, self.bytes(entry.disk_size)))
                    .child(num(COL_DATE, self.date(entry.modified)).text_color(muted))
                    .child(
                        num(COL_STATUS, self.tr("change.removed").to_string()).text_color(danger),
                    )
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
        let totals = tree.category_totals();
        let muted = theme.muted_foreground;
        let even = theme.list_even;
        let active = theme.list_active;
        let active_border = theme.list_active_border;
        let hover = theme.list_hover;
        let selected_ext = self.selected_ext;
        let selected_category = self.selected_category;
        let selected_owner = self.selected_owner;
        let grouped = self.settings.list.ext_grouping == ExtGrouping::Category;
        let palette = palette_of(self.settings.treemap.palette);
        let lang = self.settings.locale.language.clone();
        let category_total: u64 = totals.iter().map(|t| t.bytes).sum();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(&row) = self.ext_rows.get(ix) else {
                continue;
            };
            match row {
                ExtRow::Category(category) => {
                    let cat_total = &totals[category.index()];
                    let (r, g, b) = treemap::category_color(category.index());
                    let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
                    let is_sel = selected_category == Some(category);
                    let expanded = self.expanded_categories.contains(&category);
                    let frac = cat_total.bytes as f32 / category_total.max(1) as f32;
                    out.push(
                        h_flex()
                            .id(("cat", ix))
                            .role(Role::ListBoxOption)
                            .aria_label(format!(
                                "{}, {}, {}",
                                fastdir_locale::category_name(category.id(), &lang),
                                self.bytes(cat_total.bytes),
                                format::percent_in(cat_total.bytes, total, &lang)
                            ))
                            .aria_selected(is_sel)
                            .relative()
                            .h(sp(ROW_H))
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
                                    this.toggle_category(category, cx);
                                }),
                            )
                            .child(
                                div()
                                    .id(("cat-chev", ix))
                                    .size(sp(16.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .hover(|s| s.bg(hover))
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
                                            this.toggle_category_expanded(category, cx);
                                        }),
                                    ),
                            )
                            .child(div().size(sp(10.)).rounded_sm().flex_shrink_0().bg(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(sp(50.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(
                                        fastdir_locale::category_name(category.id(), &lang)
                                            .to_string(),
                                    )),
                            )
                            .child(
                                div()
                                    .w(sp(70.))
                                    .text_right()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.bytes(cat_total.bytes)),
                            )
                            .child(
                                div()
                                    .w(sp(48.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(format::percent_in(cat_total.bytes, total, &lang)),
                            )
                            .child(
                                div()
                                    .w(sp(64.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(self.count(cat_total.files)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .bottom_0()
                                    .h(px(2.))
                                    .w(relative(frac.clamp(0.0, 1.0)))
                                    .bg(color),
                            )
                            .into_any_element(),
                    );
                }
                ExtRow::Owner { owner } => {
                    let Some(owner_total) =
                        self.owner_rows.iter().find(|total| total.owner == owner)
                    else {
                        continue;
                    };
                    let (r, g, b) = treemap::ext_color_with(palette, owner);
                    let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
                    let is_sel = selected_owner == Some(owner);
                    let name = tree
                        .owner_name(owner)
                        .unwrap_or_else(|| self.tr("list.unknown_owner"))
                        .to_string();
                    out.push(
                        h_flex()
                            .id(("owner", ix))
                            .role(Role::ListBoxOption)
                            .aria_label(format!(
                                "{}, {}, {}",
                                name,
                                self.bytes(owner_total.size),
                                format::percent_in(owner_total.size, total, &lang)
                            ))
                            .aria_selected(is_sel)
                            .h(sp(ROW_H))
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
                                    this.toggle_owner(owner, cx);
                                }),
                            )
                            .child(div().size(sp(10.)).rounded_sm().flex_shrink_0().bg(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(sp(50.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .w(sp(64.))
                                    .text_right()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.bytes(owner_total.size)),
                            )
                            .child(
                                div()
                                    .w(sp(70.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(self.bytes(owner_total.disk_size)),
                            )
                            .child(
                                div()
                                    .w(sp(44.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(format::percent_in(owner_total.size, total, &lang)),
                            )
                            .child(
                                div()
                                    .w(sp(56.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(self.count(owner_total.files)),
                            )
                            .into_any_element(),
                    );
                }
                ExtRow::Extension { ext } => {
                    let Some(e) = tree.exts.get(ext as usize) else {
                        continue;
                    };
                    let (r, g, b) = treemap::ext_color_with(palette, ext);
                    let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
                    let is_sel = selected_ext == Some(ext);
                    out.push(
                        h_flex()
                            .id(("ext", ix))
                            .role(Role::ListBoxOption)
                            .aria_label(format!(
                                "{}, {}, {}",
                                e.name,
                                self.bytes(e.bytes),
                                format::percent_in(e.bytes, total, &lang)
                            ))
                            .aria_selected(is_sel)
                            .h(sp(ROW_H))
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
                            .when(grouped, |d| d.child(div().w(sp(16.)).flex_shrink_0()))
                            .child(div().size(sp(10.)).rounded_sm().flex_shrink_0().bg(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(sp(50.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(e.name.to_string())),
                            )
                            .child(
                                div()
                                    .w(sp(70.))
                                    .text_right()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.bytes(e.bytes)),
                            )
                            .child(
                                div()
                                    .w(sp(48.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(format::percent_in(e.bytes, total, &lang)),
                            )
                            .child(
                                div()
                                    .w(sp(64.))
                                    .text_right()
                                    .text_color(muted)
                                    .child(self.count(e.files)),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        out
    }

    pub(super) fn render_ext_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.ext_rows.len();
        let grouped = self.settings.list.ext_grouping == ExtGrouping::Category;
        let owners = self.settings.list.ext_grouping == ExtGrouping::Owner
            && self
                .tree
                .as_ref()
                .is_some_and(|tree| tree.read().has_owners());
        let flat_ext = !grouped && !owners;
        let language = self.settings.locale.language.clone();
        let totals = if grouped {
            self.tree.as_ref().map(|tree| tree.read().category_totals())
        } else {
            None
        };
        let category_total: u64 = totals
            .as_ref()
            .map(|t| t.iter().map(|c| c.bytes).sum())
            .unwrap_or(0);
        let (owner_key, owner_desc) = self.owner_sort;
        let owner_fg = theme.foreground;
        let owner_hover = theme.list_hover;
        let head_bg = theme.list_head;
        let head_border = theme.border;
        let owner_head = |id: &'static str,
                          label: &'static str,
                          key: SortKey,
                          w: Option<f32>,
                          cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .h_full()
                .px_2()
                .gap_1()
                .cursor_pointer()
                .when_some(w, |d, w| d.w(sp(w)).flex_shrink_0().justify_end())
                .when(w.is_none(), |d| d.flex_1().min_w(sp(50.)))
                .when(owner_key == key, |d| d.text_color(owner_fg))
                .hover(|s| s.bg(owner_hover))
                .child(label)
                .when(owner_key == key, |d| {
                    d.child(
                        Icon::new(if owner_desc {
                            IconName::ArrowDown
                        } else {
                            IconName::ArrowUp
                        })
                        .xsmall(),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_owner_sort(key, cx)))
        };
        v_flex()
            .size_full()
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(sp(24.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .text_xs()
                    .child(
                        h_flex()
                            .id("ext-grouping")
                            .gap_0p5()
                            .child(
                                div()
                                    .id("ext-by-extension")
                                    .px_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(flat_ext, |d| d.bg(theme.list_active))
                                    .hover(|s| s.bg(theme.list_hover))
                                    .child(SharedString::from(
                                        fastdir_locale::t(&language, "list.by_extension")
                                            .to_string(),
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.set_ext_grouping(ExtGrouping::Extension, cx);
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("ext-by-category")
                                    .px_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(grouped, |d| d.bg(theme.list_active))
                                    .hover(|s| s.bg(theme.list_hover))
                                    .child(SharedString::from(
                                        fastdir_locale::t(&language, "list.by_category")
                                            .to_string(),
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.set_ext_grouping(ExtGrouping::Category, cx);
                                        }),
                                    ),
                            )
                            .when(
                                self.tree
                                    .as_ref()
                                    .is_some_and(|tree| tree.read().has_owners()),
                                |d| {
                                    d.child(
                                        div()
                                            .id("ext-by-owner")
                                            .px_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .when(owners, |d| d.bg(theme.list_active))
                                            .hover(|s| s.bg(theme.list_hover))
                                            .child(SharedString::from(
                                                fastdir_locale::t(&language, "list.by_owner")
                                                    .to_string(),
                                            ))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.set_ext_grouping(ExtGrouping::Owner, cx);
                                                }),
                                            ),
                                    )
                                },
                            ),
                    ),
            )
            .child(
                h_flex()
                    .h(sp(24.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().w(sp(10.)))
                    .when(owners, |d| {
                        d.child(owner_head(
                            "owner-h-name",
                            self.tr("list.owner_col"),
                            SortKey::Name,
                            None,
                            cx,
                        ))
                        .child(owner_head(
                            "owner-h-size",
                            self.tr("list.bytes_col"),
                            SortKey::Size,
                            Some(64.),
                            cx,
                        ))
                        .child(owner_head(
                            "owner-h-disk",
                            self.tr("list.col_disk"),
                            SortKey::Disk,
                            Some(70.),
                            cx,
                        ))
                        .child(owner_head(
                            "owner-h-pct",
                            self.tr("list.col_pct"),
                            SortKey::Size,
                            Some(44.),
                            cx,
                        ))
                        .child(owner_head(
                            "owner-h-files",
                            self.tr("list.files_col"),
                            SortKey::Files,
                            Some(56.),
                            cx,
                        ))
                    })
                    .when(!owners, |d| {
                        d.child(div().flex_1().min_w(sp(50.)).child(if grouped {
                            self.tr("list.category_col")
                        } else {
                            self.tr("list.ext_col")
                        }))
                        .child(
                            div()
                                .w(sp(70.))
                                .text_right()
                                .child(self.tr("list.bytes_col")),
                        )
                        .child(
                            div()
                                .w(sp(48.))
                                .text_right()
                                .child(self.tr("list.col_pct")),
                        )
                        .child(
                            div()
                                .w(sp(64.))
                                .text_right()
                                .child(self.tr("list.files_col")),
                        )
                    }),
            )
            .when_some(totals.filter(|_| category_total > 0), |this, totals| {
                this.child(
                    h_flex()
                        .h(sp(14.))
                        .w_full()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(head_border)
                        .bg(head_bg)
                        .children(totals.iter().enumerate().filter(|(_, t)| t.bytes > 0).map(
                            |(index, t)| {
                                let (r, g, b) = treemap::category_color(index);
                                let color = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32);
                                div()
                                    .h_full()
                                    .w(relative(t.bytes as f32 / category_total.max(1) as f32))
                                    .bg(color)
                            },
                        )),
                )
            })
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

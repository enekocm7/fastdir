//! Selection model, keyboard navigation and treemap zoom.

use super::*;

impl FastDir {
    /// The cursor row of the directory tree, regardless of the active tab.
    pub(super) fn selected_tree_path(&self) -> Option<NodePath> {
        self.selected
            .and_then(|i| self.rows.get(i))
            .map(|r| r.path.clone())
    }

    /// The active selection: the tree cursor or the top-files row.
    pub(super) fn selected_path(&self) -> Option<NodePath> {
        match self.tab {
            ListTab::Tree => self.selected_tree_path(),
            ListTab::TopFiles => self
                .top_selected
                .and_then(|i| self.top_files.get(i))
                .map(|f| f.path.clone()),
        }
    }

    pub(super) fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = ix.min(self.rows.len() - 1);
        self.selected = Some(ix);
        self.marked.clear();
        self.anchor = Some(self.rows[ix].path.clone());
        self.list_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    /// Move the cursor to `ix` keeping the anchor, selecting the range
    /// between them (Shift+arrows).
    pub(super) fn extend_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = ix.min(self.rows.len() - 1);
        if self.anchor.is_none() {
            self.anchor = self.selected_path();
        }
        let path = self.rows[ix].path.clone();
        self.list_scroll.scroll_to_item(ix, ScrollStrategy::Top);
        self.apply_click(path, ix, ClickMode::Range, cx);
    }

    /// Apply a click on the row/cell `path` (currently at row `ix`) to the
    /// selection and tell the treemap.
    pub(super) fn apply_click(
        &mut self,
        path: NodePath,
        ix: usize,
        mode: ClickMode,
        cx: &mut Context<Self>,
    ) {
        // Drive pseudo-items are never part of a multi-selection.
        let is_special = self
            .tree
            .as_ref()
            .and_then(|t| t.read().get(&path).map(|n| n.is_special()))
            .unwrap_or(false);
        match mode {
            ClickMode::Context if self.marked.len() > 1 && self.marked.contains(&path) => {
                self.selected = Some(ix);
            }
            ClickMode::Toggle if !path.is_empty() && !is_special => {
                if self.marked.is_empty() {
                    if let Some(cur) = self.selected_path().filter(|p| !p.is_empty()) {
                        // Never seed a multi-selection with a pseudo-item.
                        let cur_special = self
                            .tree
                            .as_ref()
                            .and_then(|t| t.read().get(&cur).map(|n| n.is_special()))
                            .unwrap_or(false);
                        if !cur_special {
                            self.marked.insert(cur);
                        }
                    }
                }
                if !self.marked.remove(&path) {
                    self.marked.insert(path.clone());
                }
                // Unticking the last item leaves nothing selected.
                self.selected = (!self.marked.is_empty()).then_some(ix);
                self.anchor = Some(path);
            }
            ClickMode::Range if !path.is_empty() && !is_special => {
                let from = self
                    .anchor
                    .as_ref()
                    .and_then(|a| self.rows.iter().position(|r| &r.path == a))
                    .or(self.selected)
                    .unwrap_or(ix);
                let (lo, hi) = (from.min(ix), from.max(ix));
                self.marked = self.rows[lo..=hi]
                    .iter()
                    .filter(|r| !r.path.is_empty())
                    .filter(|r| {
                        self.tree
                            .as_ref()
                            .map(|t| {
                                t.read()
                                    .get(&r.path)
                                    .map(|n| !n.is_special())
                                    .unwrap_or(true)
                            })
                            .unwrap_or(true)
                    })
                    .map(|r| r.path.clone())
                    .collect();
                self.selected = Some(ix);
            }
            _ => {
                self.marked.clear();
                self.selected = Some(ix);
                self.anchor = Some(path);
            }
        }
        self.sync_treemap_selection(cx);
        cx.notify();
    }

    pub(super) fn sync_treemap_selection(&mut self, cx: &mut Context<Self>) {
        let sel = self.selection();
        self.treemap.update(cx, |t, cx| t.set_selection(sel, cx));
    }

    /// Every selected node path, sorted. Without a multi-selection this is
    /// just the cursor row.
    pub(super) fn selection(&self) -> Vec<NodePath> {
        if self.marked.is_empty() {
            self.selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        }
    }

    /// A file operation (refresh, delete, move) is running.
    pub(super) fn is_busy(&self) -> bool {
        self.busy.is_some() || self.op.is_some()
    }

    /// More than one item is selected.
    pub(super) fn is_multi(&self) -> bool {
        self.marked.len() > 1
    }

    /// The selection as file operations see it: items inside another selected
    /// folder are dropped, so nothing is counted or deleted twice.
    /// Pseudo-items are never targets (they have no file-system path).
    pub(super) fn targets(&self) -> Vec<Target> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let tree = tree.read();
        let mut out: Vec<Target> = Vec::new();
        // Sorted, so a folder always comes right before what is inside it.
        for path in self.selection() {
            if out.last().is_some_and(|t| path.starts_with(&t.path)) {
                continue;
            }
            let (Some(node), Some(fs)) = (tree.get(&path), tree.fs_path(&path)) else {
                continue;
            };
            if node.is_special() {
                continue;
            }
            out.push(Target {
                fs,
                is_dir: node.is_dir(),
                size: node.size,
                items: if node.is_dir() { node.items() + 1 } else { 1 },
                path,
            });
        }
        out
    }

    /// Expand all ancestors of `path`, select it (per `mode`) and scroll it
    /// into view.
    pub(super) fn reveal_in_list(
        &mut self,
        path: NodePath,
        mode: ClickMode,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        {
            let mut t = tree.write();
            for depth in 0..path.len() {
                if let Some(n) = t.get_mut(&path[..depth]) {
                    n.set_expanded(true);
                }
            }
        }
        self.rebuild_rows();
        if let Some(ix) = self.rows.iter().position(|r| r.path == path) {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
            self.apply_click(path, ix, mode, cx);
        }
    }

    pub(super) fn tree_changed(&mut self, select: Option<NodePath>, cx: &mut Context<Self>) {
        if let Some(p) = &select {
            // Make sure the new selection is visible.
            if let Some(tree) = &self.tree {
                let mut t = tree.write();
                for depth in 0..p.len() {
                    if let Some(n) = t.get_mut(&p[..depth]) {
                        n.set_expanded(true);
                    }
                }
            }
        }
        self.selected = None;
        // Node paths shifted, so a multi-selection no longer points anywhere.
        self.marked.clear();
        self.rebuild_rows();
        let sel = select.unwrap_or_default();
        self.selected = self.rows.iter().position(|r| r.path == sel).or(Some(0));
        if let Some(ix) = self.selected {
            self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        self.anchor = self.selected_tree_path();
        // The largest files may have changed, or a path may have moved.
        self.top_stale = true;
        if self.tab == ListTab::TopFiles {
            self.refresh_top_files();
        }
        let sel = self.selection();
        self.treemap.update(cx, |t, cx| {
            t.invalidate(cx);
            t.set_selection(sel, cx);
        });
        cx.notify();
    }

    /// Like [`Self::tree_changed`], but leaves the cursor and the scroll
    /// position where the user left them, as long as `previous_fs` (captured
    /// before the tree changed) is still in the scan. Used by the "stay on the
    /// current item after a delete" setting, so a delete finishing in the
    /// background does not pull the view to the deleted item's folder. Falls
    /// back to `tree_changed(select)` when the cursor itself was deleted or is
    /// hidden by the active filter.
    pub(super) fn tree_changed_keeping_selection(
        &mut self,
        previous_fs: &std::path::Path,
        select: Option<NodePath>,
        cx: &mut Context<Self>,
    ) {
        // Look the node up by file-system path: sibling rows shift when
        // something above them is removed, so the old `NodePath` is stale.
        let previous = self
            .tree
            .as_ref()
            .and_then(|tree| tree.read().find(previous_fs));
        let Some(previous) = previous else {
            self.tree_changed(select, cx);
            return;
        };
        // Node paths shifted, so a multi-selection no longer points anywhere.
        self.marked.clear();
        self.selected = None;
        self.rebuild_rows();
        self.selected = self.rows.iter().position(|r| r.path == previous);
        let Some(ix) = self.selected else {
            // The row is hidden by the active filter: use the normal path so
            // the cursor stays valid.
            self.tree_changed(select, cx);
            return;
        };
        // Deliberately no `scroll_to_item`: the viewport stays put.
        debug_assert_eq!(self.rows[ix].path, previous);
        self.anchor = self.rows.get(ix).map(|r| r.path.clone());
        // The largest files may have changed, or a path may have moved.
        self.top_stale = true;
        if self.tab == ListTab::TopFiles {
            self.refresh_top_files();
        }
        let sel = self.selection();
        self.treemap.update(cx, |t, cx| {
            t.invalidate(cx);
            t.set_selection(sel, cx);
        });
        cx.notify();
    }

    /// Move the cursor in whichever list is showing.
    fn step(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tab == ListTab::TopFiles {
            self.select_top_row(ix, cx);
        } else {
            self.select_row(ix, cx);
        }
    }

    fn step_extend(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tab == ListTab::TopFiles {
            self.select_top_row(ix, cx);
        } else {
            self.extend_row(ix, cx);
        }
    }

    fn current_row(&self) -> Option<usize> {
        match self.tab {
            ListTab::Tree => self.selected,
            ListTab::TopFiles => self.top_selected,
        }
    }

    pub(super) fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().unwrap_or(0).saturating_sub(1);
        self.step(ix, cx);
    }
    pub(super) fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().map(|i| i + 1).unwrap_or(0);
        self.step(ix, cx);
    }
    pub(super) fn extend_prev(&mut self, _: &ExtendPrev, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().unwrap_or(0).saturating_sub(1);
        self.step_extend(ix, cx);
    }
    pub(super) fn extend_next(&mut self, _: &ExtendNext, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().map(|i| i + 1).unwrap_or(0);
        self.step_extend(ix, cx);
    }
    pub(super) fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.step(0, cx);
    }
    pub(super) fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.step(usize::MAX, cx);
    }
    pub(super) fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().unwrap_or(0).saturating_sub(20);
        self.step(ix, cx);
    }
    pub(super) fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current_row().unwrap_or(0) + 20;
        self.step(ix, cx);
    }
    pub(super) fn expand_item(
        &mut self,
        _: &ExpandItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab == ListTab::TopFiles {
            let ix = self.top_selected.unwrap_or(0);
            self.reveal_top_file(ix, window, cx);
            return;
        }
        let Some(ix) = self.selected else { return };
        let expanded = self.is_expanded(ix);
        if expanded {
            // Already open: step into the first child.
            if self
                .rows
                .get(ix + 1)
                .is_some_and(|r| r.depth > self.rows[ix].depth)
            {
                self.select_row(ix + 1, cx);
            }
        } else {
            self.set_expanded(ix, Some(true), cx);
        }
    }
    pub(super) fn collapse_item(
        &mut self,
        _: &CollapseItem,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab == ListTab::TopFiles {
            return;
        }
        let Some(ix) = self.selected else { return };
        if self.is_expanded(ix) {
            self.set_expanded(ix, Some(false), cx);
        } else {
            self.go_parent(&GoParent, w, cx);
        }
    }
    pub(super) fn toggle_item(
        &mut self,
        _: &ToggleItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab == ListTab::TopFiles {
            let ix = self.top_selected.unwrap_or(0);
            self.reveal_top_file(ix, window, cx);
            return;
        }
        if let Some(ix) = self.selected {
            self.set_expanded(ix, None, cx);
        }
    }
    pub(super) fn go_parent(&mut self, _: &GoParent, _: &mut Window, cx: &mut Context<Self>) {
        if self.tab == ListTab::TopFiles {
            return;
        }
        let Some(ix) = self.selected else { return };
        let depth = self.rows[ix].depth;
        if depth == 0 {
            return;
        }
        if let Some(p) = (0..ix).rev().find(|&i| self.rows[i].depth < depth) {
            self.select_row(p, cx);
        }
    }

    // ------------------------------------------------------------------
    // Treemap zoom
    // ------------------------------------------------------------------

    /// Root the treemap at the selected folder (or the selected file's folder).
    pub(super) fn zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected_path() {
            self.treemap.update(cx, |t, cx| t.set_zoom(path, cx));
        }
    }

    pub(super) fn zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.treemap.update(cx, |t, cx| t.zoom_out(cx));
    }

    /// Whether Zoom in would change the treemap root.
    pub(super) fn can_zoom_in(&self, cx: &App) -> bool {
        let (Some(tree), Some(mut path)) = (&self.tree, self.selected_path()) else {
            return false;
        };
        let t = tree.read();
        while !path.is_empty() && !t.get(&path).is_some_and(Node::is_dir) {
            path.pop();
        }
        &path != self.treemap.read(cx).zoom()
    }

    pub(super) fn is_expanded(&self, ix: usize) -> bool {
        let (Some(tree), Some(row)) = (&self.tree, self.rows.get(ix)) else {
            return false;
        };
        tree.read().get(&row.path).is_some_and(|n| n.expanded())
    }
}

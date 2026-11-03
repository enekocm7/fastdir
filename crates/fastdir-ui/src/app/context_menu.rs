//! Right-click context menu for the list and the treemap.

use super::*;

impl FastDir {
    pub(super) fn build_context_menu(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let multi = self.is_multi();
        let is_root = self.only_root();
        let special = self.selected_is_special();
        let is_dir = !multi && !special && self.selected_fs_path().is_some_and(|s| s.2);
        let can_zoom_in = self.can_zoom_in(cx);
        let can_zoom_out = !self.treemap.read(cx).zoom().is_empty();
        let no_ops = is_root || special;
        menu.action_context(self.focus.clone())
            .menu_with_icon_and_disabled(
                self.tr("menu.open"),
                IconName::ExternalLink,
                Box::new(OpenItem),
                multi || special,
            )
            .menu_with_icon_and_disabled(
                self.reveal_label(),
                IconName::FolderOpen,
                Box::new(RevealItem),
                multi || special,
            )
            .menu_with_icon_and_disabled(self.tr("menu.copy_path"), IconName::Copy, Box::new(CopyPath), special)
            .separator()
            .menu_with_icon_and_disabled(
                self.tr("menu.zoom_in"),
                IconName::ZoomIn,
                Box::new(ZoomIn),
                !can_zoom_in,
            )
            .menu_with_icon_and_disabled(
                self.tr("menu.zoom_out"),
                IconName::ZoomOut,
                Box::new(ZoomOut),
                !can_zoom_out,
            )
            .separator()
            .menu_with_icon_and_disabled(
                self.tr("menu.rename"),
                IconName::Pencil,
                Box::new(RenameItem),
                is_root || multi || special,
            )
            .menu_with_icon_and_disabled(
                self.tr("menu.move_to"),
                IconName::FolderInput,
                Box::new(MoveItem),
                no_ops,
            )
            .menu_with_icon_and_disabled(
                self.tr("menu.refresh"),
                IconName::RefreshCw,
                Box::new(RefreshItem),
                !is_dir,
            )
            .separator()
            .menu_with_icon_and_disabled(
                self.tr("menu.export"),
                IconName::Download,
                Box::new(ExportResults),
                self.tree.is_none(),
            )
            .separator()
            .menu_with_icon_and_disabled(
                self.tr_fmt("menu.recycle", &[("trash", self.trash_name())]),
                IconName::Recycle,
                Box::new(TrashItem),
                no_ops,
            )
            .menu_with_icon_and_disabled(
                self.tr("menu.delete_perm"),
                IconName::Trash,
                Box::new(DeleteItem),
                no_ops,
            )
    }

    /// Only the scan root is selected (it cannot be deleted, moved or renamed).
    pub(super) fn only_root(&self) -> bool {
        self.marked.is_empty() && self.selected_path().is_some_and(|p| p.is_empty())
    }
}

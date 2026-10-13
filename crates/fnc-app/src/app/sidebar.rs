//! Sidebar (SPEC ch.3 §6): the folder tree of the index (a click searches that folder,
//! `"path\"`; right click is the shell menu) and the bookmarks (click runs, drag to
//! reorder). Width and the visible page are remembered.

use std::collections::HashMap;
use std::time::Duration;

use fnc_core::EntryId;
use fnc_index::IndexStore;

use super::FennecApp;
use crate::i18n::t;

/// Children shown per folder at most.
const MAX_CHILDREN: usize = 2000;

/// Sorted child folders, cached per tree generation.
#[derive(Default)]
pub(super) struct SidebarCache {
    generation: u64,
    instance: u64,
    children: HashMap<u32, Vec<(EntryId, String)>>,
}

impl SidebarCache {
    fn dirs(&mut self, s: &IndexStore, dir: EntryId) -> &[(EntryId, String)] {
        if self.generation != s.tree_generation() || self.instance != s.instance() {
            self.children.clear();
            self.generation = s.tree_generation();
            self.instance = s.instance();
        }
        self.children.entry(dir.0).or_insert_with(|| {
            let mut v: Vec<(EntryId, String)> =
                s.children(dir).filter(|&c| s.is_live(c) && s.is_dir(c)).map(|c| (c, s.name_string(c))).collect();
            v.sort_by(|a, b| s.cmp_names(a.0, b.0));
            v
        })
    }
}

/// Everything's folder search: `"C:\Users\"` (quoted, trailing separator).
pub(super) fn folder_query(path: &str) -> String {
    let sep = if path.contains('/') && !path.contains('\\') { '/' } else { '\\' };
    let p = path.trim_end_matches(['\\', '/']);
    format!("\"{p}{sep}\"")
}

impl FennecApp {
    fn folder_node(
        &mut self,
        ui: &mut egui::Ui,
        s: &IndexStore,
        id: EntryId,
        label: String,
        clicked: &mut Option<EntryId>,
        menu: &mut Option<EntryId>,
    ) {
        let kids: Vec<(EntryId, String)> = self.sidebar_cache.dirs(s, id).to_vec();
        let uid = ui.make_persistent_id(("sidebar-folder", s.instance(), id.0));
        let mut item = |ui: &mut egui::Ui| {
            let r = ui.add(egui::Label::new(label.as_str()).sense(egui::Sense::click()).truncate());
            if r.clicked() {
                *clicked = Some(id);
            }
            if r.secondary_clicked() {
                *menu = Some(id);
            }
        };
        if kids.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                item(ui);
            });
            return;
        }
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), uid, false)
            .show_header(ui, |ui| item(ui))
            .body(|ui| {
                for (c, name) in kids.iter().take(MAX_CHILDREN) {
                    self.folder_node(ui, s, *c, name.clone(), clicked, menu);
                }
                if kids.len() > MAX_CHILDREN {
                    ui.weak(format!("… {}", kids.len() - MAX_CHILDREN));
                }
            });
    }

    fn folder_tree(&mut self, ui: &mut egui::Ui) {
        let Some(snap) = self.shared.try_snapshot_for(Duration::from_millis(4)) else {
            ui.weak("…");
            ui.ctx().request_repaint_after(Duration::from_millis(50));
            return;
        };
        let mut clicked = None;
        let mut menu = None;
        let roots: Vec<EntryId> =
            snap.volumes().iter().flat_map(|v| v.roots.iter().copied()).filter(|&r| snap.is_live(r)).collect();
        let mut path = String::new();
        for r in roots {
            snap.full_path(r, &mut path);
            self.folder_node(ui, &snap, r, path.clone(), &mut clicked, &mut menu);
        }
        let to_path = |id: EntryId| {
            let mut p = String::new();
            snap.full_path(id, &mut p);
            p
        };
        let clicked = clicked.map(to_path);
        let menu = menu.map(to_path);
        drop(snap);
        if let Some(p) = clicked {
            self.set_query(folder_query(&p));
        }
        if let Some(p) = menu {
            let at = ui.ctx().input(|i| i.pointer.latest_pos()).unwrap_or_default();
            let ppp = ui.ctx().pixels_per_point();
            let screen = fnc_platform::shell::cursor_pos().unwrap_or(((at.x * ppp) as i32, (at.y * ppp) as i32));
            if let Err(e) =
                fnc_platform::fileops::context_menu(self.owner(), &[std::path::PathBuf::from(p)], screen, &[])
            {
                tracing::debug!(error = %e, "folder context menu");
            }
        }
    }

    fn bookmark_list(&mut self, ui: &mut egui::Ui) {
        if self.bookmarks.list.is_empty() {
            ui.weak(t("sidebar-no-bookmarks"));
            return;
        }
        let mut open = None;
        let mut moved: Option<(usize, usize)> = None;
        let mut folders: Vec<String> = Vec::new();
        for b in &self.bookmarks.list {
            if !folders.contains(&b.folder) {
                folders.push(b.folder.clone());
            }
        }
        for folder in folders {
            let mut rows = |ui: &mut egui::Ui| {
                for (i, b) in self.bookmarks.list.iter().enumerate().filter(|(_, b)| b.folder == folder) {
                    let id = egui::Id::new(("bookmark-drag", i));
                    let r = ui.dnd_drag_source(id, i, |ui| {
                        ui.add(egui::Label::new(b.name.as_str()).sense(egui::Sense::click()).truncate())
                    });
                    if r.inner.clicked() {
                        open = Some(i);
                    }
                    r.inner.on_hover_text(&b.search);
                    if let Some(from) = r.response.dnd_release_payload::<usize>() {
                        moved = Some((*from, i));
                    }
                }
            };
            if folder.is_empty() {
                rows(ui);
            } else {
                egui::CollapsingHeader::new(folder.as_str()).default_open(true).show(ui, rows);
            }
        }
        if let Some((from, to)) = moved
            && from != to
            && from < self.bookmarks.list.len()
        {
            let b = self.bookmarks.list.remove(from);
            let to = to.min(self.bookmarks.list.len());
            self.bookmarks.list.insert(to, b);
            self.save_lists();
        }
        if let Some(i) = open {
            self.open_bookmark(i);
        }
    }

    /// The sidebar panel (left).
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (page, key) in [("folders", "sidebar-folders"), ("bookmarks", "sidebar-bookmarks")] {
                if ui.selectable_label(self.settings.sidebar_page == page, t(key)).clicked() {
                    self.settings.sidebar_page = page.to_string();
                }
            }
        });
        ui.separator();
        egui::ScrollArea::both().id_salt("sidebar-scroll").auto_shrink([false, false]).show(ui, |ui| {
            if self.settings.sidebar_page == "bookmarks" {
                self.bookmark_list(ui);
            } else {
                self.folder_tree(ui);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_queries() {
        assert_eq!(folder_query(r"C:\Users"), r#""C:\Users\""#);
        assert_eq!(folder_query(r"C:\"), r#""C:\""#);
        assert_eq!(folder_query("/home/me/"), r#""/home/me/""#);
    }
}

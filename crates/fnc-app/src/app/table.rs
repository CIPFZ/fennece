//! The result table (SPEC ch.3 §4): configurable columns, multi-level sorting from
//! the header, multi-selection, in-place rename, context menu and dragging files out.

use std::time::Duration;

use egui_extras::{Column, TableBuilder};
use fnc_core::{EntryFlags, EntryId, FileTime};

use super::{FennecApp, Focus, Row, highlighted};
use crate::columns::{CellFormat, ColumnId, ColumnState};
use crate::commands::Command;
use crate::i18n::t;

/// Work that must happen after the table was drawn (modal shell loops).
pub(super) enum After {
    /// Right click: show the context menu for the selection.
    ContextMenu,
    /// Drag started on a selected row.
    DragOut,
}

/// What happened in the header this frame.
#[derive(Default)]
struct HeaderEvents {
    clicked: Option<(usize, bool)>,
    widths: Vec<f32>,
    rects: Vec<egui::Rect>,
    drag_from: Option<usize>,
    dropped_at: Option<egui::Pos2>,
    toggle_column: Option<ColumnId>,
    reset_columns: bool,
}

/// Mouse action on a result row.
#[derive(Clone, Copy)]
enum RowAction {
    Click { ctrl: bool, shift: bool },
    Open,
    Reveal,
    Context,
    Drag,
}

impl FennecApp {
    fn sort_arrow(&self, col: ColumnId) -> String {
        let key = col.sort();
        for (n, (c, asc)) in self.sort.keys().enumerate() {
            if c == key && self.column_owns_sort(col) {
                let arrow = if asc { "▲" } else { "▼" };
                return if n == 0 { format!(" {arrow}") } else { format!(" {arrow}{}", n + 1) };
            }
        }
        String::new()
    }

    /// Two columns can share a sort key (type/extension); only the first shows it.
    fn column_owns_sort(&self, col: ColumnId) -> bool {
        self.columns.iter().find(|c| c.id.sort() == col.sort()).is_some_and(|c| c.id == col)
    }

    fn header_cell(&self, ui: &mut egui::Ui, i: usize, ev: &mut HeaderEvents) {
        let col = self.columns[i].id;
        ev.widths[i] = ui.max_rect().width();
        let text = egui::RichText::new(format!("{}{}", col.title(), self.sort_arrow(col))).strong();
        let resp = ui.add(egui::Label::new(text).sense(egui::Sense::click_and_drag()).truncate());
        ev.rects[i] = ui.max_rect();
        if resp.clicked() {
            ev.clicked = Some((i, ui.input(|inp| inp.modifiers.ctrl)));
        }
        if resp.drag_started() {
            ev.drag_from = Some(i);
        }
        if resp.drag_stopped() {
            ev.drag_from = Some(i);
            ev.dropped_at = ui.ctx().pointer_latest_pos();
        }
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new(t("menu-columns")).weak());
            for c in ColumnId::ALL {
                let mut on = self.columns.iter().any(|s| s.id == c);
                let enabled = c != ColumnId::Name;
                if ui.add_enabled(enabled, egui::Checkbox::new(&mut on, c.title())).changed() {
                    ev.toggle_column = Some(c);
                }
            }
            ui.separator();
            if ui.button(t("menu-reset-columns")).clicked() {
                ev.reset_columns = true;
                ui.close();
            }
        });
    }

    fn apply_header(&mut self, ev: HeaderEvents) {
        for (c, w) in self.columns.iter_mut().zip(&ev.widths) {
            if *w > 10.0 {
                c.width = *w;
            }
        }
        if let Some((i, ctrl)) = ev.clicked {
            let col = self.columns[i].id;
            let key = col.sort();
            let first_asc = !col.sort_descending_first();
            if ctrl {
                self.sort.push_key(key, first_asc);
            } else if self.sort.column == key && self.sort.then.iter().all(Option::is_none) {
                self.sort.ascending = !self.sort.ascending;
            } else {
                self.sort = fnc_exec::SortSpec::by(key, first_asc, self.settings.folders_first);
            }
            self.send_search();
            // Keep typing in the search box after sorting.
            if self.focus == Focus::Search {
                self.keep_focus = true;
            }
        }
        if let (Some(from), Some(at)) = (ev.drag_from, ev.dropped_at)
            && let Some(to) = ev.rects.iter().position(|r| r.x_range().contains(at.x))
            && to != from
        {
            let c = self.columns.remove(from);
            self.columns.insert(to, c);
            self.rows.clear();
        }
        if let Some(c) = ev.toggle_column {
            if let Some(p) = self.columns.iter().position(|s| s.id == c) {
                self.columns.remove(p);
            } else {
                self.columns.push(ColumnState { id: c, width: c.default_width() });
            }
            self.rows.clear();
        }
        if ev.reset_columns {
            self.columns = crate::columns::parse_columns(&fnc_core::settings::Settings::default().columns);
            self.rows.clear();
        }
        self.sync_view_settings();
    }

    fn row(&mut self, id: EntryId, snap: Option<&fnc_index::IndexSnapshot>) -> Option<Row> {
        if let Some(r) = self.rows.get(&id) {
            return Some(r.clone());
        }
        let s = snap?;
        if !s.is_live(id) {
            return None;
        }
        let is_dir = s.is_dir(id);
        let mut cells = Vec::with_capacity(self.columns.len());
        let (date, size) = (self.settings.date_format, self.settings.size_format);
        let cols: Vec<ColumnId> = self.columns.iter().map(|c| c.id).collect();
        let names = self.type_names.clone();
        let cache = &mut self.type_cache;
        let mut type_name = |ext: &str, dir: bool| {
            cache.entry((ext.to_string(), dir)).or_insert_with(|| type_name_of(&names, ext, dir)).clone()
        };
        let mut f = CellFormat { date, size, type_name: &mut type_name, now: FileTime::now() };
        for &c in &cols {
            cells.push(crate::columns::cell(s, id, c, &mut f));
        }
        let mut name_hl = Vec::new();
        let mut path_hl = Vec::new();
        if let Some(h) = &self.highlighter {
            if let Some(i) = cols.iter().position(|&c| c == ColumnId::Name) {
                h.name_ranges(&cells[i], &mut name_hl);
            }
            if let Some(i) = cols.iter().position(|&c| matches!(c, ColumnId::Path | ColumnId::FullPath)) {
                h.path_ranges(&cells[i], &mut path_hl);
            }
        }
        let row = Row { cells, is_dir, offline: s.flags(id).contains(EntryFlags::OFFLINE), name_hl, path_hl };
        if self.rows.len() > 4096 {
            self.rows.clear();
        }
        self.rows.insert(id, row.clone());
        Some(row)
    }

    pub(super) fn results_table(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            ui.centered_and_justified(|ui| {
                let text = match self.status {
                    crate::indexer::IndexStatus::Starting | crate::indexer::IndexStatus::Building { .. } => {
                        t("empty-indexing")
                    }
                    _ => t("empty-results"),
                };
                ui.weak(text);
            });
            return;
        }
        // Selectable labels would take the clicks meant for the row (select, open,
        // context menu, drag).
        ui.style_mut().interaction.selectable_labels = false;
        // One short-lived snapshot for all visible rows; never wait more than 2 ms.
        let snap = self.shared.try_snapshot_for(Duration::from_millis(2));
        if let Some(s) = &snap
            && s.generation() != self.rows_generation
        {
            self.rows_generation = s.generation();
            self.rows.clear();
        }
        let text_h = ui.text_style_height(&egui::TextStyle::Body);
        let row_h = if self.settings.row_height > 0 {
            self.settings.row_height as f32
        } else {
            (text_h + 6.0).max(ui.spacing().interact_size.y)
        };
        let results = self.results.clone();
        let mut action: Option<(usize, RowAction)> = None;
        let ncols = self.columns.len();
        let mut builder = TableBuilder::new(ui)
            .striped(self.settings.alternate_row_color)
            .resizable(true)
            .drag_to_scroll(egui::scroll_area::DragScroll::Never)
            .sense(egui::Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (i, c) in self.columns.iter().enumerate() {
            let col = if i + 1 == ncols {
                Column::remainder().at_least(60.0).clip(true)
            } else {
                Column::initial(c.width).at_least(30.0).clip(true)
            };
            builder = builder.column(col);
        }
        if let Some(i) = self.scroll_to.take() {
            builder = builder.scroll_to_row(i, None);
        }
        let mut header =
            HeaderEvents { widths: vec![0.0; ncols], rects: vec![egui::Rect::NOTHING; ncols], ..Default::default() };
        let mut rename_done: Option<bool> = None;
        builder
            .header(text_h + 8.0, |mut h| {
                for i in 0..ncols {
                    h.col(|ui| self.header_cell(ui, i, &mut header));
                }
            })
            .body(|body| {
                body.rows(row_h, results.len(), |mut row| {
                    let i = row.index();
                    let id = results.ids[i];
                    row.set_selected(self.sel.contains(id) || (self.sel.ids.is_empty() && self.selected == Some(i)));
                    let data = self.row(id, snap.as_ref());
                    let weak = data.as_ref().is_some_and(|r| r.offline);
                    for (ci, c) in self.columns.clone().iter().enumerate() {
                        row.col(|ui| {
                            let Some(r) = &data else {
                                if ci == 0 {
                                    ui.weak("…");
                                }
                                return;
                            };
                            let text = &r.cells[ci];
                            match c.id {
                                ColumnId::Name => {
                                    if let Some(rn) = self.rename.as_mut().filter(|rn| rn.id == id) {
                                        let resp = ui
                                            .add(egui::TextEdit::singleline(&mut rn.text).desired_width(f32::INFINITY));
                                        if rn.focus {
                                            rn.focus = false;
                                            resp.request_focus();
                                        }
                                        if resp.lost_focus() {
                                            rename_done = Some(ui.input(|i| !i.key_pressed(egui::Key::Escape)));
                                        }
                                    } else {
                                        let icon = if r.is_dir { "🗀 " } else { "🗋 " };
                                        ui.label(highlighted(ui, icon, text, &r.name_hl, weak));
                                    }
                                }
                                ColumnId::Path | ColumnId::FullPath => {
                                    ui.label(highlighted(ui, "", text, &r.path_hl, weak));
                                }
                                _ if c.id.numeric() => {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(text);
                                    });
                                }
                                _ => {
                                    if weak {
                                        ui.weak(text);
                                    } else {
                                        ui.label(text);
                                    }
                                }
                            }
                        });
                    }
                    let resp = row.response();
                    let mods = resp.ctx.input(|i| i.modifiers);
                    if resp.double_clicked() {
                        action = Some((i, RowAction::Open));
                    } else if resp.middle_clicked() {
                        action = Some((i, RowAction::Reveal));
                    } else if resp.secondary_clicked() {
                        action = Some((i, RowAction::Context));
                    } else if resp.drag_started() && mods.is_none() {
                        action = Some((i, RowAction::Drag));
                    } else if resp.clicked() {
                        action = Some((i, RowAction::Click { ctrl: mods.ctrl, shift: mods.shift }));
                    }
                });
            });
        drop(snap);
        self.apply_header(header);
        match rename_done {
            Some(true) => self.commit_rename(),
            Some(false) => self.rename = None,
            None => {}
        }
        let Some((i, action)) = action else { return };
        // Clicking the list moves keyboard focus to it (Del, Ctrl+C act on files).
        if !matches!(action, RowAction::Drag) {
            self.focus = Focus::Results;
            ui.ctx().memory_mut(|m| m.surrender_focus(egui::Id::new(super::SEARCH_ID)));
        }
        let id = results.ids[i];
        match action {
            RowAction::Click { ctrl: true, .. } => {
                if self.sel.all {
                    self.sel.all = false;
                    self.sel.ids = results.ids.iter().copied().collect();
                }
                if !self.sel.ids.remove(&id) {
                    self.sel.ids.insert(id);
                }
                self.sel.anchor = Some(i);
                self.selected = Some(i);
                self.selected_id = Some(id);
            }
            RowAction::Click { shift: true, .. } => self.select_range(i),
            RowAction::Click { .. } => {
                self.select_only(i);
                self.scroll_to = None;
                if self.settings.single_click_open {
                    self.pending.push(Command::Open);
                }
            }
            RowAction::Open => {
                self.select_only(i);
                self.scroll_to = None;
                self.pending.push(Command::Open);
            }
            RowAction::Reveal => {
                self.select_only(i);
                self.scroll_to = None;
                self.pending.push(Command::OpenPath);
            }
            RowAction::Context => {
                if !self.sel.contains(id) {
                    self.select_only(i);
                    self.scroll_to = None;
                }
                self.after.push(After::ContextMenu);
            }
            RowAction::Drag => {
                if !self.sel.contains(id) {
                    self.select_only(i);
                    self.scroll_to = None;
                }
                self.after.push(After::DragOut);
            }
        }
    }

    /// Runs deferred list actions after the frame was laid out.
    pub(super) fn after_table(&mut self, ctx: &egui::Context) {
        for a in std::mem::take(&mut self.after) {
            match a {
                After::ContextMenu => self.context_menu(ctx),
                After::DragOut => {
                    let paths: Vec<std::path::PathBuf> = self.target_paths().into_iter().map(|(_, p)| p).collect();
                    if paths.is_empty() {
                        continue;
                    }
                    match fnc_platform::fileops::drag_files(&paths) {
                        Ok(r) => tracing::debug!(?r, files = paths.len(), "drag finished"),
                        // Other platforms: dragging out is not available.
                        Err(e) => tracing::debug!(error = %e, "drag out unavailable"),
                    }
                }
            }
        }
    }
}

fn type_name_of(names: &fnc_exec::TypeNames, ext: &str, is_dir: bool) -> String {
    let shell = if is_dir { fnc_platform::shell::type_name("", true) } else { names.lookup(ext) };
    shell.unwrap_or_else(|| {
        if is_dir {
            t("type-folder")
        } else if ext.is_empty() {
            t("type-file")
        } else {
            crate::i18n::t_args("type-ext-file", &[("ext", ext.to_uppercase().into())])
        }
    })
}

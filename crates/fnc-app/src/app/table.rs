//! The result table (SPEC ch.3 §4): configurable columns, multi-level sorting from
//! the header, multi-selection, in-place rename, context menu and dragging files out.

use std::path::PathBuf;
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

/// `true` if `results` come from a duplicate search (`dupe:` …).
fn has_dupe_groups(results: &fnc_exec::ResultSet) -> bool {
    results.extra.as_ref().is_some_and(|e| !e.dupe_groups.is_empty())
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
        let Some(key) = col.sort() else { return String::new() };
        for (n, (c, asc)) in self.v.sort.sort_keys().enumerate() {
            if c == key && self.column_owns_sort(col) {
                let arrow = if asc { "▲" } else { "▼" };
                return if n == 0 { format!(" {arrow}") } else { format!(" {arrow}{}", n + 1) };
            }
        }
        String::new()
    }

    /// Two columns can share a sort key (type/extension); only the first shows it.
    fn column_owns_sort(&self, col: ColumnId) -> bool {
        self.display_columns().iter().find(|c| c.id.sort() == col.sort()).is_some_and(|c| c.id == col)
    }

    /// Columns drawn for the current results: the configured ones, plus the duplicate
    /// group column (after the first column, so it is never pushed out of view) while a
    /// `dupe:` search is shown (not saved).
    fn display_columns(&self) -> Vec<ColumnState> {
        let mut cols = self.columns.clone();
        if has_dupe_groups(&self.v.results) && !cols.iter().any(|c| c.id == ColumnId::DupeGroup) {
            let at = cols.len().min(1);
            cols.insert(at, ColumnState { id: ColumnId::DupeGroup, width: ColumnId::DupeGroup.default_width() });
        }
        // Relevance appears while semantic terms take part (SPEC ch.5 §3.3).
        if self.v.results.has_relevance() && !cols.iter().any(|c| c.id == ColumnId::Relevance) {
            let at = cols.len().min(1);
            cols.insert(at, ColumnState { id: ColumnId::Relevance, width: ColumnId::Relevance.default_width() });
        }
        cols
    }

    fn header_cell(&self, ui: &mut egui::Ui, cols: &[ColumnState], i: usize, ev: &mut HeaderEvents) {
        let col = cols[i].id;
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
            for c in ColumnId::customs() {
                let mut on = self.columns.iter().any(|s| s.id == c);
                if ui.checkbox(&mut on, c.title()).changed() {
                    ev.toggle_column = Some(c);
                }
            }
            // Property columns (SPEC ch.3 §4.3), indexed ones first.
            // About 300 entries: a filter box (title or search name) at the top.
            ui.menu_button(t("menu-property-columns"), |ui| {
                let filter_id = ui.make_persistent_id("property-column-filter");
                let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id)).unwrap_or_default();
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut filter).hint_text(t("menu-property-filter")).desired_width(220.0),
                );
                if edit.changed() {
                    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    let indexed = self.stores.config().props.stored();
                    let all = crate::columns::filter_properties(ColumnId::properties(), &filter);
                    let (first, rest): (Vec<ColumnId>, Vec<ColumnId>) = all.into_iter().partition(|c| match c {
                        ColumnId::Property(p) => indexed.contains(p),
                        _ => false,
                    });
                    for (k, c) in first.iter().chain(&rest).enumerate() {
                        if k == first.len() && k > 0 {
                            ui.separator();
                        }
                        let mut on = self.columns.iter().any(|s| s.id == *c);
                        if ui.checkbox(&mut on, c.title()).changed() {
                            ev.toggle_column = Some(*c);
                        }
                    }
                });
            });
            ui.separator();
            if ui.button(t("menu-reset-columns")).clicked() {
                ev.reset_columns = true;
                ui.close();
            }
        });
    }

    fn apply_header(&mut self, ev: HeaderEvents, cols: &[ColumnState]) {
        // Widths by column (the display list may contain the virtual dupe column).
        for (d, w) in cols.iter().zip(&ev.widths) {
            if *w > 10.0
                && let Some(c) = self.columns.iter_mut().find(|c| c.id == d.id)
            {
                c.width = *w;
            }
        }
        if let Some((i, ctrl)) = ev.clicked
            && let Some(col) = cols.get(i).map(|c| c.id)
            && let Some(key) = col.sort()
        {
            let first_asc = !col.sort_descending_first();
            let single = self.v.sort.then.iter().all(Option::is_none);
            let primary = self.v.sort.sort_keys().next().map(|(k, _)| k);
            if ctrl {
                self.v.sort.push_sort_key(key, first_asc);
            } else if primary == Some(key) && single {
                self.v.sort.ascending = !self.v.sort.ascending;
            } else {
                self.v.sort = fnc_exec::SortSpec::by_key(key, first_asc, self.settings.folders_first);
            }
            self.send_search();
            // Keep typing in the search box after sorting.
            if self.v.focus == Focus::Search {
                self.v.keep_focus = true;
            }
        }
        let real = |i: usize| cols.get(i).and_then(|d| self.columns.iter().position(|c| c.id == d.id));
        if let (Some(from), Some(at)) = (ev.drag_from, ev.dropped_at)
            && let Some(to) = ev.rects.iter().position(|r| r.x_range().contains(at.x))
            && let (Some(from), Some(to)) = (real(from), real(to))
            && to != from
        {
            let c = self.columns.remove(from);
            self.columns.insert(to, c);
            self.v.rows.clear();
        }
        if let Some(c) = ev.toggle_column {
            if let Some(p) = self.columns.iter().position(|s| s.id == c) {
                self.columns.remove(p);
            } else {
                self.columns.push(ColumnState { id: c, width: c.default_width() });
            }
            self.v.rows.clear();
        }
        if ev.reset_columns {
            self.columns = crate::columns::parse_columns(&fnc_core::settings::Settings::default().columns);
            self.v.rows.clear();
        }
        self.sync_view_settings();
    }

    fn row(
        &mut self,
        id: EntryId,
        snap: Option<&fnc_index::IndexSnapshot>,
        props: Option<&fnc_props::PropertySnapshot>,
        cols: &[ColumnId],
    ) -> Option<Row> {
        if let Some(r) = self.v.rows.get(&id) {
            return Some(r.clone());
        }
        let s = snap?;
        if !s.is_live(id) {
            return None;
        }
        let is_dir = s.is_dir(id);
        let mut cells = Vec::with_capacity(cols.len());
        let (date, size) = (self.settings.date_format, self.settings.size_format);
        let names = self.type_names.clone();
        let cache = &mut self.type_cache;
        let mut type_name = |ext: &str, dir: bool| {
            cache.entry((ext.to_string(), dir)).or_insert_with(|| type_name_of(&names, ext, dir)).clone()
        };
        let results = self.v.results.clone();
        let mut f = CellFormat {
            date,
            size,
            type_name: &mut type_name,
            now: FileTime::now(),
            props: props.map(|p| &**p),
            results: Some(&results),
        };
        for &c in cols {
            cells.push(crate::columns::cell(s, id, c, &mut f));
        }
        let mut name_hl = Vec::new();
        let mut path_hl = Vec::new();
        if let Some(h) = &self.v.highlighter {
            if let Some(i) = cols.iter().position(|&c| c == ColumnId::Name) {
                h.name_ranges(&cells[i], &mut name_hl);
            }
            if let Some(i) = cols.iter().position(|&c| matches!(c, ColumnId::Path | ColumnId::FullPath)) {
                h.path_ranges(&cells[i], &mut path_hl);
            }
        }
        let mut full = String::new();
        s.full_path(id, &mut full);
        let row = Row {
            cells,
            path: PathBuf::from(full),
            is_dir,
            offline: s.flags(id).contains(EntryFlags::OFFLINE),
            name_hl,
            path_hl,
        };
        if self.v.rows.len() > 4096 {
            self.v.rows.clear();
        }
        self.v.rows.insert(id, row.clone());
        Some(row)
    }

    pub(super) fn results_table(&mut self, ui: &mut egui::Ui) {
        if self.v.results.is_empty() {
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
        // The indexer held the lock: draw what is cached and come back for the rest.
        if snap.is_none() {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
        let cols = self.display_columns();
        let col_ids: Vec<ColumnId> = cols.iter().map(|c| c.id).collect();
        let wants_props = col_ids.iter().any(|c| matches!(c, ColumnId::Property(_)));
        // Property cells: the store snapshot is taken after the index one (lock order).
        let props = if wants_props && snap.is_some() {
            self.stores.props.try_snapshot_for(Duration::from_millis(1))
        } else {
            None
        };
        if wants_props && props.is_none() {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
        if let Some(s) = &snap {
            let generation = s.generation() ^ self.stores.generation().rotate_left(17);
            if generation != self.v.rows_generation {
                self.v.rows_generation = generation;
                self.v.rows.clear();
            }
        }
        let text_h = ui.text_style_height(&egui::TextStyle::Body);
        let row_h = if self.settings.row_height > 0 {
            self.settings.row_height as f32
        } else {
            (text_h + 6.0).max(ui.spacing().interact_size.y)
        };
        let results = self.v.results.clone();
        let mut action: Option<(usize, RowAction)> = None;
        let ncols = cols.len();
        let mut builder = TableBuilder::new(ui)
            .striped(self.settings.alternate_row_color)
            .resizable(true)
            .drag_to_scroll(egui::scroll_area::DragScroll::Never)
            .sense(egui::Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (i, c) in cols.iter().enumerate() {
            let col = if i + 1 == ncols {
                Column::remainder().at_least(60.0).clip(true)
            } else {
                Column::initial(c.width).at_least(30.0).clip(true)
            };
            builder = builder.column(col);
        }
        if let Some(i) = self.v.scroll_to.take() {
            builder = builder.scroll_to_row(i, None);
        }
        let mut header =
            HeaderEvents { widths: vec![0.0; ncols], rects: vec![egui::Rect::NOTHING; ncols], ..Default::default() };
        let mut rename_done: Option<bool> = None;
        builder
            .header(text_h + 8.0, |mut h| {
                for i in 0..ncols {
                    h.col(|ui| self.header_cell(ui, &cols, i, &mut header));
                }
            })
            .body(|body| {
                body.rows(row_h, results.len(), |mut row| {
                    let i = row.index();
                    let id = results.ids[i];
                    row.set_selected(
                        self.v.sel.contains(id) || (self.v.sel.ids.is_empty() && self.v.selected == Some(i)),
                    );
                    // Without the property snapshot this frame, don't cache incomplete rows.
                    let data = if wants_props && props.is_none() && !self.v.rows.contains_key(&id) {
                        None
                    } else {
                        self.row(id, snap.as_ref(), props.as_ref(), &col_ids)
                    };
                    let weak = data.as_ref().is_some_and(|r| r.offline);
                    for (ci, c) in cols.iter().enumerate() {
                        row.col(|ui| {
                            let Some(r) = &data else {
                                if ci == 0 {
                                    ui.weak("…");
                                }
                                return;
                            };
                            let Some(text) = r.cells.get(ci) else { return };
                            match c.id {
                                ColumnId::Name => {
                                    if let Some(rn) = self.v.rename.as_mut().filter(|rn| rn.id == id) {
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
                                        let px = (16.0 * ui.ctx().pixels_per_point()).round() as u32;
                                        let tex = self.settings.show_icons.then(|| {
                                            self.thumbs.get(crate::thumbs::icon_key(&r.path, r.is_dir, px), &r.path)
                                        });
                                        match tex.flatten() {
                                            Some(tex) => {
                                                ui.add(
                                                    egui::Image::new(&tex).fit_to_exact_size(egui::vec2(16.0, 16.0)),
                                                );
                                                ui.label(highlighted(ui, "", text, &r.name_hl, weak));
                                            }
                                            None => {
                                                let icon = if r.is_dir { "🗀 " } else { "🗋 " };
                                                ui.label(highlighted(ui, icon, text, &r.name_hl, weak));
                                            }
                                        }
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
        drop(props);
        drop(snap);
        self.apply_header(header, &cols);
        match rename_done {
            Some(true) => self.commit_rename(),
            Some(false) => self.v.rename = None,
            None => {}
        }
        let Some((i, action)) = action else { return };
        // Clicking the list moves keyboard focus to it (Del, Ctrl+C act on files).
        if !matches!(action, RowAction::Drag) {
            self.v.focus = Focus::Results;
            let sid = self.search_id();
            ui.ctx().memory_mut(|m| m.surrender_focus(sid));
        }
        let id = results.ids[i];
        match action {
            RowAction::Click { ctrl: true, .. } => {
                if self.v.sel.all {
                    self.v.sel.all = false;
                    self.v.sel.ids = results.ids.iter().copied().collect();
                }
                if !self.v.sel.ids.remove(&id) {
                    self.v.sel.ids.insert(id);
                }
                self.v.sel.anchor = Some(i);
                self.v.selected = Some(i);
                self.v.selected_id = Some(id);
            }
            RowAction::Click { shift: true, .. } => self.select_range(i),
            RowAction::Click { .. } => {
                self.select_only(i);
                self.v.scroll_to = None;
                if self.settings.single_click_open {
                    self.pending.push(Command::Open);
                }
            }
            RowAction::Open => {
                self.select_only(i);
                self.v.scroll_to = None;
                self.pending.push(Command::Open);
            }
            RowAction::Reveal => {
                self.select_only(i);
                self.v.scroll_to = None;
                self.pending.push(Command::OpenPath);
            }
            RowAction::Context => {
                if !self.v.sel.contains(id) {
                    self.select_only(i);
                    self.v.scroll_to = None;
                }
                self.after.push(After::ContextMenu);
            }
            RowAction::Drag => {
                if !self.v.sel.contains(id) {
                    self.select_only(i);
                    self.v.scroll_to = None;
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

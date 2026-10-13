//! Thumbnail views (SPEC ch.3 §4.1, Ctrl+Shift+2..4): a virtualised grid of thumbnails
//! with the same selection, opening, context menu and drag behaviour as the table.

use std::time::Duration;

use fnc_core::EntryFlags;
use fnc_core::settings::ViewMode;

use super::table::After;
use super::{FennecApp, Focus};
use crate::commands::Command;
use crate::thumbs::ThumbKey;

/// Thumbnail edge length in points for a view mode.
pub(super) fn thumb_size(v: ViewMode) -> f32 {
    match v {
        ViewMode::Details => 16.0,
        ViewMode::Medium => 96.0,
        ViewMode::Large => 160.0,
        ViewMode::ExtraLarge => 256.0,
    }
}

impl FennecApp {
    /// Columns of the grid at the current width (keyboard navigation moves by rows).
    pub(super) fn grid_columns(&self, width: f32) -> usize {
        let cell = thumb_size(self.settings.view_mode) + 24.0;
        ((width / cell).floor() as usize).max(1)
    }

    pub(super) fn results_grid(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.weak(crate::i18n::t("empty-results"));
            });
            return;
        }
        ui.style_mut().interaction.selectable_labels = false;
        let size = thumb_size(self.settings.view_mode);
        let cell = egui::vec2(size + 24.0, size + 2.0 * ui.text_style_height(&egui::TextStyle::Body) + 16.0);
        let cols = self.grid_columns(ui.available_width());
        self.grid_cols = cols;
        let rows = self.results.len().div_ceil(cols);
        let results = self.results.clone();
        let snap = self.shared.try_snapshot_for(Duration::from_millis(2));
        let px = (size * ui.ctx().pixels_per_point()).round() as u32;
        let mut action: Option<(usize, egui::Response)> = None;
        let mut area = egui::ScrollArea::vertical().id_salt("fennec-grid").auto_shrink([false, false]);
        if let Some(i) = self.scroll_to.take() {
            area = area.vertical_scroll_offset((i / cols) as f32 * cell.y - cell.y);
        }
        area.show_rows(ui, cell.y, rows, |ui, range| {
            for r in range {
                ui.horizontal(|ui| {
                    for c in 0..cols {
                        let i = r * cols + c;
                        let Some(&id) = results.ids.get(i) else { break };
                        let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click_and_drag());
                        let selected = self.sel.contains(id) || (self.sel.ids.is_empty() && self.selected == Some(i));
                        if selected {
                            ui.painter().rect_filled(rect.shrink(2.0), 4.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect.shrink(2.0), 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        let Some(s) = snap.as_ref().filter(|s| s.is_live(id)) else { continue };
                        let mut path = String::new();
                        s.full_path(id, &mut path);
                        let path = std::path::PathBuf::from(path);
                        let stamp = (s.date_modified(id).0 as u64) ^ s.size(id).unwrap_or(0).rotate_left(17);
                        let img_rect =
                            egui::Rect::from_min_size(rect.min + egui::vec2(12.0, 6.0), egui::vec2(size, size));
                        let key = if s.is_dir(id) {
                            crate::thumbs::icon_key(&path, true, px.min(256))
                        } else {
                            ThumbKey { path: path.clone(), size: px.next_power_of_two().min(1024), icon: false, stamp }
                        };
                        match self.thumbs.get(key, &path) {
                            Some(tex) => {
                                let t = tex.size_vec2();
                                let scale = (size / t.x).min(size / t.y);
                                let fit = egui::Rect::from_center_size(img_rect.center(), t * scale);
                                ui.painter().image(
                                    tex.id(),
                                    fit,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            }
                            None => {
                                let glyph = if s.is_dir(id) { "🗀" } else { "🗋" };
                                ui.painter().text(
                                    img_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    glyph,
                                    egui::FontId::proportional(size * 0.5),
                                    ui.visuals().weak_text_color(),
                                );
                            }
                        }
                        let name = s.name_string(id);
                        let weak = s.flags(id).contains(EntryFlags::OFFLINE);
                        let color = if weak { ui.visuals().weak_text_color() } else { ui.visuals().text_color() };
                        let galley =
                            ui.painter().layout(name, egui::TextStyle::Body.resolve(ui.style()), color, cell.x - 8.0);
                        let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, img_rect.max.y + 4.0);
                        ui.painter().with_clip_rect(rect).galley(pos, galley, color);
                        if resp.clicked()
                            || resp.double_clicked()
                            || resp.secondary_clicked()
                            || resp.drag_started()
                            || resp.middle_clicked()
                        {
                            action = Some((i, resp));
                        }
                    }
                });
            }
        });
        drop(snap);
        let Some((i, resp)) = action else { return };
        let id = results.ids[i];
        let mods = ui.input(|inp| inp.modifiers);
        if !resp.drag_started() {
            self.focus = Focus::Results;
            ui.ctx().memory_mut(|m| m.surrender_focus(egui::Id::new(super::SEARCH_ID)));
        }
        if resp.double_clicked() {
            self.select_only(i);
            self.scroll_to = None;
            self.pending.push(Command::Open);
        } else if resp.middle_clicked() {
            self.select_only(i);
            self.scroll_to = None;
            self.pending.push(Command::OpenPath);
        } else if resp.secondary_clicked() {
            if !self.sel.contains(id) {
                self.select_only(i);
                self.scroll_to = None;
            }
            self.after.push(After::ContextMenu);
        } else if resp.drag_started() {
            if !self.sel.contains(id) {
                self.select_only(i);
                self.scroll_to = None;
            }
            self.after.push(After::DragOut);
        } else if mods.ctrl {
            if self.sel.all {
                self.sel.all = false;
                self.sel.ids = results.ids.iter().copied().collect();
            }
            if !self.sel.ids.remove(&id) {
                self.sel.ids.insert(id);
            }
            self.selected = Some(i);
            self.selected_id = Some(id);
        } else if mods.shift {
            self.select_range(i);
        } else {
            self.select_only(i);
            self.scroll_to = None;
        }
    }
}

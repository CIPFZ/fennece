//! The Options window (SPEC ch.3 §12): a separate viewport with a page tree, generic
//! editors driven by the settings metadata, a key binding page, and the Advanced page
//! listing every `key = value` with a "show settings containing" filter (UI-08).

use fnc_core::settings::{SettingDescriptor, SettingKind, Settings, SettingsMeta};

use super::FennecApp;
use crate::commands::{Chord, Command, KeyCtx, Keymap};
use crate::i18n::t;

/// Page tree: `(page id, depth)`; titles are `options-page-<id>`.
const PAGES: &[(&str, u8)] = &[
    ("general", 0),
    ("ui", 1),
    ("search", 1),
    ("results", 1),
    ("view", 1),
    ("fonts_colors", 1),
    ("history", 1),
    ("keyboard", 1),
    ("context_menu", 1),
    ("open_commands", 1),
    ("index", 0),
    ("ntfs", 1),
    ("folders", 1),
    ("filelists", 1),
    ("exclude", 1),
    ("properties", 1),
    ("content", 1),
    ("semantic", 1),
    ("index_journal", 1),
    ("server", 0),
    ("advanced", 0),
];

/// Open Options window.
pub(super) struct OptionsState {
    draft: Settings,
    keymap: Keymap,
    page: &'static str,
    filter: String,
    /// Waiting for a key press to bind `(context, command)`.
    capture: Option<(KeyCtx, Command)>,
    /// Text being edited per key (so invalid input can be corrected).
    edits: std::collections::HashMap<&'static str, String>,
    error: Option<String>,
}

/// What the window asked for this frame.
enum Outcome {
    None,
    Apply,
    Ok,
    Cancel,
}

impl OptionsState {
    pub(super) fn new(s: &Settings, keymap: &Keymap) -> Self {
        Self {
            draft: s.clone(),
            keymap: keymap.clone(),
            page: "general",
            filter: String::new(),
            capture: None,
            edits: Default::default(),
            error: None,
        }
    }

    /// Opens on the Advanced page with `filter` (tests, `/options advanced`).
    #[cfg(test)]
    pub(super) fn advanced(mut self, filter: &str) -> Self {
        self.page = "advanced";
        self.filter = filter.to_string();
        self
    }

    fn editor(&mut self, ui: &mut egui::Ui, d: &'static SettingDescriptor) {
        let current = self.draft.get(d.key).unwrap_or_default();
        match d.kind {
            SettingKind::Bool => {
                let mut on = current == "true";
                if ui.checkbox(&mut on, "").changed() {
                    let _ = self.draft.set(d.key, if on { "true" } else { "false" });
                }
            }
            SettingKind::Choice(choices) => {
                let mut sel = current.clone();
                egui::ComboBox::from_id_salt(d.key).selected_text(&sel).show_ui(ui, |ui| {
                    for &c in choices {
                        ui.selectable_value(&mut sel, c.to_string(), c);
                    }
                });
                if sel != current {
                    let _ = self.draft.set(d.key, &sel);
                }
            }
            SettingKind::Int | SettingKind::Float | SettingKind::Text | SettingKind::List => {
                let text = self.edits.entry(d.key).or_insert_with(|| current.clone());
                let width = if matches!(d.kind, SettingKind::Int | SettingKind::Float) { 90.0 } else { 320.0 };
                let resp = ui.add(egui::TextEdit::singleline(text).desired_width(width));
                if resp.changed() {
                    match self.draft.set(d.key, text) {
                        Ok(()) => self.error = None,
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                if resp.lost_focus() {
                    // Show the normalised value.
                    *text = self.draft.get(d.key).unwrap_or_default();
                }
            }
        }
    }

    fn settings_grid(&mut self, ui: &mut egui::Ui, descs: Vec<&'static SettingDescriptor>, show_keys: bool) {
        if descs.is_empty() {
            ui.weak(t("options-empty-page"));
            return;
        }
        egui::Grid::new(("options-grid", self.page)).num_columns(3).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
            for d in descs {
                let label = if show_keys { d.key.to_string() } else { d.doc.to_string() };
                ui.label(label).on_hover_text(format!("{}\n{} = {}", d.doc, d.key, d.default_src));
                self.editor(ui, d);
                let is_default = {
                    let mut def = Settings::default();
                    def.reset(d.key);
                    def.get(d.key) == self.draft.get(d.key)
                };
                if ui.add_enabled(!is_default, egui::Button::new(t("options-reset")).small()).clicked() {
                    self.draft.reset(d.key);
                    self.edits.remove(d.key);
                }
                ui.end_row();
            }
        });
    }

    fn keyboard_page(&mut self, ui: &mut egui::Ui) {
        ui.label(t("options-keyboard-help"));
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t("options-filter-hint")));
        ui.separator();
        // Capture the next key press for the binding being edited.
        if let Some((ctx, cmd)) = self.capture {
            let pressed = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some(Chord::from_event(*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some(ch) = pressed {
                if ch.key == egui::Key::Escape {
                    self.capture = None;
                } else {
                    self.keymap.bind(ctx, cmd, Some(ch));
                    self.capture = None;
                }
            }
        }
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("keys").num_columns(4).striped(true).show(ui, |ui| {
                ui.strong(t("options-command"));
                for ctx in [KeyCtx::Search, KeyCtx::Results, KeyCtx::Global] {
                    ui.strong(ctx.label());
                }
                ui.end_row();
                for &cmd in Command::ALL {
                    let label = cmd.label();
                    if !filter.is_empty() && !label.to_lowercase().contains(&filter) && !cmd.id().contains(&filter) {
                        continue;
                    }
                    ui.label(label).on_hover_text(cmd.id());
                    for ctx in [KeyCtx::Search, KeyCtx::Results, KeyCtx::Global] {
                        let chords = self.keymap.chords(ctx, cmd);
                        let text = if self.capture == Some((ctx, cmd)) {
                            t("options-press-key")
                        } else {
                            chords.iter().map(Chord::display).collect::<Vec<_>>().join(", ")
                        };
                        ui.horizontal(|ui| {
                            if ui.button(if text.is_empty() { "—".to_string() } else { text }).clicked() {
                                self.capture = Some((ctx, cmd));
                            }
                            if !chords.is_empty() && ui.small_button("✕").clicked() {
                                self.keymap.bind(ctx, cmd, None);
                            }
                        });
                    }
                    ui.end_row();
                }
            });
        });
        if ui.button(t("options-reset-keys")).clicked() {
            self.keymap = Keymap::default();
        }
    }

    fn page_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(t(&format!("options-page-{}", self.page)));
        ui.add_space(6.0);
        match self.page {
            "keyboard" => self.keyboard_page(ui),
            "advanced" => {
                ui.horizontal(|ui| {
                    ui.label(t("options-show-containing"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter).id_salt("advanced-filter").desired_width(240.0),
                    );
                });
                ui.separator();
                let descs = Settings::search(&self.filter);
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_grid(ui, descs, true));
            }
            "fonts_colors" => {
                // Theme and scale live on the UI page; colours follow the theme.
                let descs: Vec<_> = Settings::descriptors()
                    .iter()
                    .filter(|d| matches!(d.key, "theme" | "ui_scale" | "row_height"))
                    .collect();
                self.settings_grid(ui, descs, false);
            }
            page => {
                ui.weak(t(&format!("options-desc-{page}")));
                ui.add_space(4.0);
                let descs: Vec<_> = Settings::descriptors().iter().filter(|d| d.page == page).collect();
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_grid(ui, descs, false));
            }
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let mut outcome = Outcome::None;
        egui::Panel::bottom("options-buttons").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("options-apply")).clicked() {
                        outcome = Outcome::Apply;
                    }
                    if ui.button(t("options-cancel")).clicked() {
                        outcome = Outcome::Cancel;
                    }
                    if ui.button(t("options-ok")).clicked() {
                        outcome = Outcome::Ok;
                    }
                });
            });
            ui.add_space(4.0);
        });
        egui::Panel::left("options-pages").resizable(false).exact_size(190.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for &(id, depth) in PAGES {
                    ui.horizontal(|ui| {
                        ui.add_space(f32::from(depth) * 14.0);
                        if ui.selectable_label(self.page == id, t(&format!("options-page-{id}"))).clicked() {
                            if self.page != id {
                                self.filter.clear();
                            }
                            self.page = id;
                        }
                    });
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.page_ui(ui));
        outcome
    }

    /// The edited settings including key bindings.
    fn result(&self) -> Settings {
        let mut s = self.draft.clone();
        s.key_bindings = self.keymap.to_overrides();
        s
    }
}

impl FennecApp {
    /// Draws the Options viewport if open and applies its outcome.
    pub(super) fn options_viewport(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.options_window.take() else { return };
        let id = egui::ViewportId::from_hash_of("fennec-options");
        let builder = egui::ViewportBuilder::default()
            .with_title(t("options-title"))
            .with_inner_size([860.0, 580.0])
            .with_min_inner_size([600.0, 400.0]);
        let mut outcome = Outcome::None;
        ctx.show_viewport_immediate(id, builder, |ui, class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                outcome = Outcome::Cancel;
            }
            // Backends without multiple viewports (tests) embed it in a window.
            let _ = class;
            let o = state.show(ui);
            if !matches!(o, Outcome::None) {
                outcome = o;
            }
        });
        match outcome {
            Outcome::None => self.options_window = Some(state),
            Outcome::Apply => {
                self.apply_settings(ctx, state.result());
                self.options_window = Some(state);
            }
            Outcome::Ok => self.apply_settings(ctx, state.result()),
            Outcome::Cancel => {}
        }
    }
}

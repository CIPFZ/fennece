//! Dialogs of the main window: new folder, run count, search builder (SPEC ch.3 §2),
//! bookmark and filter editors and managers (§3, ch.2 §7), export, the fallback
//! context menu, help and about.

use std::path::PathBuf;

use super::FennecApp;
use crate::commands::Command;
use crate::filters::{Bookmark, Filter};
use crate::i18n::{t, t_args};

/// An open dialog.
pub(super) enum Dialog {
    NewFolder { parent: PathBuf, name: String },
    SetRunCount { text: String },
    Builder(Box<Builder>),
    EditBookmark { index: Option<usize>, bookmark: Bookmark },
    Bookmarks { selected: Option<usize> },
    EditFilter { index: Option<usize>, filter: Filter },
    Filters { selected: Option<usize> },
    Export { path: String },
    ConfirmDelete { paths: Vec<PathBuf> },
    ContextMenu { at: egui::Pos2 },
    Help,
    About,
}

/// Search builder form ("Advanced search").
#[derive(Default, Clone)]
pub(super) struct Builder {
    pub all_words: String,
    pub exact_phrase: String,
    pub any_words: String,
    pub none_words: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub folder: String,
    pub subfolders: bool,
    pub exts: String,
    /// 0 = files and folders, 1 = files, 2 = folders.
    pub kind: u8,
    pub size_min: String,
    pub size_max: String,
    pub date_from: String,
    pub date_to: String,
    pub hidden: bool,
    pub system: bool,
    pub readonly: bool,
}

fn quote_if_needed(s: &str) -> String {
    if s.contains([' ', '|', '<', '>', '!']) { format!("\"{s}\"") } else { s.to_string() }
}

impl Builder {
    /// The Everything search this form describes.
    pub fn query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mods =
            format!("{}{}", if self.match_case { "case:" } else { "" }, if self.whole_word { "ww:" } else { "" });
        for w in self.all_words.split_whitespace() {
            parts.push(format!("{mods}{}", quote_if_needed(w)));
        }
        if !self.exact_phrase.trim().is_empty() {
            parts.push(format!("{mods}\"{}\"", self.exact_phrase.trim().replace('"', "")));
        }
        let any: Vec<String> =
            self.any_words.split_whitespace().map(|w| format!("{mods}{}", quote_if_needed(w))).collect();
        match any.len() {
            0 => {}
            1 => parts.push(any[0].clone()),
            _ => parts.push(format!("<{}>", any.join("|"))),
        }
        for w in self.none_words.split_whitespace() {
            parts.push(format!("!{mods}{}", quote_if_needed(w)));
        }
        let folder = self.folder.trim().trim_end_matches(['\\', '/']);
        if !folder.is_empty() {
            if self.subfolders {
                parts.push(format!("\"{folder}\\\""));
            } else {
                parts.push(format!("parent:\"{folder}\""));
            }
        }
        let exts: Vec<&str> = self
            .exts
            .split([';', ',', ' '])
            .map(|e| e.trim().trim_start_matches("*.").trim_start_matches('.'))
            .filter(|e| !e.is_empty())
            .collect();
        if !exts.is_empty() {
            parts.push(format!("ext:{}", exts.join(";")));
        }
        match self.kind {
            1 => parts.push("file:".into()),
            2 => parts.push("folder:".into()),
            _ => {}
        }
        let range = |func: &str, a: &str, b: &str| match (a.trim(), b.trim()) {
            ("", "") => None,
            (a, "") => Some(format!("{func}:>={a}")),
            ("", b) => Some(format!("{func}:<={b}")),
            (a, b) => Some(format!("{func}:{a}..{b}")),
        };
        parts.extend(range("size", &self.size_min, &self.size_max));
        parts.extend(range("dm", &self.date_from, &self.date_to));
        let mut attrs = String::new();
        for (on, c) in [(self.hidden, 'H'), (self.system, 'S'), (self.readonly, 'R')] {
            if on {
                attrs.push(c);
            }
        }
        if !attrs.is_empty() {
            parts.push(format!("attrib:{attrs}"));
        }
        parts.join(" ")
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("builder").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            let row = |ui: &mut egui::Ui, key: &str, v: &mut String| {
                ui.label(t(key));
                ui.add(egui::TextEdit::singleline(v).desired_width(320.0));
                ui.end_row();
            };
            row(ui, "builder-all-words", &mut self.all_words);
            row(ui, "builder-exact-phrase", &mut self.exact_phrase);
            row(ui, "builder-any-words", &mut self.any_words);
            row(ui, "builder-none-words", &mut self.none_words);
            ui.label("");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.match_case, t("builder-match-case"));
                ui.checkbox(&mut self.whole_word, t("builder-whole-word"));
            });
            ui.end_row();
            row(ui, "builder-folder", &mut self.folder);
            ui.label("");
            ui.checkbox(&mut self.subfolders, t("builder-subfolders"));
            ui.end_row();
            row(ui, "builder-extensions", &mut self.exts);
            ui.label(t("builder-kind"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.kind, 0, t("builder-kind-all"));
                ui.radio_value(&mut self.kind, 1, t("builder-kind-files"));
                ui.radio_value(&mut self.kind, 2, t("builder-kind-folders"));
            });
            ui.end_row();
            ui.label(t("builder-size"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.size_min).desired_width(100.0).hint_text("1mb"));
                ui.label("–");
                ui.add(egui::TextEdit::singleline(&mut self.size_max).desired_width(100.0).hint_text("10mb"));
            });
            ui.end_row();
            ui.label(t("builder-date"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.date_from).desired_width(100.0).hint_text("2026-01-01"));
                ui.label("–");
                ui.add(egui::TextEdit::singleline(&mut self.date_to).desired_width(100.0).hint_text("today"));
            });
            ui.end_row();
            ui.label(t("builder-attributes"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.hidden, t("builder-hidden"));
                ui.checkbox(&mut self.system, t("builder-system"));
                ui.checkbox(&mut self.readonly, t("builder-readonly"));
            });
            ui.end_row();
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(t("builder-preview"));
            ui.monospace(self.query());
        });
    }
}

enum Act {
    None,
    Close,
    Keep,
}

fn buttons(ui: &mut egui::Ui, ok: &str) -> (bool, bool) {
    let mut r = (false, false);
    ui.horizontal(|ui| {
        r.0 = ui.button(t(ok)).clicked();
        r.1 = ui.button(t("dialog-cancel")).clicked();
    });
    r
}

impl FennecApp {
    /// Draws the open dialog (if any).
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut act = Act::Keep;
        let mut cmds: Vec<Command> = Vec::new();
        let title = match &d {
            Dialog::NewFolder { .. } => t("cmd-new-folder"),
            Dialog::SetRunCount { .. } => t("cmd-set-run-count"),
            Dialog::Builder(_) => t("cmd-advanced-search"),
            Dialog::EditBookmark { .. } => t("cmd-add-bookmark"),
            Dialog::Bookmarks { .. } => t("cmd-organize-bookmarks"),
            Dialog::EditFilter { .. } => t("dialog-edit-filter"),
            Dialog::Filters { .. } => t("cmd-organize-filters"),
            Dialog::Export { .. } => t("cmd-export"),
            Dialog::ConfirmDelete { .. } => t("cmd-delete-permanently"),
            Dialog::ContextMenu { .. } => String::new(),
            Dialog::Help => t("cmd-help"),
            Dialog::About => t("cmd-about"),
        };
        if let Dialog::ContextMenu { at } = &d {
            let area = egui::Area::new(egui::Id::new("fennec-ctx-menu")).fixed_pos(*at).order(egui::Order::Foreground);
            let r = area.show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    super::menus::app_context_menu(ui, &mut cmds);
                })
            });
            let clicked_outside = ctx.input(|i| i.pointer.any_click()) && !r.response.hovered();
            act = if !cmds.is_empty() || clicked_outside { Act::Close } else { Act::Keep };
        } else {
            let mut open = true;
            egui::Window::new(title)
                .collapsible(false)
                .resizable(true)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .open(&mut open)
                .show(ctx, |ui| {
                    act = self.dialog_body(ui, &mut d, &mut cmds);
                });
            if !open {
                act = Act::Close;
            }
        }
        match act {
            Act::Keep | Act::None => self.dialog = Some(d),
            Act::Close => {}
        }
        self.pending.extend(cmds);
    }

    fn dialog_body(&mut self, ui: &mut egui::Ui, d: &mut Dialog, cmds: &mut Vec<Command>) -> Act {
        match d {
            Dialog::NewFolder { parent, name } => {
                ui.label(t_args("dialog-new-folder-in", &[("path", parent.display().to_string().into())]));
                let r = ui.text_edit_singleline(name);
                r.request_focus();
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let (ok, cancel) = buttons(ui, "dialog-create");
                if (ok || enter) && !name.trim().is_empty() {
                    let (owner, parent, name) = (self.owner(), parent.clone(), name.trim().to_string());
                    self.spawn_op(t_args("msg-created", &[("name", name.clone().into())]), move || {
                        fnc_platform::fileops::new_folder(owner, &parent, &name)
                    });
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::SetRunCount { text } => {
                ui.label(t("dialog-run-count"));
                ui.text_edit_singleline(text);
                let (ok, cancel) = buttons(ui, "dialog-ok");
                if ok && let Ok(n) = text.trim().parse::<u32>() {
                    let ids = self.target_ids();
                    let (shared, engine) = (self.shared.clone(), self.engine.handle());
                    let _ = std::thread::Builder::new().name("fennec-run-count".into()).spawn(move || {
                        let mut s = shared.write();
                        for id in ids {
                            s.set_run_count(id, n);
                        }
                        drop(s);
                        engine.refresh();
                    });
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::Builder(b) => {
                b.ui(ui);
                let (ok, cancel) = buttons(ui, "dialog-search");
                if ok {
                    let q = b.query();
                    self.set_query(q);
                    self.focus_search = true;
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::EditBookmark { index, bookmark } => {
                bookmark_form(ui, bookmark, &self.filters);
                let (ok, cancel) = buttons(ui, "dialog-ok");
                if ok && !bookmark.name.trim().is_empty() {
                    match index {
                        Some(i) if *i < self.bookmarks.list.len() => self.bookmarks.list[*i] = bookmark.clone(),
                        _ => self.bookmarks.list.push(bookmark.clone()),
                    }
                    self.save_lists();
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::Bookmarks { selected } => {
                let mut open: Option<usize> = None;
                let mut edit: Option<usize> = None;
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (i, b) in self.bookmarks.list.iter().enumerate() {
                        let r = ui.selectable_label(*selected == Some(i), format!("{}    {}", b.name, b.search));
                        if r.clicked() {
                            *selected = Some(i);
                        }
                        if r.double_clicked() {
                            open = Some(i);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(t("dialog-add")).clicked() {
                        cmds.push(Command::AddBookmark);
                    }
                    let sel = selected.filter(|&i| i < self.bookmarks.list.len());
                    if ui.add_enabled(sel.is_some(), egui::Button::new(t("dialog-edit"))).clicked() {
                        edit = sel;
                    }
                    if ui.add_enabled(sel.is_some(), egui::Button::new(t("dialog-delete"))).clicked()
                        && let Some(i) = sel
                    {
                        self.bookmarks.list.remove(i);
                        *selected = None;
                        self.save_lists();
                    }
                    if ui.add_enabled(sel.is_some_and(|i| i > 0), egui::Button::new("▲")).clicked()
                        && let Some(i) = sel
                    {
                        self.bookmarks.list.swap(i, i - 1);
                        *selected = Some(i - 1);
                        self.save_lists();
                    }
                    if ui
                        .add_enabled(sel.is_some_and(|i| i + 1 < self.bookmarks.list.len()), egui::Button::new("▼"))
                        .clicked()
                        && let Some(i) = sel
                    {
                        self.bookmarks.list.swap(i, i + 1);
                        *selected = Some(i + 1);
                        self.save_lists();
                    }
                    if ui.button(t("dialog-import")).clicked() {
                        self.import_csv(false);
                    }
                    if ui.button(t("dialog-close")).clicked() {
                        *selected = Some(usize::MAX);
                    }
                });
                if *selected == Some(usize::MAX) || !cmds.is_empty() {
                    return Act::Close;
                }
                if let Some(i) = open {
                    self.open_bookmark(i);
                    return Act::Close;
                }
                if let Some(i) = edit {
                    *d = Dialog::EditBookmark { index: Some(i), bookmark: self.bookmarks.list[i].clone() };
                }
                Act::Keep
            }
            Dialog::EditFilter { index, filter } => {
                filter_form(ui, filter);
                let (ok, cancel) = buttons(ui, "dialog-ok");
                if ok && !filter.name.trim().is_empty() {
                    match index {
                        Some(i) if *i < self.filters.list.len() => self.filters.list[*i] = filter.clone(),
                        _ => self.filters.list.push(filter.clone()),
                    }
                    self.save_lists();
                    self.rebuild_search_options();
                    self.send_search();
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::Filters { selected } => {
                let mut edit: Option<usize> = None;
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (i, f) in self.filters.list.iter().enumerate() {
                        let label = format!("{}    {}", f.name, f.search);
                        if ui.selectable_label(*selected == Some(i), label).clicked() {
                            *selected = Some(i);
                        }
                    }
                });
                let sel = selected.filter(|&i| i < self.filters.list.len());
                let user = sel.filter(|&i| !self.filters.list[i].builtin);
                ui.horizontal(|ui| {
                    if ui.button(t("dialog-add")).clicked() {
                        cmds.push(Command::AddFilter);
                    }
                    if ui.add_enabled(user.is_some(), egui::Button::new(t("dialog-edit"))).clicked() {
                        edit = user;
                    }
                    if ui.add_enabled(user.is_some(), egui::Button::new(t("dialog-delete"))).clicked()
                        && let Some(i) = user
                    {
                        self.filters.list.remove(i);
                        self.active_filters.clear();
                        *selected = None;
                        self.save_lists();
                        self.rebuild_search_options();
                        self.send_search();
                    }
                    if ui.button(t("dialog-import")).clicked() {
                        self.import_csv(true);
                    }
                    if ui.button(t("dialog-close")).clicked() {
                        *selected = Some(usize::MAX);
                    }
                });
                if *selected == Some(usize::MAX) || !cmds.is_empty() {
                    return Act::Close;
                }
                if let Some(i) = edit {
                    *d = Dialog::EditFilter { index: Some(i), filter: self.filters.list[i].clone() };
                }
                Act::Keep
            }
            Dialog::Export { path } => {
                ui.label(t("dialog-export-help"));
                ui.add(egui::TextEdit::singleline(path).desired_width(420.0));
                let (ok, cancel) = buttons(ui, "dialog-save");
                if ok {
                    self.export(PathBuf::from(path.trim()));
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::ConfirmDelete { paths } => {
                ui.label(t_args("dialog-confirm-delete", &[("count", paths.len().into())]));
                for p in paths.iter().take(8) {
                    ui.monospace(p.display().to_string());
                }
                if paths.len() > 8 {
                    ui.weak("…");
                }
                let (ok, cancel) = buttons(ui, "dialog-delete");
                if ok {
                    let paths = std::mem::take(paths);
                    self.delete_paths(paths, true);
                    return Act::Close;
                }
                if cancel { Act::Close } else { Act::Keep }
            }
            Dialog::Help => {
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    ui.label(t("help-syntax"));
                });
                if ui.button(t("dialog-close")).clicked() { Act::Close } else { Act::Keep }
            }
            Dialog::About => {
                ui.heading(format!("Fennec {}", env!("CARGO_PKG_VERSION")));
                ui.label(t("about-text"));
                if ui.button(t("dialog-close")).clicked() { Act::Close } else { Act::Keep }
            }
            Dialog::ContextMenu { .. } => Act::None,
        }
    }

    fn save_lists(&mut self) {
        if let Err(e) = self.filters.save() {
            self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
        }
        if let Err(e) = self.bookmarks.save() {
            self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
        }
    }

    /// Imports `Filters.csv` / `Bookmarks.csv` from the data folder or the Everything
    /// folder in `%APPDATA%`.
    fn import_csv(&mut self, filters: bool) {
        let name = if filters { "Filters.csv" } else { "Bookmarks.csv" };
        let mut candidates = vec![crate::filters::data_file(&self.settings_path, name)];
        if let Some(app) = std::env::var_os("APPDATA") {
            candidates.push(PathBuf::from(app).join("Everything").join(name));
        }
        let Some((path, text)) =
            candidates.iter().find_map(|p| std::fs::read_to_string(p).ok().map(|t| (p.clone(), t)))
        else {
            self.say(t_args("msg-import-missing", &[("file", name.into())]));
            return;
        };
        let n = if filters { self.filters.import_csv(&text) } else { self.bookmarks.import_csv(&text) };
        self.save_lists();
        self.say(t_args("msg-imported", &[("count", n.into()), ("file", path.display().to_string().into())]));
    }

    /// Writes the current results as EFU (`.efu`), CSV (`.csv`) or a path list.
    fn export(&mut self, path: PathBuf) {
        let ids = self.results.ids.clone();
        let shared = self.shared.clone();
        let format = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let done = t_args("msg-exported", &[("count", ids.len().into()), ("path", path.display().to_string().into())]);
        self.spawn_op(done, move || {
            let snap = shared.snapshot();
            let file = std::fs::File::create(&path)?;
            let mut w = std::io::BufWriter::new(file);
            let mut buf = String::new();
            use std::io::Write;
            match format.as_str() {
                "efu" | "csv" => {
                    let mut ew = fnc_index::efu::EfuWriter::new(w)?;
                    for id in ids {
                        if snap.is_live(id) {
                            ew.write_entry(&snap, id, &mut buf)?;
                        }
                    }
                    ew.finish()?.flush()?;
                }
                _ => {
                    for id in ids {
                        if snap.is_live(id) {
                            snap.full_path(id, &mut buf);
                            writeln!(w, "{buf}")?;
                        }
                    }
                    w.flush()?;
                }
            }
            Ok(())
        });
    }
}

fn bookmark_form(ui: &mut egui::Ui, b: &mut Bookmark, filters: &crate::filters::Filters) {
    egui::Grid::new("bookmark").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label(t("field-name"));
        ui.add(egui::TextEdit::singleline(&mut b.name).desired_width(320.0));
        ui.end_row();
        ui.label(t("field-search"));
        ui.add(egui::TextEdit::singleline(&mut b.search).desired_width(320.0));
        ui.end_row();
        ui.label(t("field-filter"));
        egui::ComboBox::from_id_salt("bm-filter")
            .selected_text(if b.filter.is_empty() { t("filter-everything") } else { b.filter.clone() })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut b.filter, String::new(), t("filter-everything"));
                for f in &filters.list {
                    ui.selectable_value(&mut b.filter, f.name.clone(), &f.name);
                }
            });
        ui.end_row();
        ui.label(t("field-sort"));
        ui.add(egui::TextEdit::singleline(&mut b.sort).desired_width(320.0).hint_text("size-descending"));
        ui.end_row();
        ui.label(t("field-folder"));
        ui.add(egui::TextEdit::singleline(&mut b.folder).desired_width(320.0));
        ui.end_row();
    });
}

fn filter_form(ui: &mut egui::Ui, f: &mut Filter) {
    egui::Grid::new("filter").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label(t("field-name"));
        ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(320.0));
        ui.end_row();
        ui.label(t("field-search"));
        ui.add(egui::TextEdit::singleline(&mut f.search).desired_width(320.0));
        ui.end_row();
        ui.label(t("field-macro"));
        ui.add(egui::TextEdit::singleline(&mut f.macro_name).desired_width(160.0));
        ui.end_row();
        ui.label("");
        ui.vertical(|ui| {
            ui.checkbox(&mut f.case, t("builder-match-case"));
            ui.checkbox(&mut f.whole_word, t("builder-whole-word"));
            ui.checkbox(&mut f.path, t("cmd-match-path"));
            ui.checkbox(&mut f.diacritics, t("cmd-match-diacritics"));
            ui.checkbox(&mut f.regex, t("cmd-enable-regex"));
        });
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_generates_everything_syntax() {
        let b = Builder {
            all_words: "quarterly report".into(),
            exact_phrase: "final draft".into(),
            any_words: "pdf docx".into(),
            none_words: "old".into(),
            folder: r"D:\Docs\".into(),
            subfolders: true,
            exts: "*.pdf; .docx".into(),
            kind: 1,
            size_min: "1mb".into(),
            date_from: "2026-01-01".into(),
            date_to: "today".into(),
            hidden: true,
            ..Builder::default()
        };
        assert_eq!(
            b.query(),
            r#"quarterly report "final draft" <pdf|docx> !old "D:\Docs\" ext:pdf;docx file: size:>=1mb dm:2026-01-01..today attrib:H"#
        );
        let c = Builder { all_words: "a".into(), match_case: true, folder: "C:\\x".into(), ..Builder::default() };
        assert_eq!(c.query(), r#"case:a parent:"C:\x""#);
        // Everything it generates parses without diagnostics.
        let parsed = fnc_query::parse(&b.query(), &fnc_query::ParseEnv::default());
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }
}

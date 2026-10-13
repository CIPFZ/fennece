//! Menu bar (SPEC ch.3 §1) and the application items of the context menu (§4.4).

use egui::Ui;

use super::FennecApp;
use crate::commands::Command;
use crate::i18n::t;

/// Everything commands placed on top of the Explorer context menu; the item id is the
/// command's index in [`Command::ALL`].
pub(super) fn shell_menu_items() -> Vec<fnc_platform::fileops::MenuItem> {
    let item = |c: Command, sep: bool| fnc_platform::fileops::MenuItem {
        id: Command::ALL.iter().position(|&x| x == c).unwrap_or(0) as u32,
        label: c.label(),
        separator_after: sep,
        disabled: false,
    };
    vec![
        item(Command::Open, false),
        item(Command::OpenPath, false),
        item(Command::ExploreInFennec, true),
        item(Command::CopyFullPath, false),
        item(Command::CopyName, false),
        item(Command::Omit, true),
    ]
}

/// The Everything-command part of the context menu as an egui menu (other platforms,
/// or when the shell menu is unavailable).
pub(super) fn app_context_menu(ui: &mut Ui, out: &mut Vec<Command>) {
    use Command as K;
    for group in [
        &[K::Open, K::OpenPath, K::OpenWith, K::ExploreInFennec][..],
        &[K::CopyFiles, K::CutFiles, K::Paste][..],
        &[K::CopyFullPath, K::CopyFullPathQuoted, K::CopyName, K::CopyPath, K::CopyAsTsv, K::CopyAsCsv][..],
        &[K::Rename, K::Delete, K::NewFolder][..],
        &[K::Omit, K::SetRunCount, K::Properties][..],
    ] {
        for &c in group {
            if ui.button(c.label()).clicked() {
                out.push(c);
                ui.close();
            }
        }
        ui.separator();
    }
}

impl FennecApp {
    fn item(&self, ui: &mut Ui, cmd: Command, out: &mut Vec<Command>) {
        let shortcut = self.keymap.shortcut_text(cmd);
        let mut b = egui::Button::new(cmd.label());
        if !shortcut.is_empty() {
            b = b.shortcut_text(shortcut);
        }
        if ui.add(b).clicked() {
            out.push(cmd);
            ui.close();
        }
    }

    fn check(&self, ui: &mut Ui, cmd: Command, on: bool, out: &mut Vec<Command>) {
        let shortcut = self.keymap.shortcut_text(cmd);
        let label = if on { format!("✔ {}", cmd.label()) } else { format!("    {}", cmd.label()) };
        let mut b = egui::Button::new(label);
        if !shortcut.is_empty() {
            b = b.shortcut_text(shortcut);
        }
        if ui.add(b).clicked() {
            out.push(cmd);
            ui.close();
        }
    }

    pub(super) fn menu_bar(&mut self, ui: &mut Ui) {
        use Command as K;
        let mut out = Vec::new();
        let mut run_bookmark: Option<usize> = None;
        let mut choose_filter: Option<usize> = None;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(t("menu-file"), |ui| {
                self.item(ui, K::NewWindow, &mut out);
                self.item(ui, K::CloseWindow, &mut out);
                ui.separator();
                self.item(ui, K::Export, &mut out);
                ui.separator();
                self.item(ui, K::HideWindow, &mut out);
                self.item(ui, K::Exit, &mut out);
            });
            ui.menu_button(t("menu-edit"), |ui| {
                for c in [K::CutFiles, K::CopyFiles, K::Paste] {
                    self.item(ui, c, &mut out);
                }
                ui.separator();
                ui.menu_button(t("menu-copy-as"), |ui| {
                    for c in [
                        K::CopyFullPath,
                        K::CopyFullPathQuoted,
                        K::CopyName,
                        K::CopyPath,
                        K::CopyAsTsv,
                        K::CopyAsCsv,
                        K::CopyAsEfu,
                    ] {
                        self.item(ui, c, &mut out);
                    }
                });
                ui.separator();
                for c in [K::SelectAll, K::InvertSelection] {
                    self.item(ui, c, &mut out);
                }
                ui.separator();
                for c in
                    [K::Rename, K::Delete, K::DeletePermanently, K::NewFolder, K::Omit, K::SetRunCount, K::Properties]
                {
                    self.item(ui, c, &mut out);
                }
            });
            ui.menu_button(t("menu-view"), |ui| {
                self.check(ui, K::ToggleMenuBar, self.settings.show_menu_bar, &mut out);
                self.check(ui, K::ToggleFilterBar, self.settings.show_filter_bar, &mut out);
                self.check(ui, K::ToggleStatusBar, self.settings.show_status_bar, &mut out);
                self.check(ui, K::TogglePreview, self.settings.show_preview, &mut out);
                ui.separator();
                {
                    use fnc_core::settings::ViewMode;
                    let v = self.settings.view_mode;
                    self.check(ui, K::ViewDetails, v == ViewMode::Details, &mut out);
                    self.check(ui, K::ViewMedium, v == ViewMode::Medium, &mut out);
                    self.check(ui, K::ViewLarge, v == ViewMode::Large, &mut out);
                    self.check(ui, K::ViewExtraLarge, v == ViewMode::ExtraLarge, &mut out);
                }
                ui.separator();
                ui.menu_button(t("menu-theme"), |ui| {
                    use fnc_core::settings::Theme;
                    self.check(ui, K::ThemeSystem, self.settings.theme == Theme::System, &mut out);
                    self.check(ui, K::ThemeLight, self.settings.theme == Theme::Light, &mut out);
                    self.check(ui, K::ThemeDark, self.settings.theme == Theme::Dark, &mut out);
                });
                ui.menu_button(t("menu-zoom"), |ui| {
                    for c in [K::ZoomIn, K::ZoomOut, K::ZoomReset] {
                        self.item(ui, c, &mut out);
                    }
                });
                ui.separator();
                self.item(ui, K::Refresh, &mut out);
            });
            ui.menu_button(t("menu-search"), |ui| {
                self.check(ui, K::ToggleMatchCase, self.settings.match_case, &mut out);
                self.check(ui, K::ToggleWholeWord, self.settings.match_whole_word, &mut out);
                self.check(ui, K::ToggleMatchPath, self.settings.match_path, &mut out);
                self.check(ui, K::ToggleDiacritics, self.settings.match_diacritics, &mut out);
                self.check(ui, K::ToggleRegex, self.settings.match_regex, &mut out);
                ui.separator();
                self.item(ui, K::AdvancedSearch, &mut out);
                self.item(ui, K::SearchHistory, &mut out);
                self.item(ui, K::ClearHistory, &mut out);
                ui.separator();
                self.check(ui, K::FilterNone, self.v.active_filters.is_empty(), &mut out);
                let slots = [
                    K::Filter1,
                    K::Filter2,
                    K::Filter3,
                    K::Filter4,
                    K::Filter5,
                    K::Filter6,
                    K::Filter7,
                    K::Filter8,
                    K::Filter9,
                ];
                for (i, f) in self.filters.list.iter().enumerate() {
                    let on = self.v.active_filters.contains(&i);
                    let label = if on { format!("✔ {}", f.name) } else { format!("    {}", f.name) };
                    let mut b = egui::Button::new(label);
                    if let Some(&c) = slots.get(i) {
                        let s = self.keymap.shortcut_text(c);
                        if !s.is_empty() {
                            b = b.shortcut_text(s);
                        }
                    }
                    if ui.add(b).clicked() {
                        choose_filter = Some(i);
                        ui.close();
                    }
                }
                ui.separator();
                self.item(ui, K::AddFilter, &mut out);
                self.item(ui, K::OrganizeFilters, &mut out);
            });
            ui.menu_button(t("menu-bookmarks"), |ui| {
                self.item(ui, K::AddBookmark, &mut out);
                self.item(ui, K::OrganizeBookmarks, &mut out);
                if !self.bookmarks.list.is_empty() {
                    ui.separator();
                }
                let mut folders: Vec<&str> =
                    self.bookmarks.list.iter().map(|b| b.folder.as_str()).filter(|f| !f.is_empty()).collect();
                folders.sort_unstable();
                folders.dedup();
                for folder in folders {
                    ui.menu_button(folder, |ui| {
                        for (i, b) in self.bookmarks.list.iter().enumerate().filter(|(_, b)| b.folder == folder) {
                            if ui.button(&b.name).clicked() {
                                run_bookmark = Some(i);
                                ui.close();
                            }
                        }
                    });
                }
                for (i, b) in self.bookmarks.list.iter().enumerate().filter(|(_, b)| b.folder.is_empty()) {
                    if ui.button(&b.name).clicked() {
                        run_bookmark = Some(i);
                        ui.close();
                    }
                }
            });
            ui.menu_button(t("menu-tools"), |ui| {
                self.item(ui, K::Options, &mut out);
                ui.separator();
                self.check(ui, K::PauseUpdates, self.indexer.paused(), &mut out);
                self.item(ui, K::IndexJournal, &mut out);
                self.item(ui, K::SaveDatabase, &mut out);
                ui.separator();
                self.item(ui, K::InstallService, &mut out);
                self.item(ui, K::UninstallService, &mut out);
            });
            ui.menu_button(t("menu-help"), |ui| {
                self.item(ui, K::Help, &mut out);
                self.item(ui, K::About, &mut out);
            });
        });
        self.pending.extend(out);
        if let Some(i) = choose_filter {
            self.set_filters(vec![i]);
        }
        if let Some(i) = run_bookmark {
            self.open_bookmark(i);
        }
    }

    /// Applies bookmark `i`: its search, filter and sort.
    pub(super) fn open_bookmark(&mut self, i: usize) {
        let Some(b) = self.bookmarks.list.get(i).cloned() else { return };
        self.v.active_filters = self.filters.find(&b.filter).into_iter().collect();
        self.settings.filter = b.filter.clone();
        if !b.sort.is_empty() {
            let mut s = self.settings.clone();
            s.sort = b.sort.clone();
            self.v.sort = super::sort_from_settings(&s);
        }
        self.rebuild_search_options();
        self.set_query(b.search);
        self.v.focus_search = true;
    }
}

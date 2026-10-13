//! Find bar (SPEC ch.3 §8): finds within the current results and jumps to the match
//! without changing the result set; case, whole word and regex; F3 / Shift+F3.

use std::time::Duration;

use super::FennecApp;
use crate::i18n::{t, t_args};

/// Find bar state of a view.
#[derive(Default)]
pub(super) struct FindBar {
    pub(super) open: bool,
    pub(super) text: String,
    pub(super) case: bool,
    whole_word: bool,
    regex: bool,
    focus: bool,
    /// Last outcome: `Some(true)` found, `Some(false)` not found.
    found: Option<bool>,
    /// Matches in the results (counted when the result set is small enough).
    count: Option<usize>,
}

/// Results counted for the "n matches" label at most.
const COUNT_LIMIT: usize = 200_000;

enum Matcher {
    Text { needle: String, case: bool, whole: bool },
    Regex(regex::Regex),
}

impl Matcher {
    fn new(f: &FindBar) -> Option<Self> {
        if f.text.is_empty() {
            return None;
        }
        if f.regex {
            return regex::RegexBuilder::new(&f.text).case_insensitive(!f.case).build().ok().map(Matcher::Regex);
        }
        let needle = if f.case { f.text.clone() } else { f.text.to_lowercase() };
        Some(Matcher::Text { needle, case: f.case, whole: f.whole_word })
    }

    fn is_match(&self, name: &str) -> bool {
        match self {
            Matcher::Regex(r) => r.is_match(name),
            Matcher::Text { needle, case, whole } => {
                let hay = if *case { name.to_string() } else { name.to_lowercase() };
                if !*whole {
                    return hay.contains(needle.as_str());
                }
                hay.match_indices(needle.as_str()).any(|(i, m)| {
                    let before = hay[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
                    let after = hay[i + m.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
                    before && after
                })
            }
        }
    }
}

impl FennecApp {
    /// Ctrl+F in the result list (find-bar mode).
    pub(super) fn open_find_bar(&mut self) {
        self.v.find.open = true;
        self.v.find.focus = true;
    }

    /// F3 / Shift+F3; `false` if the bar is closed (the key keeps its other meaning).
    pub(super) fn find_step(&mut self, forward: bool) -> bool {
        if !self.v.find.open {
            return false;
        }
        let Some(m) = Matcher::new(&self.v.find) else { return true };
        let Some(snap) = self.shared.try_snapshot_for(Duration::from_millis(50)) else { return true };
        let ids = &self.v.results.ids;
        let n = ids.len();
        if n == 0 {
            self.v.find.found = Some(false);
            return true;
        }
        let start = self.v.selected.map_or(if forward { n - 1 } else { 0 }, |s| s.min(n - 1));
        let mut name = String::new();
        let mut hit = None;
        for k in 1..=n {
            let i = if forward { (start + k) % n } else { (start + n - k % n) % n };
            snap.name_into(ids[i], &mut name);
            if m.is_match(&name) {
                hit = Some(i);
                break;
            }
        }
        drop(snap);
        self.v.find.found = Some(hit.is_some());
        if let Some(i) = hit {
            self.select_only(i);
        }
        true
    }

    fn recount(&mut self) {
        self.v.find.count = None;
        let Some(m) = Matcher::new(&self.v.find) else { return };
        if self.v.results.len() > COUNT_LIMIT {
            return;
        }
        let Some(snap) = self.shared.try_snapshot_for(Duration::from_millis(20)) else { return };
        let mut name = String::new();
        let count = self
            .v
            .results
            .ids
            .iter()
            .filter(|&&id| {
                snap.name_into(id, &mut name);
                m.is_match(&name)
            })
            .count();
        self.v.find.count = Some(count);
    }

    /// The bar, above the status bar.
    pub(super) fn find_bar(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let (mut next, mut prev, mut close) = (false, false, false);
        ui.horizontal(|ui| {
            ui.label(t("find-label"));
            let f = &mut self.v.find;
            let r = ui.add(egui::TextEdit::singleline(&mut f.text).desired_width(260.0).id_salt("find-bar"));
            if f.focus {
                f.focus = false;
                r.request_focus();
            }
            changed |= r.changed();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if ui.input(|i| i.modifiers.shift) {
                    prev = true
                } else {
                    next = true
                }
                r.request_focus();
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            changed |= ui.toggle_value(&mut f.case, "Aa").on_hover_text(t("find-case")).changed();
            changed |= ui.toggle_value(&mut f.whole_word, "ab").on_hover_text(t("find-whole-word")).changed();
            changed |= ui.toggle_value(&mut f.regex, ".*").on_hover_text(t("find-regex")).changed();
            prev |= ui.small_button("▲").on_hover_text(t("find-previous")).clicked();
            next |= ui.small_button("▼").on_hover_text(t("find-next")).clicked();
            match (f.found, f.count) {
                (Some(false), _) => {
                    ui.colored_label(ui.visuals().warn_fg_color, t("find-none"));
                }
                (_, Some(c)) if !f.text.is_empty() => {
                    ui.weak(t_args("find-count", &[("count", c.into())]));
                }
                _ => {}
            }
            close |= ui.small_button("×").on_hover_text(t("find-close")).clicked();
        });
        if changed {
            self.v.find.found = None;
            self.recount();
            // Typing jumps to the first match from the current row (incremental find).
            if let Some(s) = self.v.selected {
                self.v.selected = Some(s.saturating_sub(1).min(s));
            }
            self.find_step(true);
        }
        if next {
            self.find_step(true);
        }
        if prev {
            self.find_step(false);
        }
        if close {
            self.v.find.open = false;
            self.v.focus = super::Focus::Results;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_modes() {
        let f = |text: &str, case: bool, whole: bool, regex: bool| FindBar {
            text: text.into(),
            case,
            whole_word: whole,
            regex,
            ..FindBar::default()
        };
        let m = Matcher::new(&f("report", false, false, false)).unwrap();
        assert!(m.is_match("Q3 Report.docx") && !m.is_match("notes.txt"));
        let m = Matcher::new(&f("Report", true, false, false)).unwrap();
        assert!(!m.is_match("report.docx"));
        let m = Matcher::new(&f("port", false, true, false)).unwrap();
        assert!(!m.is_match("report.docx") && m.is_match("port 80.txt"));
        let m = Matcher::new(&f(r"^img_\d+", false, false, true)).unwrap();
        assert!(m.is_match("IMG_0001.JPG") && !m.is_match("my IMG_1.jpg"));
        assert!(Matcher::new(&f("", false, false, false)).is_none());
        assert!(Matcher::new(&f("(", false, false, true)).is_none());
    }
}

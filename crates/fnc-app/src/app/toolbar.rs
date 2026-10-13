//! Toolbar (SPEC ch.3 §1, §12): buttons for the commands listed in the `toolbar`
//! setting (any command id; `|` is a separator), with back/forward through the searches
//! of the current tab.

use super::FennecApp;
use crate::commands::Command;

/// Back/forward history of one tab (searches that were used, as in the search history).
#[derive(Default)]
pub(super) struct Nav {
    back: Vec<String>,
    forward: Vec<String>,
    current: Option<String>,
}

impl Nav {
    /// A search was used.
    pub(super) fn visit(&mut self, q: &str) {
        if self.current.as_deref() == Some(q) {
            return;
        }
        if let Some(c) = self.current.take() {
            self.back.push(c);
            if self.back.len() > 100 {
                self.back.remove(0);
            }
        }
        self.forward.clear();
        self.current = Some(q.to_string());
    }

    /// The previous search.
    pub(super) fn back(&mut self) -> Option<String> {
        let q = self.back.pop()?;
        if let Some(c) = self.current.replace(q.clone()) {
            self.forward.push(c);
        }
        Some(q)
    }

    /// The next search.
    pub(super) fn forward(&mut self) -> Option<String> {
        let q = self.forward.pop()?;
        if let Some(c) = self.current.replace(q.clone()) {
            self.back.push(c);
        }
        Some(q)
    }

    fn can(&self, back: bool) -> bool {
        if back { !self.back.is_empty() } else { !self.forward.is_empty() }
    }
}

/// Short glyphs for common commands; others show their name.
fn glyph(c: Command) -> Option<&'static str> {
    Some(match c {
        Command::NavigateBack => "◀",
        Command::NavigateForward => "▶",
        Command::Refresh => "⟳",
        Command::NewTab => "＋",
        _ => return None,
    })
}

impl FennecApp {
    /// Back (Alt+Left).
    pub(super) fn navigate(&mut self, back: bool) {
        let q = if back { self.v.nav.back() } else { self.v.nav.forward() };
        if let Some(q) = q {
            self.set_query(q);
            // Not a new visit.
            self.v.history_recorded = true;
        }
    }

    /// The toolbar row.
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut run = None;
        ui.horizontal(|ui| {
            for id in self.settings.toolbar.clone() {
                let id = id.trim();
                if id == "|" {
                    ui.separator();
                    continue;
                }
                let Some(&c) = Command::ALL.iter().find(|c| c.id() == id) else { continue };
                let enabled = match c {
                    Command::NavigateBack => self.v.nav.can(true),
                    Command::NavigateForward => self.v.nav.can(false),
                    _ => true,
                };
                let text = glyph(c).map_or_else(|| c.label(), str::to_string);
                if ui.add_enabled(enabled, egui::Button::new(text)).on_hover_text(c.label()).clicked() {
                    run = Some(c);
                }
            }
        });
        if let Some(c) = run {
            self.pending.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward() {
        let mut n = Nav::default();
        n.visit("a");
        n.visit("b");
        n.visit("b");
        n.visit("c");
        assert_eq!(n.back().as_deref(), Some("b"));
        assert_eq!(n.back().as_deref(), Some("a"));
        assert_eq!(n.back(), None);
        assert_eq!(n.forward().as_deref(), Some("b"));
        n.visit("d");
        assert_eq!(n.forward(), None, "a new search clears forward");
        assert_eq!(n.back().as_deref(), Some("b"));
    }
}

//! Tabs (SPEC ch.3 §7): every tab is a [`View`] with its own search, filters, sort,
//! selection and engine. The active tab lives in `FennecApp::v`; the others wait in
//! their slots. Secondary windows have their own tab strips (swapped in with the view).

use super::{FennecApp, View};
use crate::i18n::t;

/// Tab strip of one window.
pub(super) struct Tabs {
    /// One slot per tab; the active slot is empty (its view is `FennecApp::v`).
    slots: Vec<Option<View>>,
    active: usize,
}

impl Default for Tabs {
    fn default() -> Self {
        Self { slots: vec![None], active: 0 }
    }
}

impl Tabs {
    /// Index of the active tab.
    pub(super) fn active(&self) -> usize {
        self.active
    }

    /// Queries of the inactive tabs by index (the active one is `None`).
    pub(super) fn queries(&self) -> Vec<Option<String>> {
        self.slots.iter().map(|s| s.as_ref().map(|v| v.query.clone())).collect()
    }

    /// The view of inactive tab `i`.
    pub(super) fn view(&self, i: usize) -> Option<&View> {
        self.slots.get(i)?.as_ref()
    }
}

/// Label of a tab: its search, or "New tab".
fn label(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return t("tab-new");
    }
    let mut s: String = q.chars().take(28).collect();
    if q.chars().count() > 28 {
        s.push('…');
    }
    s
}

impl FennecApp {
    /// Ctrl+T: a new tab after the current one, searching for `query`.
    pub(super) fn tab_new(&mut self, query: String) {
        let mut view = self.new_view(query);
        std::mem::swap(&mut self.v, &mut view);
        // `view` is the tab we left.
        let at = self.tabs.active;
        self.tabs.slots[at] = Some(view);
        self.tabs.slots.insert(at + 1, None);
        self.tabs.active = at + 1;
        self.rebuild_search_options();
        self.send_search();
        self.v.focus_search = true;
    }

    /// Switches to tab `j`.
    pub(super) fn tab_switch(&mut self, j: usize) {
        if j == self.tabs.active || j >= self.tabs.slots.len() {
            return;
        }
        let Some(mut next) = self.tabs.slots[j].take() else { return };
        std::mem::swap(&mut self.v, &mut next);
        let at = self.tabs.active;
        self.tabs.slots[at] = Some(next);
        self.tabs.active = j;
        // Results of a background tab may predate index changes.
        self.v.engine.refresh();
        self.v.focus_search = true;
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab.
    pub(super) fn tab_step(&mut self, delta: isize) {
        let n = self.tabs.slots.len() as isize;
        if n > 1 {
            let j = (self.tabs.active as isize + delta).rem_euclid(n) as usize;
            self.tab_switch(j);
        }
    }

    /// Ctrl+W: closes the current tab; returns `false` if it was the last one (the
    /// caller closes the window unless `close_last_tab_keeps_window`).
    pub(super) fn tab_close(&mut self) -> bool {
        if self.tabs.slots.len() <= 1 {
            if self.settings.close_last_tab_keeps_window {
                self.v.query.clear();
                self.send_search();
                return true;
            }
            return false;
        }
        let at = self.tabs.active;
        let j = if at + 1 < self.tabs.slots.len() { at + 1 } else { at - 1 };
        let Some(mut next) = self.tabs.slots[j].take() else { return true };
        std::mem::swap(&mut self.v, &mut next);
        // `next` is the closed tab's view: dropping it stops its engine.
        drop(next);
        self.tabs.slots.remove(at);
        self.tabs.active = if j > at { at } else { j };
        self.v.engine.refresh();
        true
    }

    /// The tab bar (shown with more than one tab, or always with `show_tab_bar`).
    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui) {
        if self.tabs.slots.len() < 2 && !self.settings.show_tab_bar {
            return;
        }
        let mut switch = None;
        let mut close = None;
        let mut new = false;
        ui.horizontal(|ui| {
            for (i, slot) in self.tabs.slots.iter().enumerate() {
                let q = match slot {
                    Some(v) => v.query.as_str(),
                    None => self.v.query.as_str(),
                };
                let active = i == self.tabs.active;
                let r = ui.selectable_label(active, label(q)).on_hover_text(q);
                if r.clicked() {
                    switch = Some(i);
                }
                if r.middle_clicked() {
                    close = Some(i);
                }
                if active
                    && self.tabs.slots.len() > 1
                    && ui.small_button("×").on_hover_text(t("cmd-close-tab")).clicked()
                {
                    close = Some(i);
                }
            }
            if ui.small_button("+").on_hover_text(t("cmd-new-tab")).clicked() {
                new = true;
            }
        });
        if let Some(i) = close {
            self.tab_switch(i);
            self.tab_close();
        } else if let Some(i) = switch {
            self.tab_switch(i);
        }
        if new {
            self.tab_new(String::new());
        }
    }
}

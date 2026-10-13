//! Search box completion (SPEC ch.3 §2): function, modifier and macro names, history
//! entries and path segments (`C:\Win` → `C:\Windows\`).

use egui::Key;
use fnc_core::EntryId;
use fnc_index::IndexSnapshot;

use crate::history::SearchHistory;

/// One suggestion: the full new search text and its label.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Suggestion {
    pub label: String,
    pub text: String,
    /// Comes from the search history (Del removes it).
    pub history: bool,
}

/// Popup state.
#[derive(Default)]
pub(super) struct CompleteState {
    items: Vec<Suggestion>,
    selected: Option<usize>,
    hovered: bool,
}

const MAX: usize = 12;

/// The token ending at `cursor` (byte offset) and its start.
fn current_token(text: &str, cursor: usize) -> (usize, &str) {
    let head = &text[..cursor.min(text.len())];
    // A quote opens a token that may contain spaces (paths).
    let quotes = head.matches('"').count();
    let start = if quotes % 2 == 1 {
        head.rfind('"').map_or(0, |i| i + 1)
    } else {
        head.rfind([' ', '|', '<', '>', '!']).map_or(0, |i| i + 1)
    };
    (start, &head[start..])
}

/// Names (functions, modifiers, macros) starting with `prefix`.
fn names(prefix: &str) -> Vec<String> {
    let p = prefix.to_ascii_lowercase();
    if p.len() < 2 || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Vec::new();
    }
    let mut v: Vec<String> = fnc_query::funcs::FUNCTION_TABLE
        .iter()
        .map(|(n, _)| *n)
        .chain(fnc_query::funcs::MODIFIER_TABLE.iter().map(|(n, _, _)| *n))
        .chain(fnc_query::funcs::MACRO_TABLE.iter().map(|(n, _)| *n))
        .filter(|n| n.starts_with(&p) && *n != p)
        .map(|n| format!("{n}:"))
        .collect();
    v.sort();
    v.dedup();
    v.truncate(MAX);
    v
}

/// Children of the folder named by `path` (up to its last separator) whose names start
/// with the rest.
fn path_children(path: &str, snap: &IndexSnapshot) -> Vec<String> {
    let Some(cut) = path.rfind(['\\', '/']) else { return Vec::new() };
    let (dir, rest) = (&path[..cut], &path[cut + 1..]);
    let Some(folder) = resolve_folder(snap, dir) else { return Vec::new() };
    let rest_fold = rest.to_lowercase();
    let sep = path[cut..].chars().next().unwrap_or('\\');
    let mut v: Vec<String> = snap
        .children(folder)
        .filter(|&c| snap.is_live(c) && snap.is_dir(c))
        .map(|c| snap.name_string(c))
        .filter(|n| n.to_lowercase().starts_with(&rest_fold))
        .map(|n| format!("{}{sep}{n}{sep}", &path[..cut]))
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v.truncate(MAX);
    v
}

/// Finds the folder entry for `C:` / `C:\Windows` / `/home/u` by walking the tree.
fn resolve_folder(snap: &IndexSnapshot, dir: &str) -> Option<EntryId> {
    let mut parts = dir.split(['\\', '/']).filter(|p| !p.is_empty());
    let first = if dir.starts_with('/') { "/" } else { parts.next()? };
    let first_fold = first.to_lowercase();
    let root = snap.volumes().iter().flat_map(|v| v.roots.iter().copied()).find(|&r| {
        snap.is_live(r)
            && snap.name_string(r).to_lowercase().trim_end_matches(['\\', '/'])
                == first_fold.trim_end_matches(['\\', '/'])
    })?;
    let mut cur = root;
    for p in parts {
        cur = snap.find_child(cur, fnc_core::fold::fold_str(p).as_bytes())?;
    }
    Some(cur)
}

impl CompleteState {
    pub(super) fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    pub(super) fn hovered(&self) -> bool {
        self.hovered
    }

    pub(super) fn close(&mut self) {
        self.items.clear();
        self.selected = None;
        self.hovered = false;
    }

    /// Shows the most used searches (Ctrl+H / Search → History).
    pub(super) fn open_history(&mut self, h: &SearchHistory) {
        self.items = h
            .entries()
            .iter()
            .take(20)
            .map(|e| Suggestion { label: e.text.clone(), text: e.text.clone(), history: true })
            .collect();
        self.selected = None;
    }

    /// Recomputes suggestions for `text` with the cursor at character `cursor`.
    pub(super) fn update(&mut self, text: &str, cursor: usize, history: &SearchHistory, snap: Option<&IndexSnapshot>) {
        self.close();
        if text.trim().is_empty() {
            return;
        }
        let byte_cursor = text.char_indices().nth(cursor).map_or(text.len(), |(i, _)| i);
        let (start, token) = current_token(text, byte_cursor);
        let replace = |with: &str| format!("{}{}{}", &text[..start], with, &text[byte_cursor..]);
        if token.contains(['\\', '/']) {
            if let Some(s) = snap {
                for p in path_children(token, s) {
                    let quoted =
                        if p.contains(' ') && !text[..start].ends_with('"') { format!("\"{p}") } else { p.clone() };
                    self.items.push(Suggestion { label: p, text: replace(&quoted), history: false });
                }
            }
        } else if !token.contains(':') {
            for n in names(token) {
                self.items.push(Suggestion { label: n.clone(), text: replace(&n), history: false });
            }
        }
        for e in history.matching(text, MAX.saturating_sub(self.items.len()).min(6)) {
            self.items.push(Suggestion { label: format!("🕓 {}", e.text), text: e.text.clone(), history: true });
        }
        self.items.truncate(MAX);
    }

    /// Text of the highlighted entry if it comes from the history.
    pub(super) fn selected_history(&self) -> Option<String> {
        self.selected.and_then(|i| self.items.get(i)).filter(|s| s.history).map(|s| s.text.clone())
    }

    /// Drops the highlighted entry.
    pub(super) fn remove_selected(&mut self) {
        if let Some(i) = self.selected.filter(|&i| i < self.items.len()) {
            self.items.remove(i);
            self.selected = if self.items.is_empty() { None } else { Some(i.min(self.items.len() - 1)) };
        }
    }

    /// Up/Down/Enter/Tab while the popup is open; returns a new search text on accept.
    pub(super) fn key(&mut self, key: Key, _text: &str) -> Option<String> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        match key {
            Key::ArrowDown => {
                self.selected = Some(self.selected.map_or(0, |s| (s + 1) % n));
                None
            }
            Key::ArrowUp => {
                self.selected = Some(self.selected.map_or(n - 1, |s| (s + n - 1) % n));
                None
            }
            Key::Enter | Key::Tab => {
                let pick = self.selected.or((key == Key::Tab).then_some(0))?;
                let s = self.items.get(pick).map(|s| s.text.clone());
                self.close();
                s
            }
            _ => None,
        }
    }

    /// Draws the popup under the search box; returns a picked suggestion.
    pub(super) fn show(&mut self, ui: &egui::Ui, anchor: &egui::Response, _text: &str) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let mut picked = None;
        let area = egui::Area::new(egui::Id::new("fennec-complete"))
            .fixed_pos(anchor.rect.left_bottom())
            .order(egui::Order::Foreground);
        let r = area.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(anchor.rect.width().min(600.0));
                for (i, s) in self.items.iter().enumerate() {
                    if ui.selectable_label(self.selected == Some(i), &s.label).clicked() {
                        picked = Some(s.text.clone());
                    }
                }
            })
        });
        self.hovered = r.response.hovered() || r.response.contains_pointer();
        if picked.is_some() {
            self.close();
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(current_token("abc ext", 7), (4, "ext"));
        assert_eq!(current_token("a \"C:\\Program Fi", 16), (3, "C:\\Program Fi"));
        assert_eq!(current_token("<a|siz", 6), (3, "siz"));
    }

    #[test]
    fn function_names_and_history() {
        let mut h = SearchHistory::in_memory(10);
        h.record("dm:today ext:log");
        let mut c = CompleteState::default();
        c.update("foo dm", 6, &h, None);
        assert!(c.items.iter().any(|s| s.text == "foo dmdupe:" || s.text.starts_with("foo dm")));
        c.update("ext", 3, &h, None);
        assert!(c.items.iter().any(|s| s.label == "extension:"));
        assert!(c.items.iter().any(|s| s.text == "dm:today ext:log"), "history entry containing the text");
        assert!(c.key(Key::ArrowDown, "").is_none());
        assert!(c.key(Key::Enter, "").is_some());
        assert!(!c.is_open());
    }

    #[test]
    fn path_segments_from_the_index() {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Windows\\System32\\x.dll\",1,,,32\n\
                   \"C:\\Program Files\\App\\a.exe\",1,,,32\n\
                   \"C:\\Program Data\\b\",1,,,32\n";
        let mut s = fnc_index::IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        let shared = fnc_index::SharedIndex::default();
        shared.replace(s);
        let snap = shared.snapshot();
        assert_eq!(path_children("C:\\Win", &snap), ["C:\\Windows\\"]);
        assert_eq!(path_children("c:\\program ", &snap), ["c:\\Program Data\\", "c:\\Program Files\\"]);
        assert_eq!(path_children("C:\\Windows\\sys", &snap), ["C:\\Windows\\System32\\"]);
        let mut c = CompleteState::default();
        c.update("\"C:\\Program F", 13, &SearchHistory::in_memory(0), Some(&snap));
        assert_eq!(c.items[0].text, "\"C:\\Program Files\\");
    }
}

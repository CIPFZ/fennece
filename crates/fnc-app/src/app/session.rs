//! Sessions (SPEC ch.3 §7): with `restore_session`, the windows and their tabs (search
//! and sort) are written to `session.toml` on exit and reopened at start.

use serde::{Deserialize, Serialize};

use super::{FennecApp, View};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Tab {
    query: String,
    sort: String,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Window {
    active: usize,
    tabs: Vec<Tab>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Session {
    #[serde(default)]
    window: Vec<Window>,
}

fn tab_of(v: &View) -> Tab {
    Tab { query: v.query.clone(), sort: crate::columns::sort_setting(&v.sort) }
}

impl FennecApp {
    fn session_file(&self) -> std::path::PathBuf {
        crate::filters::data_file(&self.settings_path, "session.toml")
    }

    fn window_state(&self) -> Window {
        let queries = self.tabs.queries();
        let mut tabs = Vec::with_capacity(queries.len());
        for (i, q) in queries.into_iter().enumerate() {
            match q {
                Some(_) => tabs.push(self.tabs.view(i).map(tab_of).unwrap_or_default()),
                None => tabs.push(tab_of(&self.v)),
            }
        }
        Window { active: self.tabs.active(), tabs }
    }

    /// Writes `session.toml` (exit) when sessions are on.
    pub(super) fn save_session(&mut self) {
        if !self.settings.restore_session {
            return;
        }
        let mut s = Session { window: vec![self.window_state()] };
        let mut wins = std::mem::take(&mut self.windows);
        for w in &mut wins {
            std::mem::swap(&mut self.v, &mut w.view);
            std::mem::swap(&mut self.tabs, &mut w.tabs);
            s.window.push(self.window_state());
            std::mem::swap(&mut self.tabs, &mut w.tabs);
            std::mem::swap(&mut self.v, &mut w.view);
        }
        self.windows = wins;
        match toml::to_string(&s) {
            Ok(text) => {
                if let Err(e) = crate::history::write_atomic(&self.session_file(), &text) {
                    tracing::warn!(error = %e, "cannot save the session");
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot save the session"),
        }
    }

    fn restore_tabs(&mut self, w: &Window) {
        for (i, tab) in w.tabs.iter().enumerate() {
            if i > 0 {
                self.tab_new(tab.query.clone());
            } else {
                self.v.query = tab.query.clone();
            }
            if !tab.sort.is_empty() {
                let keys = crate::columns::parse_sort(&tab.sort);
                if let Some(&(c, asc)) = keys.first() {
                    let mut spec = fnc_exec::SortSpec::by(c, asc, self.settings.folders_first);
                    for (k, key) in keys.iter().skip(1).take(2).enumerate() {
                        spec.then[k] = Some(*key);
                    }
                    self.v.sort = spec;
                }
            }
            self.send_search();
        }
        self.tab_switch(w.active.min(w.tabs.len().saturating_sub(1)));
    }

    /// Reopens the saved windows and tabs (start, when sessions are on and the command
    /// line names no search).
    pub(super) fn restore_session(&mut self) {
        if !self.settings.restore_session {
            return;
        }
        let Ok(text) = std::fs::read_to_string(self.session_file()) else { return };
        let s: Session = match toml::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring a damaged session file");
                return;
            }
        };
        let mut it = s.window.iter().filter(|w| !w.tabs.is_empty());
        if let Some(main) = it.next() {
            self.restore_tabs(main);
        }
        for w in it {
            self.open_window(w.tabs[0].query.clone());
            // Build the other tabs with the new window's view swapped in.
            let Some(mut win) = self.windows.pop() else { continue };
            std::mem::swap(&mut self.v, &mut win.view);
            std::mem::swap(&mut self.tabs, &mut win.tabs);
            self.restore_tabs(w);
            std::mem::swap(&mut self.tabs, &mut win.tabs);
            std::mem::swap(&mut self.v, &mut win.view);
            self.windows.push(win);
        }
    }
}

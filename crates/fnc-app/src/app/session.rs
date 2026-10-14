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
        if let Err(e) = crate::upgrade::save_toml(&self.session_file(), &s) {
            tracing::warn!(error = %e, "cannot save the session");
        }
    }

    fn restore_tabs(&mut self, w: &Window) {
        for (i, tab) in w.tabs.iter().enumerate() {
            if i > 0 {
                self.tab_new(tab.query.clone());
            } else {
                self.v.query = tab.query.clone();
            }
            if let Some(spec) = crate::columns::sort_spec(&tab.sort, self.settings.folders_first) {
                self.v.sort = spec;
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
        // A damaged or newer file is copied to a backup first (ADR-038).
        let (s, recovery): (Session, _) = crate::upgrade::load_toml(&self.session_file());
        if let Some(r) = recovery {
            self.notice(crate::upgrade::notice(&r, false));
        }
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

//! Secondary windows (`-newwindow`, Ctrl+N; SPEC ch.3 §7): each is an egui viewport with
//! its own [`View`] (search, results, selection, engine). While a window is drawn its
//! view is swapped into `FennecApp::v`, so every widget and command works unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{FennecApp, View};

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// One extra window.
pub(crate) struct WindowView {
    id: egui::ViewportId,
    pub(super) view: View,
    /// The window's other tabs.
    pub(super) tabs: super::tabs::Tabs,
    open: bool,
}

impl WindowView {
    /// Marks the window closed (exit).
    pub(crate) fn close(&mut self) {
        self.open = false;
    }
}

impl FennecApp {
    /// A fresh view with its own engine, searching for `query`.
    pub(super) fn new_view(&self, query: String) -> View {
        let (engine, engine_rx) = (self.new_engine)();
        View {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            pending_select: None,
            engine,
            engine_rx,
            search_opts: self.v.search_opts.clone(),
            query,
            sent_query: None,
            edited_at: None,
            next_id: 1,
            latest_shown: 0,
            sort: self.v.sort,
            results: Arc::new(fnc_exec::ResultSet::default()),
            highlighter: None,
            diagnostics: Vec::new(),
            elapsed: std::time::Duration::ZERO,
            selected: None,
            selected_id: None,
            sel: super::Selection::default(),
            scroll_to: None,
            rows: Default::default(),
            rows_generation: 0,
            omit: Default::default(),
            active_filters: self.v.active_filters.clone(),
            history_recorded: true,
            history_idle_since: None,
            complete: Default::default(),
            focus: super::Focus::Search,
            focus_search: true,
            keep_focus: false,
            rename: None,
            grid_cols: 1,
            preview: Default::default(),
            find: Default::default(),
            nav: Default::default(),
        }
    }

    /// Opens another window searching for `query` (INT-01).
    pub(super) fn open_window(&mut self, query: String) {
        let mut view = self.new_view(query);
        let id = egui::ViewportId::from_hash_of(("fennec-window", view.uid));
        // Send its first search with the view swapped in.
        std::mem::swap(&mut self.v, &mut view);
        self.rebuild_search_options();
        self.send_search();
        std::mem::swap(&mut self.v, &mut view);
        self.windows.push(WindowView { id, view, tabs: Default::default(), open: true });
    }

    /// Drains a view's finished searches (the active view is drained by `logic`).
    pub(super) fn drain_engine(&mut self) {
        while let Ok(fnc_exec::EngineEvent::Done(o)) = self.v.engine_rx.try_recv() {
            self.on_outcome(o);
        }
    }

    /// Draws every secondary window; closed ones are dropped.
    pub(super) fn secondary_windows(&mut self, ctx: &egui::Context) {
        let mut wins = std::mem::take(&mut self.windows);
        for w in &mut wins {
            std::mem::swap(&mut self.v, &mut w.view);
            std::mem::swap(&mut self.tabs, &mut w.tabs);
            self.drain_engine();
            let title = if self.v.query.is_empty() {
                crate::i18n::t("app-title")
            } else {
                format!("{} - {}", self.v.query, crate::i18n::t("app-title"))
            };
            let builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([900.0, 560.0])
                .with_min_inner_size([400.0, 200.0])
                .with_app_id("fennec");
            let mut closed = false;
            self.in_secondary = true;
            self.close_secondary = false;
            ctx.show_viewport_immediate(w.id, builder, |ui, _class| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed = true;
                }
                let c = ui.ctx().clone();
                self.handle_keys(&c);
                self.draw_main(ui, false);
                self.dialogs(&c);
                for cmd in std::mem::take(&mut self.pending) {
                    self.run(&c, cmd);
                }
                self.after_table(&c);
            });
            self.in_secondary = false;
            if closed || self.close_secondary {
                w.open = false;
            }
            std::mem::swap(&mut self.tabs, &mut w.tabs);
            std::mem::swap(&mut self.v, &mut w.view);
        }
        wins.retain(|w| w.open);
        // Windows opened while drawing (Ctrl+N inside a window) were pushed meanwhile.
        wins.append(&mut self.windows);
        self.windows = wins;
    }
}

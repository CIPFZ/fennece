//! The main window: search box, virtualised result table, status bar (SPEC ch.3 §1–4).
//!
//! The UI thread never blocks: searches run on the engine thread, index snapshots are
//! taken with a 2 ms timeout (falling back to cached rows), and only visible rows are
//! formatted.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Key, Modifiers};
use egui_extras::{Column, TableBuilder};
use fnc_core::settings::{Settings, Theme};
use fnc_core::{EntryFlags, EntryId};
use fnc_exec::{
    EngineEvent, Highlighter, ResultSet, SearchEngine, SearchOptions, SearchOutcome, SearchRequest, SortColumn,
    SortSpec,
};
use fnc_index::SharedIndex;
use fnc_query::DiagKind;
use fnc_query::plan::PlanDiag;

use crate::format;
use crate::i18n::{t, t_args};
use crate::indexer::{IndexEvent, IndexStatus, Indexer};

/// Accent used for highlighted matches (the brand orange).
const ACCENT: Color32 = Color32::from_rgb(0xE8, 0x76, 0x2B);
const SEARCH_ID: &str = "fennec-search";

/// Everything the window needs from `main`.
pub struct AppDeps {
    /// Shared index.
    pub shared: SharedIndex,
    /// Search service.
    pub engine: SearchEngine,
    /// Search results.
    pub engine_rx: Receiver<EngineEvent>,
    /// Indexer progress.
    pub index_rx: Receiver<IndexEvent>,
    /// Command lines forwarded by later launches.
    pub commands_rx: Option<Receiver<Vec<String>>>,
    /// Indexer handle (stopped on exit).
    pub indexer: Indexer,
    /// Settings.
    pub settings: Settings,
    /// Where settings are saved.
    pub settings_path: PathBuf,
    /// Initial search text.
    pub initial_search: Option<String>,
    /// Start hidden (tray).
    pub start_hidden: bool,
}

#[derive(Clone)]
struct Row {
    name: String,
    path: String,
    size: String,
    modified: String,
    is_dir: bool,
    offline: bool,
    name_hl: Vec<Range<usize>>,
    path_hl: Vec<Range<usize>>,
}

/// Main application state.
pub struct FennecApp {
    shared: SharedIndex,
    engine: SearchEngine,
    engine_rx: Receiver<EngineEvent>,
    index_rx: Receiver<IndexEvent>,
    commands_rx: Option<Receiver<Vec<String>>>,
    indexer: Indexer,
    settings: Settings,
    settings_path: PathBuf,
    options: Arc<SearchOptions>,

    query: String,
    sent_query: Option<String>,
    edited_at: Option<Instant>,
    next_id: u64,
    latest_shown: u64,
    sort: SortSpec,

    results: Arc<ResultSet>,
    highlighter: Option<Highlighter>,
    diagnostics: Vec<String>,
    elapsed: Duration,
    selected: Option<usize>,
    selected_id: Option<EntryId>,
    scroll_to: Option<usize>,
    rows: HashMap<EntryId, Row>,
    rows_generation: u64,

    status: IndexStatus,
    message: Option<(String, Instant)>,
    focus_search: bool,
    keep_focus: bool,
    hidden: bool,
    #[cfg(windows)]
    integration: Option<crate::tray::Integration>,
}

impl FennecApp {
    /// Builds the app and kicks off the first search.
    pub fn new(cc: &eframe::CreationContext<'_>, deps: AppDeps) -> Self {
        let ctx = &cc.egui_ctx;
        crate::fonts::install_cjk_fallback(ctx.clone());
        apply_theme(ctx, deps.settings.theme);
        if (deps.settings.ui_scale - 1.0).abs() > f32::EPSILON {
            ctx.set_zoom_factor(deps.settings.ui_scale.clamp(0.5, 3.0));
        }
        #[cfg(windows)]
        let integration =
            Some(crate::tray::install(ctx, deps.settings.show_tray_icon, &deps.settings.hotkey_show_window));
        let options = Arc::new(SearchOptions::from_settings(&deps.settings));
        let sort = SortSpec { column: SortColumn::Name, ascending: true, folders_first: deps.settings.folders_first };
        let mut app = Self {
            shared: deps.shared,
            engine: deps.engine,
            engine_rx: deps.engine_rx,
            index_rx: deps.index_rx,
            commands_rx: deps.commands_rx,
            indexer: deps.indexer,
            settings: deps.settings,
            settings_path: deps.settings_path,
            options,
            query: deps.initial_search.unwrap_or_default(),
            sent_query: None,
            edited_at: None,
            next_id: 1,
            latest_shown: 0,
            sort,
            results: Arc::new(ResultSet::default()),
            highlighter: None,
            diagnostics: Vec::new(),
            elapsed: Duration::ZERO,
            selected: None,
            selected_id: None,
            scroll_to: None,
            rows: HashMap::new(),
            rows_generation: 0,
            status: IndexStatus::Starting,
            message: None,
            focus_search: true,
            keep_focus: false,
            hidden: deps.start_hidden,
            #[cfg(windows)]
            integration,
        };
        if app.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        app.send_search();
        app
    }

    fn send_search(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.sent_query = Some(self.query.clone());
        self.edited_at = None;
        self.engine.search(SearchRequest {
            id,
            text: self.query.clone(),
            filter: None,
            sort: self.sort,
            options: self.options.clone(),
        });
    }

    fn on_outcome(&mut self, o: SearchOutcome) {
        if o.id < self.latest_shown {
            return;
        }
        self.latest_shown = o.id;
        self.elapsed = o.elapsed;
        self.highlighter = self.settings.highlight.then(|| Highlighter::new(&o.plan)).filter(|h| !h.is_empty());
        self.diagnostics = diagnostics_text(&o);
        let same_set = Arc::ptr_eq(&self.results, &o.results);
        self.results = o.results;
        if !same_set {
            self.rows.clear();
        }
        // Keep the selection on the same entry if it is still listed.
        self.selected = self.selected_id.and_then(|sid| self.results.ids.iter().position(|&i| i == sid));
        if self.selected.is_none() {
            self.selected_id = None;
        }
    }

    fn drain_channels(&mut self, ctx: &egui::Context) {
        while let Ok(EngineEvent::Done(o)) = self.engine_rx.try_recv() {
            self.on_outcome(o);
        }
        while let Ok(ev) = self.index_rx.try_recv() {
            match ev {
                IndexEvent::Status(s) => self.status = s,
                IndexEvent::Message(m) => self.message = Some((m, Instant::now())),
            }
        }
        let forwarded: Vec<Vec<String>> =
            self.commands_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for args in forwarded {
            let cli = crate::cli::parse(args);
            if cli.exit {
                self.quit(ctx);
                return;
            }
            if let Some(s) = cli.search {
                self.query = s;
                self.send_search();
            }
            self.show_window(ctx);
        }
        #[cfg(windows)]
        {
            let events: Vec<crate::tray::ShellEvent> =
                self.integration.as_ref().map(|i| i.events.try_iter().collect()).unwrap_or_default();
            for e in events {
                match e {
                    crate::tray::ShellEvent::Show => self.show_window(ctx),
                    crate::tray::ShellEvent::Toggle => {
                        if self.hidden {
                            self.show_window(ctx);
                        } else {
                            self.hide_window(ctx);
                        }
                    }
                    crate::tray::ShellEvent::Exit => self.quit(ctx),
                }
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.focus_search = true;
        ctx.request_repaint();
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn can_hide_to_tray(&self) -> bool {
        #[cfg(windows)]
        {
            self.settings.show_tray_icon && self.integration.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.indexer.stop();
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!(error = %e, "cannot save settings");
        }
        self.settings.close_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn selected_path(&self) -> Option<(EntryId, String)> {
        let id = *self.results.ids.get(self.selected?)?;
        let snap = self.shared.try_snapshot_for(Duration::from_millis(5))?;
        if !snap.is_live(id) {
            return None;
        }
        let mut p = String::new();
        snap.full_path(id, &mut p);
        if snap.flags(id).contains(EntryFlags::ROOT) && !p.ends_with(['\\', '/']) {
            p.push(snap.separator(id));
        }
        Some((id, p))
    }

    fn open_selected(&mut self, reveal: bool) {
        let Some((id, path)) = self.selected_path() else { return };
        let p = std::path::Path::new(&path);
        let r = if reveal { fnc_platform::shell::reveal(p) } else { fnc_platform::shell::open(p) };
        match r {
            Ok(()) => {
                if !reveal {
                    self.shared.write().record_run(id);
                    self.engine.refresh();
                }
            }
            Err(e) => {
                let msg = t_args("msg-open-failed", &[("path", path.into()), ("error", e.to_string().into())]);
                self.message = Some((msg, Instant::now()));
            }
        }
    }

    fn copy_selected(&mut self, ctx: &egui::Context, name_only: bool) {
        let Some((_, path)) = self.selected_path() else { return };
        let text = if name_only {
            path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(&path).to_string()
        } else {
            path
        };
        ctx.copy_text(text);
        self.message = Some((t("msg-copied"), Instant::now()));
    }

    fn move_selection(&mut self, delta: isize) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let cur = self.selected.map_or(-1, |s| s as isize);
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.select(next);
    }

    fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.selected_id = self.results.ids.get(i).copied();
        self.scroll_to = Some(i);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (mut down, mut up, mut pgdn, mut pgup, mut home, mut end) = (false, false, false, false, false, false);
        let (mut enter, mut ctrl_enter, mut esc, mut f5, mut focus) = (false, false, false, false, false);
        let (mut copy_path, mut copy_name) = (false, false);
        ctx.input_mut(|i| {
            down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
            up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
            pgdn = i.consume_key(Modifiers::NONE, Key::PageDown);
            pgup = i.consume_key(Modifiers::NONE, Key::PageUp);
            home = i.consume_key(Modifiers::CTRL, Key::Home);
            end = i.consume_key(Modifiers::CTRL, Key::End);
            ctrl_enter = i.consume_key(Modifiers::CTRL, Key::Enter);
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
            esc = i.consume_key(Modifiers::NONE, Key::Escape);
            f5 = i.consume_key(Modifiers::NONE, Key::F5);
            focus = i.consume_key(Modifiers::CTRL, Key::F)
                || i.consume_key(Modifiers::NONE, Key::F3)
                || i.consume_key(Modifiers::ALT, Key::D);
            copy_path = i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::C);
            copy_name = i.consume_key(Modifiers::CTRL | Modifiers::ALT, Key::C);
        });
        let page = 20;
        if down {
            self.move_selection(1);
        }
        if up {
            self.move_selection(-1);
        }
        if pgdn {
            self.move_selection(page);
        }
        if pgup {
            self.move_selection(-page);
        }
        if home && !self.results.is_empty() {
            self.select(0);
        }
        if end && !self.results.is_empty() {
            self.select(self.results.len() - 1);
        }
        if enter {
            self.open_selected(false);
        }
        if ctrl_enter {
            self.open_selected(true);
        }
        if f5 {
            self.engine.refresh();
        }
        if focus {
            self.focus_search = true;
        }
        if copy_path {
            self.copy_selected(ctx, false);
        }
        if copy_name {
            self.copy_selected(ctx, true);
        }
        if esc {
            if self.query.is_empty() {
                if self.can_hide_to_tray() {
                    self.hide_window(ctx);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            } else {
                self.query.clear();
                self.send_search();
            }
            // egui drops text focus on Escape; Everything keeps typing in the search box.
            self.keep_focus = true;
        }
    }

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::new(SEARCH_ID);
        let edit = egui::TextEdit::singleline(&mut self.query)
            .id(id)
            .hint_text(t("search-hint"))
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Body);
        let resp = ui.add(edit);
        if self.keep_focus {
            self.keep_focus = false;
            resp.request_focus();
        }
        if self.focus_search {
            self.focus_search = false;
            resp.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.query.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
        }
        if resp.changed() {
            self.edited_at = Some(Instant::now());
        }
        // `search_delay` (default 0 ms, like Everything).
        if let Some(at) = self.edited_at {
            let delay = Duration::from_millis(u64::from(self.settings.search_delay));
            if at.elapsed() >= delay {
                if self.sent_query.as_deref() != Some(self.query.as_str()) {
                    self.send_search();
                } else {
                    self.edited_at = None;
                }
            } else {
                ui.ctx().request_repaint_after(delay - at.elapsed());
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let n = format::group_digits(self.results.len() as u64);
            ui.label(t_args("status-objects", &[("count", n.into())]));
            if self.selected.is_some() {
                ui.separator();
                ui.label(t_args("status-selected", &[("count", "1".into())]));
            }
            ui.separator();
            ui.label(t_args("status-elapsed", &[("ms", format!("{:.1}", self.elapsed.as_secs_f64() * 1000.0).into())]));
            if let Some(d) = self.diagnostics.first() {
                ui.separator();
                ui.colored_label(ui.visuals().warn_fg_color, d);
            }
            if let Some((m, at)) = &self.message {
                if at.elapsed() < Duration::from_secs(8) {
                    ui.separator();
                    ui.label(m);
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                } else {
                    self.message = None;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(status_text(&self.status));
            });
        });
    }

    fn header_button(&mut self, ui: &mut egui::Ui, label: String, col: SortColumn) {
        let arrow = if self.sort.column == col { if self.sort.ascending { " ▲" } else { " ▼" } } else { "" };
        let resp = ui
            .add(egui::Label::new(egui::RichText::new(format!("{label}{arrow}")).strong()).sense(egui::Sense::click()));
        if resp.clicked() {
            if self.sort.column == col {
                self.sort.ascending = !self.sort.ascending;
            } else {
                // Sizes and dates start descending (largest / newest first).
                self.sort.ascending = matches!(col, SortColumn::Name | SortColumn::Path);
                self.sort.column = col;
            }
            self.send_search();
        }
    }

    fn row(&mut self, id: EntryId, snap: Option<&fnc_index::IndexSnapshot>) -> Option<Row> {
        if let Some(r) = self.rows.get(&id) {
            return Some(r.clone());
        }
        let s = snap?;
        if !s.is_live(id) {
            return None;
        }
        let name = s.name_string(id);
        let mut path = String::new();
        s.parent_path(id, &mut path);
        let is_dir = s.is_dir(id);
        let mut name_hl = Vec::new();
        let mut path_hl = Vec::new();
        if let Some(h) = &self.highlighter {
            h.name_ranges(&name, &mut name_hl);
            h.path_ranges(&path, &mut path_hl);
        }
        let row = Row {
            size: if is_dir { String::new() } else { s.size(id).map(format::size).unwrap_or_default() },
            modified: format::date(s.date_modified(id)),
            offline: s.flags(id).contains(EntryFlags::OFFLINE),
            is_dir,
            name,
            path,
            name_hl,
            path_hl,
        };
        if self.rows.len() > 4096 {
            self.rows.clear();
        }
        self.rows.insert(id, row.clone());
        Some(row)
    }

    fn results_table(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            ui.centered_and_justified(|ui| {
                let text = match self.status {
                    IndexStatus::Starting | IndexStatus::Building { .. } => t("empty-indexing"),
                    _ => t("empty-results"),
                };
                ui.weak(text);
            });
            return;
        }
        // One short-lived snapshot for all visible rows; never wait more than 2 ms.
        let snap = self.shared.try_snapshot_for(Duration::from_millis(2));
        if let Some(s) = &snap
            && s.generation() != self.rows_generation
        {
            self.rows_generation = s.generation();
            self.rows.clear();
        }
        let text_h = ui.text_style_height(&egui::TextStyle::Body);
        let row_h = if self.settings.row_height > 0 {
            self.settings.row_height as f32
        } else {
            (text_h + 6.0).max(ui.spacing().interact_size.y)
        };
        let results = self.results.clone();
        let mut clicked: Option<(usize, RowAction)> = None;
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(300.0).at_least(80.0).clip(true))
            .column(Column::initial(420.0).at_least(80.0).clip(true))
            .column(Column::initial(90.0).at_least(50.0))
            .column(Column::remainder().at_least(110.0));
        if let Some(i) = self.scroll_to.take() {
            builder = builder.scroll_to_row(i, None);
        }
        builder
            .header(text_h + 8.0, |mut header| {
                header.col(|ui| self.header_button(ui, t("col-name"), SortColumn::Name));
                header.col(|ui| self.header_button(ui, t("col-path"), SortColumn::Path));
                header.col(|ui| self.header_button(ui, t("col-size"), SortColumn::Size));
                header.col(|ui| self.header_button(ui, t("col-modified"), SortColumn::DateModified));
            })
            .body(|body| {
                body.rows(row_h, results.len(), |mut row| {
                    let i = row.index();
                    let id = results.ids[i];
                    row.set_selected(self.selected == Some(i));
                    let data = self.row(id, snap.as_ref());
                    let weak = ui_weak_color(&data);
                    row.col(|ui| match &data {
                        Some(r) => {
                            let icon = if r.is_dir { "🗀 " } else { "🗋 " };
                            ui.label(highlighted(ui, icon, &r.name, &r.name_hl, weak));
                        }
                        None => {
                            ui.weak("…");
                        }
                    });
                    row.col(|ui| {
                        if let Some(r) = &data {
                            ui.label(highlighted(ui, "", &r.path, &r.path_hl, weak));
                        }
                    });
                    row.col(|ui| {
                        if let Some(r) = &data {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(&r.size);
                            });
                        }
                    });
                    row.col(|ui| {
                        if let Some(r) = &data {
                            ui.label(&r.modified);
                        }
                    });
                    let resp = row.response();
                    if resp.double_clicked() {
                        clicked = Some((i, RowAction::Open));
                    } else if resp.middle_clicked() {
                        clicked = Some((i, RowAction::Reveal));
                    } else if resp.clicked() {
                        clicked = Some((i, RowAction::Select));
                    }
                });
            });
        drop(snap);
        if let Some((i, action)) = clicked {
            self.selected = Some(i);
            self.selected_id = results.ids.get(i).copied();
            // Keep typing in the search box after clicking a row.
            self.keep_focus = true;
            match action {
                RowAction::Select => {}
                RowAction::Open => self.open_selected(false),
                RowAction::Reveal => self.open_selected(true),
            }
        }
    }
}

/// Mouse action on a result row.
#[derive(Clone, Copy)]
enum RowAction {
    Select,
    /// Double click.
    Open,
    /// Middle click: open the containing folder.
    Reveal,
}

fn ui_weak_color(row: &Option<Row>) -> bool {
    row.as_ref().is_some_and(|r| r.offline)
}

fn highlighted(ui: &egui::Ui, prefix: &str, text: &str, ranges: &[Range<usize>], weak: bool) -> LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let normal = if weak { ui.visuals().weak_text_color() } else { ui.visuals().text_color() };
    let fmt = |color| TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = LayoutJob::default();
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    if !prefix.is_empty() {
        job.append(prefix, 0.0, fmt(ui.visuals().weak_text_color()));
    }
    let mut pos = 0;
    for r in ranges {
        let (a, b) = (r.start.min(text.len()), r.end.min(text.len()));
        if a < pos || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        job.append(&text[pos..a], 0.0, fmt(normal));
        job.append(&text[a..b], 0.0, fmt(ACCENT));
        pos = b;
    }
    job.append(&text[pos..], 0.0, fmt(normal));
    job
}

fn status_text(s: &IndexStatus) -> String {
    match s {
        IndexStatus::Starting => t("index-starting"),
        IndexStatus::Building { volume, done } => {
            t_args("index-building", &[("volume", volume.clone().into()), ("done", format::group_digits(*done).into())])
        }
        IndexStatus::Metadata { percent } => t_args("index-meta", &[("percent", u32::from(*percent).into())]),
        IndexStatus::Ready { entries, seconds } => t_args(
            "index-ready",
            &[("count", format::group_digits(*entries as u64).into()), ("seconds", format!("{seconds:.1}").into())],
        ),
    }
}

fn diagnostics_text(o: &SearchOutcome) -> Vec<String> {
    let mut v = Vec::new();
    for d in &o.parse_diagnostics {
        match d.kind {
            DiagKind::UnclosedQuote => v.push(t("diag-quote")),
            DiagKind::UnclosedGroup => v.push(t("diag-group")),
            _ => {}
        }
    }
    for d in &o.plan_diagnostics {
        match d {
            PlanDiag::Unsupported(f) => v.push(t_args("diag-unsupported", &[("name", f.name().into())])),
            PlanDiag::InvalidValue(f, val) => {
                v.push(t_args("diag-invalid", &[("name", f.name().into()), ("value", val.clone().into())]))
            }
        }
    }
    for e in &o.results.stats.errors {
        v.push(t_args("diag-pattern", &[("message", e.clone().into())]));
    }
    v
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
}

impl eframe::App for FennecApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_channels(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.settings.close_to_tray && self.can_hide_to_tray() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        egui::Panel::top("search").show(ui, |ui| {
            ui.add_space(4.0);
            self.search_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.results_table(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.indexer.stop();
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!(error = %e, "cannot save settings");
        }
    }
}

#[cfg(test)]
mod tests;

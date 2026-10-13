//! The main window (SPEC ch.3): menu bar, search box with completion and filter
//! selector, filter bar, the virtualised result list, status bar, dialogs and the
//! Options window.
//!
//! The UI thread never blocks: searches run on the engine thread, index snapshots are
//! taken with short timeouts (falling back to cached rows), only visible rows are
//! formatted, and file operations run on worker threads. Modal shell loops (context
//! menu, drag & drop) run on the UI thread as the OS requires.

mod complete;
mod dialogs;
mod menus;
mod options;
mod table;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Key};
use fnc_core::settings::{Settings, SettingsMeta, Theme};
use fnc_core::{EntryFlags, EntryId};
use fnc_exec::{
    EngineEvent, Highlighter, ResultSet, SearchEngine, SearchOptions, SearchOutcome, SearchRequest, SortColumn,
    SortSpec,
};
use fnc_index::SharedIndex;
use fnc_platform::fileops::Owner;
use fnc_platform::instance::Primary;
use fnc_query::DiagKind;
use fnc_query::plan::PlanDiag;

use crate::columns::ColumnState;
use crate::commands::{Chord, Command, KeyCtx, Keymap};
use crate::filters::{Bookmarks, Filters};
use crate::format;
use crate::history::SearchHistory;
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
    /// Single-instance ownership (mutex + command pipe); kept alive for the process
    /// lifetime. Forwarded command lines arrive on it.
    pub instance: Option<Primary>,
    /// Indexer handle (stopped on exit).
    pub indexer: Indexer,
    /// Settings.
    pub settings: Settings,
    /// Where settings are saved (filters, bookmarks and history live next to it).
    pub settings_path: PathBuf,
    /// Initial search text.
    pub initial_search: Option<String>,
    /// Start hidden (tray).
    pub start_hidden: bool,
}

#[derive(Clone)]
struct Row {
    /// One text per visible column.
    cells: Vec<String>,
    is_dir: bool,
    offline: bool,
    name_hl: Vec<Range<usize>>,
    path_hl: Vec<Range<usize>>,
}

/// Multi-selection over the current results (by entry, so it survives re-sorting).
#[derive(Default, Debug)]
struct Selection {
    ids: HashSet<EntryId>,
    /// Ctrl+A: every result (without materialising a million ids).
    all: bool,
    /// Range start for Shift+click / Shift+arrows.
    anchor: Option<usize>,
}

impl Selection {
    fn contains(&self, id: EntryId) -> bool {
        self.all || self.ids.contains(&id)
    }

    fn clear(&mut self) {
        self.ids.clear();
        self.all = false;
    }

    fn count(&self, total: usize) -> usize {
        if self.all { total } else { self.ids.len() }
    }
}

/// Which widget receives keys.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
}

/// In-place rename (F2).
struct RenameState {
    id: EntryId,
    path: PathBuf,
    text: String,
    focus: bool,
}

/// Result of a background file operation.
enum WorkerMsg {
    Done(String),
    Failed(String),
}

/// Main application state.
pub struct FennecApp {
    shared: SharedIndex,
    engine: SearchEngine,
    engine_rx: Receiver<EngineEvent>,
    index_rx: Receiver<IndexEvent>,
    instance: Option<Primary>,
    indexer: Indexer,
    settings: Settings,
    settings_path: PathBuf,
    search_opts: Arc<SearchOptions>,
    keymap: Keymap,

    query: String,
    sent_query: Option<String>,
    edited_at: Option<Instant>,
    next_id: u64,
    latest_shown: u64,
    sort: SortSpec,
    columns: Vec<ColumnState>,

    results: Arc<ResultSet>,
    highlighter: Option<Highlighter>,
    diagnostics: Vec<String>,
    elapsed: Duration,
    /// Focused (cursor) row.
    selected: Option<usize>,
    selected_id: Option<EntryId>,
    sel: Selection,
    scroll_to: Option<usize>,
    rows: HashMap<EntryId, Row>,
    rows_generation: u64,
    /// Shell type names (`type:` and the Type column), shared with the engine.
    type_names: fnc_exec::TypeNames,
    /// Type column texts per `(extension, is_dir)`.
    type_cache: HashMap<(String, bool), String>,
    omit: HashSet<EntryId>,

    filters: Filters,
    /// Active filters (indices into `filters.list`; empty = Everything).
    active_filters: Vec<usize>,
    bookmarks: Bookmarks,
    history: SearchHistory,
    /// The current search was recorded in the history.
    history_recorded: bool,
    /// When the current search was sent (idle searches enter the history).
    history_idle_since: Option<Instant>,
    complete: complete::CompleteState,

    status: IndexStatus,
    message: Option<(String, Instant)>,
    focus: Focus,
    focus_search: bool,
    keep_focus: bool,
    hidden: bool,
    rename: Option<RenameState>,
    dialog: Option<dialogs::Dialog>,
    options_window: Option<options::OptionsState>,
    /// Commands queued by menus, the tray and widgets (run after drawing).
    pending: Vec<Command>,
    /// Deferred list actions (context menu, drag out) that need the drawn frame.
    after: Vec<table::After>,
    /// Native window handle (shell menus, dialogs, drag & drop).
    hwnd: isize,
    /// Alt pressed while the menu bar is hidden: show it temporarily.
    menu_peek: bool,
    /// Last known window rectangle (outer position, inner size) and maximised state.
    window_geometry: Option<(egui::Rect, bool)>,

    worker_tx: Sender<WorkerMsg>,
    worker_rx: Receiver<WorkerMsg>,
    /// Exiting for real (tray "Exit", `-exit`): skip close-to-tray.
    quitting: bool,
    /// Last tray toggle, to debounce double clicks.
    #[cfg(windows)]
    last_toggle: Option<Instant>,
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
        let filters = Filters::load(Some(crate::filters::data_file(&deps.settings_path, "filters.toml")));
        let bookmarks = Bookmarks::load(Some(crate::filters::data_file(&deps.settings_path, "bookmarks.toml")));
        let history = if deps.settings.search_history_enabled {
            SearchHistory::load(
                crate::filters::data_file(&deps.settings_path, "search_history.toml"),
                deps.settings.search_history_max as usize,
            )
        } else {
            SearchHistory::in_memory(0)
        };
        let (keymap, bad_keys) = Keymap::with_overrides(&deps.settings.key_bindings);
        for b in bad_keys {
            tracing::warn!(binding = %b, "ignoring invalid key binding");
        }
        let active_filters: Vec<usize> = filters.find(&deps.settings.filter).into_iter().collect();
        let sort = sort_from_settings(&deps.settings);
        let columns = crate::columns::parse_columns(&deps.settings.columns);
        let (worker_tx, worker_rx) = crossbeam_channel::unbounded();
        if deps.settings.pause_updates {
            deps.indexer.set_paused(true);
        }
        let mut app = Self {
            shared: deps.shared,
            engine: deps.engine,
            engine_rx: deps.engine_rx,
            index_rx: deps.index_rx,
            instance: deps.instance,
            indexer: deps.indexer,
            search_opts: Arc::new(SearchOptions::default()),
            keymap,
            query: deps.initial_search.unwrap_or_default(),
            sent_query: None,
            edited_at: None,
            next_id: 1,
            latest_shown: 0,
            sort,
            columns,
            results: Arc::new(ResultSet::default()),
            highlighter: None,
            diagnostics: Vec::new(),
            elapsed: Duration::ZERO,
            selected: None,
            selected_id: None,
            sel: Selection::default(),
            scroll_to: None,
            rows: HashMap::new(),
            rows_generation: 0,
            type_names: fnc_exec::TypeNames::new(|ext| fnc_platform::shell::type_name(ext, false)),
            type_cache: HashMap::new(),
            omit: HashSet::new(),
            filters,
            active_filters,
            bookmarks,
            history,
            history_recorded: true,
            history_idle_since: None,
            complete: complete::CompleteState::default(),
            status: IndexStatus::Starting,
            message: None,
            focus: Focus::Search,
            focus_search: true,
            keep_focus: false,
            // Start-hidden is applied by `main` via `ViewportBuilder::with_visible(false)`
            // (no flash of the window).
            hidden: deps.start_hidden,
            rename: None,
            dialog: None,
            options_window: None,
            pending: Vec::new(),
            after: Vec::new(),
            hwnd: 0,
            menu_peek: false,
            window_geometry: None,
            worker_tx,
            worker_rx,
            quitting: false,
            settings: deps.settings,
            settings_path: deps.settings_path,
            #[cfg(windows)]
            last_toggle: None,
            #[cfg(windows)]
            integration,
        };
        app.rebuild_search_options();
        app.send_search();
        app
    }

    // ------------------------------------------------------------------ searching

    /// Search options from settings plus the filters' macros, omitted results and the
    /// shell's type names.
    fn rebuild_search_options(&mut self) {
        let mut o = SearchOptions::from_settings(&self.settings);
        o.macros.extend(self.filters.macros());
        o.type_names = Some(self.type_names.clone());
        if !self.omit.is_empty() {
            let instance = self.shared.try_snapshot_for(Duration::from_millis(20)).map_or(0, |s| s.instance());
            o.omit = Some(fnc_query::plan::OmitSet { instance, ids: self.omit.iter().copied().collect() });
        }
        self.search_opts = Arc::new(o);
    }

    /// The active filters with their own match options (SPEC ch.2 §7).
    fn filter_set(&self) -> Option<fnc_query::FilterSet> {
        let filters: Vec<fnc_query::Filter> = self
            .active_filters
            .iter()
            .filter_map(|&i| self.filters.list.get(i))
            .map(crate::filters::Filter::to_query)
            .collect();
        let set = fnc_query::FilterSet { filters, and: false };
        (!set.is_empty()).then_some(set)
    }

    fn send_search(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        if self.sent_query.as_deref() != Some(self.query.as_str()) && !self.omit.is_empty() {
            self.omit.clear();
            self.rebuild_search_options();
        }
        if self.sent_query.as_deref() != Some(self.query.as_str()) {
            self.history_recorded = false;
            self.history_idle_since = Some(Instant::now());
        }
        self.sent_query = Some(self.query.clone());
        self.edited_at = None;
        self.engine.search(SearchRequest {
            id,
            text: self.query.clone(),
            filter: self.filter_set(),
            sort: self.sort,
            options: self.search_opts.clone(),
        });
    }

    fn set_query(&mut self, q: String) {
        self.query = q;
        self.complete.close();
        self.send_search();
    }

    fn record_history(&mut self) {
        if !self.history_recorded && self.settings.search_history_enabled {
            self.history.record(&self.query);
            self.history_recorded = true;
        }
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
        // Keep the cursor on the same entry if it is still listed.
        self.selected = self.selected_id.and_then(|sid| self.results.ids.iter().position(|&i| i == sid));
        if self.selected.is_none() {
            self.selected_id = None;
        }
        self.sel.all = false;
        // Deleted or no longer matching entries leave the selection.
        if !self.sel.ids.is_empty() {
            let listed: HashSet<EntryId> = self.results.ids.iter().copied().collect();
            self.sel.ids.retain(|id| listed.contains(id));
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
        while let Ok(m) = self.worker_rx.try_recv() {
            let text = match m {
                WorkerMsg::Done(s) | WorkerMsg::Failed(s) => s,
            };
            self.message = Some((text, Instant::now()));
        }
        let forwarded: Vec<Vec<String>> =
            self.instance.as_ref().map(|p| p.commands().try_iter().collect()).unwrap_or_default();
        for args in forwarded {
            let cli = crate::cli::parse(args);
            if cli.exit {
                self.quit(ctx);
                return;
            }
            if !cli.file_lists.is_empty() || !cli.folders.is_empty() {
                // Index sources are fixed at start-up.
                tracing::warn!("forwarded -filelist/-folder ignored by the running instance");
                self.message = Some((t("msg-restart-required"), Instant::now()));
            }
            if let Some(s) = cli.search {
                self.set_query(s);
            }
            // A second `-startup` launch (e.g. autostart) must not pop the window up.
            if !cli.start_hidden {
                self.show_window(ctx);
            }
        }
        #[cfg(windows)]
        {
            let events: Vec<crate::tray::ShellEvent> =
                self.integration.as_ref().map(|i| i.events.try_iter().collect()).unwrap_or_default();
            for e in events {
                match e {
                    crate::tray::ShellEvent::Show => self.show_window(ctx),
                    crate::tray::ShellEvent::Toggle => {
                        // The second click of a double click must not undo the first.
                        if self.last_toggle.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
                            continue;
                        }
                        self.last_toggle = Some(Instant::now());
                        if self.hidden {
                            self.show_window(ctx);
                        } else {
                            self.hide_window(ctx);
                        }
                    }
                    crate::tray::ShellEvent::Options => {
                        self.show_window(ctx);
                        self.pending.push(Command::Options);
                    }
                    crate::tray::ShellEvent::TogglePause => self.toggle_pause(),
                    crate::tray::ShellEvent::Exit => self.quit(ctx),
                }
            }
        }
    }

    fn toggle_pause(&mut self) {
        let paused = !self.indexer.paused();
        self.indexer.set_paused(paused);
        self.settings.pause_updates = paused;
        let key = if paused { "msg-updates-paused" } else { "msg-updates-resumed" };
        self.say(t(key));
    }

    // ------------------------------------------------------------------ window

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
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn say(&mut self, text: String) {
        self.message = Some((text, Instant::now()));
    }

    fn owner(&self) -> Owner {
        Owner(self.hwnd)
    }

    // ------------------------------------------------------------------ selection

    /// Entries the selection-based commands act on: the selection, or the cursor row.
    fn target_ids(&self) -> Vec<EntryId> {
        if self.sel.all {
            return self.results.ids.clone();
        }
        if !self.sel.ids.is_empty() {
            // In result order (stable for copy-as-text).
            return self.results.ids.iter().copied().filter(|id| self.sel.ids.contains(id)).collect();
        }
        self.selected.and_then(|i| self.results.ids.get(i).copied()).into_iter().collect()
    }

    /// Full paths of the target entries (roots get their trailing separator).
    fn target_paths(&self) -> Vec<(EntryId, PathBuf)> {
        let ids = self.target_ids();
        if ids.is_empty() {
            return Vec::new();
        }
        let Some(snap) = self.shared.try_snapshot_for(Duration::from_millis(50)) else { return Vec::new() };
        let mut buf = String::new();
        ids.into_iter()
            .filter(|&id| snap.is_live(id))
            .map(|id| {
                snap.full_path(id, &mut buf);
                if snap.flags(id).contains(EntryFlags::ROOT) && !buf.ends_with(['\\', '/']) {
                    buf.push(snap.separator(id));
                }
                (id, PathBuf::from(&buf))
            })
            .collect()
    }

    fn select_only(&mut self, i: usize) {
        self.sel.clear();
        if let Some(&id) = self.results.ids.get(i) {
            self.sel.ids.insert(id);
        }
        self.sel.anchor = Some(i);
        self.set_cursor(i);
    }

    fn set_cursor(&mut self, i: usize) {
        self.selected = Some(i);
        self.selected_id = self.results.ids.get(i).copied();
        self.scroll_to = Some(i);
    }

    fn select_range(&mut self, to: usize) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let from = self.sel.anchor.or(self.selected).unwrap_or(to).min(n - 1);
        let (a, b) = (from.min(to), from.max(to).min(n - 1));
        self.sel.all = false;
        self.sel.ids = self.results.ids[a..=b].iter().copied().collect();
        self.sel.anchor = Some(from);
        self.set_cursor(to);
    }

    fn move_cursor(&mut self, delta: isize, mode: MoveMode) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let cur = self.selected.map_or(-1, |s| s as isize);
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        match mode {
            MoveMode::Select => self.select_only(next),
            MoveMode::Extend => self.select_range(next),
            MoveMode::FocusOnly => self.set_cursor(next),
        }
    }

    // ------------------------------------------------------------------ commands

    fn run(&mut self, ctx: &egui::Context, cmd: Command) {
        use Command as K;
        let page = 20;
        match cmd {
            K::FocusSearch => self.focus_search = true,
            K::Refresh => self.engine.refresh(),
            K::MoveDown => self.move_cursor(1, MoveMode::Select),
            K::MoveUp => self.move_cursor(-1, MoveMode::Select),
            K::ExtendDown => self.move_cursor(1, MoveMode::Extend),
            K::ExtendUp => self.move_cursor(-1, MoveMode::Extend),
            K::FocusDown => self.move_cursor(1, MoveMode::FocusOnly),
            K::FocusUp => self.move_cursor(-1, MoveMode::FocusOnly),
            K::PageDown => self.move_cursor(page, MoveMode::Select),
            K::PageUp => self.move_cursor(-page, MoveMode::Select),
            K::First => {
                if !self.results.is_empty() {
                    self.select_only(0);
                }
            }
            K::Last => {
                if !self.results.is_empty() {
                    self.select_only(self.results.len() - 1);
                }
            }
            K::SelectAll => {
                self.sel.all = !self.results.is_empty();
                self.sel.ids.clear();
            }
            K::InvertSelection => {
                let all = self.sel.all;
                let cur: HashSet<EntryId> = std::mem::take(&mut self.sel.ids);
                self.sel.all = false;
                if !all {
                    self.sel.ids = self.results.ids.iter().copied().filter(|id| !cur.contains(id)).collect();
                }
            }
            K::ToggleSelection => {
                if let Some(id) = self.selected.and_then(|i| self.results.ids.get(i).copied()) {
                    if self.sel.all {
                        self.sel.all = false;
                        self.sel.ids = self.results.ids.iter().copied().collect();
                    }
                    if !self.sel.ids.remove(&id) {
                        self.sel.ids.insert(id);
                    }
                }
            }
            K::Open => self.open_targets(false),
            K::OpenPath => self.open_targets(true),
            K::OpenWith => self.shell_single(fnc_platform::fileops::open_with),
            K::RunAsAdmin => {
                if let Some((_, p)) = self.target_paths().into_iter().next()
                    && let Err(e) = fnc_platform::fileops::run_as_admin(&p)
                {
                    self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
                }
            }
            K::Properties => self.shell_single(fnc_platform::fileops::properties),
            K::ContextMenu => self.context_menu(ctx),
            K::CopyFiles | K::CutFiles => {
                let paths: Vec<PathBuf> = self.target_paths().into_iter().map(|(_, p)| p).collect();
                if !paths.is_empty() {
                    match fnc_platform::fileops::set_clipboard_files(&paths, cmd == K::CutFiles) {
                        Ok(()) => self.say(t_args("msg-copied-files", &[("count", paths.len().into())])),
                        // No file clipboard on this platform: copy the paths as text.
                        Err(e) => {
                            tracing::warn!(error = %e, "cannot put files on the clipboard; copying paths");
                            self.copy_text(ctx, TextKind::FullPath);
                        }
                    }
                }
            }
            K::Paste => {
                if let Some(dest) = self.paste_target() {
                    let owner = self.owner();
                    self.spawn_op(t("msg-pasted"), move || fnc_platform::fileops::paste_files(owner, &dest));
                }
            }
            K::CopyFullPath => self.copy_text(ctx, TextKind::FullPath),
            K::CopyFullPathQuoted => self.copy_text(ctx, TextKind::Quoted),
            K::CopyName => self.copy_text(ctx, TextKind::Name),
            K::CopyPath => self.copy_text(ctx, TextKind::Folder),
            K::CopyAsTsv => self.copy_text(ctx, TextKind::Tsv),
            K::CopyAsCsv => self.copy_text(ctx, TextKind::Csv),
            K::CopyAsEfu => self.copy_text(ctx, TextKind::Efu),
            K::Delete => {
                let paths: Vec<PathBuf> = self.target_paths().into_iter().map(|(_, p)| p).collect();
                self.delete_paths(paths, false);
            }
            K::DeletePermanently => {
                // Permanent deletion cannot be undone: always ask (SPEC ch.3 §4.2).
                let paths: Vec<PathBuf> = self.target_paths().into_iter().map(|(_, p)| p).collect();
                if !paths.is_empty() {
                    self.dialog = Some(dialogs::Dialog::ConfirmDelete { paths });
                }
            }
            K::Rename => self.start_rename(),
            K::NewFolder => {
                if let Some(parent) = self.paste_target() {
                    self.dialog = Some(dialogs::Dialog::NewFolder { parent, name: t("new-folder-name") });
                }
            }
            K::ExploreInFennec => {
                if let Some((id, p)) = self.target_paths().into_iter().next() {
                    let is_dir = self.shared.try_snapshot_for(Duration::from_millis(5)).is_some_and(|s| s.is_dir(id));
                    let folder = if is_dir { p.clone() } else { p.parent().map(PathBuf::from).unwrap_or(p) };
                    let mut s = folder.display().to_string();
                    if !s.ends_with(['\\', '/']) {
                        s.push(std::path::MAIN_SEPARATOR);
                    }
                    self.set_query(format!("\"{s}\" "));
                    self.focus_search = true;
                }
            }
            K::Omit => {
                let ids = self.target_ids();
                if !ids.is_empty() {
                    // Hidden until the search text changes (`omit:` lists them).
                    self.omit.extend(ids);
                    self.sel.clear();
                    self.rebuild_search_options();
                    self.send_search();
                }
            }
            K::SetRunCount => {
                if !self.target_ids().is_empty() {
                    self.dialog = Some(dialogs::Dialog::SetRunCount { text: "0".into() });
                }
            }
            K::ToggleMatchCase => self.toggle_setting(|s| &mut s.match_case),
            K::ToggleWholeWord => self.toggle_setting(|s| &mut s.match_whole_word),
            K::ToggleMatchPath => self.toggle_setting(|s| &mut s.match_path),
            K::ToggleDiacritics => self.toggle_setting(|s| &mut s.match_diacritics),
            K::ToggleRegex => self.toggle_setting(|s| &mut s.match_regex),
            K::AdvancedSearch => self.dialog = Some(dialogs::Dialog::Builder(Box::default())),
            K::AddBookmark => {
                self.dialog = Some(dialogs::Dialog::EditBookmark {
                    index: None,
                    bookmark: crate::filters::Bookmark {
                        name: self.query.clone(),
                        search: self.query.clone(),
                        filter: self.active_filter_name(),
                        sort: crate::columns::sort_setting(&self.sort),
                        ..Default::default()
                    },
                })
            }
            K::OrganizeBookmarks => self.dialog = Some(dialogs::Dialog::Bookmarks { selected: None }),
            K::OrganizeFilters => self.dialog = Some(dialogs::Dialog::Filters { selected: None }),
            K::AddFilter => {
                self.dialog = Some(dialogs::Dialog::EditFilter {
                    index: None,
                    filter: crate::filters::Filter {
                        name: self.query.clone(),
                        search: self.query.clone(),
                        ..Default::default()
                    },
                })
            }
            K::FilterNone => self.set_filters(Vec::new()),
            K::Filter1
            | K::Filter2
            | K::Filter3
            | K::Filter4
            | K::Filter5
            | K::Filter6
            | K::Filter7
            | K::Filter8
            | K::Filter9 => {
                if let Some(slot) = cmd.filter_slot()
                    && slot <= self.filters.list.len()
                {
                    self.set_filters(vec![slot - 1]);
                }
            }
            K::Options => {
                if self.options_window.is_none() {
                    self.options_window = Some(options::OptionsState::new(&self.settings, &self.keymap));
                }
            }
            K::Export => self.dialog = Some(dialogs::Dialog::Export { path: default_export_path() }),
            K::ToggleMenuBar => self.settings.show_menu_bar = !self.settings.show_menu_bar,
            K::ToggleStatusBar => self.settings.show_status_bar = !self.settings.show_status_bar,
            K::ToggleFilterBar => self.settings.show_filter_bar = !self.settings.show_filter_bar,
            K::ThemeLight | K::ThemeDark | K::ThemeSystem => {
                self.settings.theme = match cmd {
                    K::ThemeLight => Theme::Light,
                    K::ThemeDark => Theme::Dark,
                    _ => Theme::System,
                };
                apply_theme(ctx, self.settings.theme);
            }
            K::ZoomIn | K::ZoomOut | K::ZoomReset => {
                let z = match cmd {
                    K::ZoomIn => self.settings.ui_scale + 0.1,
                    K::ZoomOut => self.settings.ui_scale - 0.1,
                    _ => 1.0,
                };
                self.settings.ui_scale = (z * 10.0).round().clamp(5.0, 30.0) / 10.0;
                ctx.set_zoom_factor(self.settings.ui_scale);
            }
            K::SearchHistory => {
                self.focus_search = true;
                self.complete.open_history(&self.history);
            }
            K::ClearHistory => {
                self.history.clear();
                self.history.save();
                self.complete.close();
                self.say(t("msg-history-cleared"));
            }
            K::InstallService | K::UninstallService => {
                let action = if cmd == K::InstallService {
                    crate::cli::ServiceAction::Install
                } else {
                    crate::cli::ServiceAction::Uninstall
                };
                match crate::service_command(action, "") {
                    Ok(()) => self.say(t("msg-service-requested")),
                    Err(e) => self.say(t_args("msg-op-failed", &[("error", e.to_string().into())])),
                }
            }
            K::SaveDatabase => {
                self.indexer.save();
                self.say(t("msg-db-saved"));
            }
            K::PauseUpdates => self.toggle_pause(),
            K::Help => self.dialog = Some(dialogs::Dialog::Help),
            K::About => self.dialog = Some(dialogs::Dialog::About),
            K::HideWindow => {
                if self.can_hide_to_tray() {
                    self.hide_window(ctx);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            }
            K::Exit => self.quit(ctx),
        }
    }

    /// Deletes `paths` on a worker thread (recycle bin unless `permanent`).
    fn delete_paths(&mut self, paths: Vec<PathBuf>, permanent: bool) {
        if paths.is_empty() {
            return;
        }
        let owner = self.owner();
        let n = paths.len();
        self.spawn_op(t_args("msg-deleted", &[("count", n.into())]), move || {
            fnc_platform::fileops::delete(owner, &paths, permanent).map(|_| ())
        });
    }

    fn toggle_setting(&mut self, f: impl Fn(&mut Settings) -> &mut bool) {
        let v = f(&mut self.settings);
        *v = !*v;
        self.rebuild_search_options();
        self.send_search();
    }

    fn active_filter_name(&self) -> String {
        self.active_filters.first().and_then(|&i| self.filters.list.get(i)).map(|f| f.name.clone()).unwrap_or_default()
    }

    fn set_filters(&mut self, active: Vec<usize>) {
        self.active_filters = active;
        self.settings.filter = self.active_filter_name();
        self.rebuild_search_options();
        self.send_search();
    }

    /// Runs `op` on a worker thread (file operations may show dialogs and take long).
    fn spawn_op(&mut self, done: String, op: impl FnOnce() -> std::io::Result<()> + Send + 'static) {
        let tx = self.worker_tx.clone();
        let r = std::thread::Builder::new().name("fennec-fileop".into()).spawn(move || {
            let msg = match op() {
                Ok(()) => WorkerMsg::Done(done),
                Err(e) => WorkerMsg::Failed(t_args("msg-op-failed", &[("error", e.to_string().into())])),
            };
            let _ = tx.send(msg);
        });
        if let Err(e) = r {
            self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
        }
    }

    fn shell_single(&mut self, f: fn(Owner, &std::path::Path) -> std::io::Result<()>) {
        if let Some((_, p)) = self.target_paths().into_iter().next()
            && let Err(e) = f(self.owner(), &p)
        {
            self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
        }
    }

    /// Folder for paste / new folder: the selected folder, else the selection's parent.
    fn paste_target(&self) -> Option<PathBuf> {
        let (id, p) = self.target_paths().into_iter().next()?;
        let is_dir = self.shared.try_snapshot_for(Duration::from_millis(5))?.is_dir(id);
        if is_dir { Some(p) } else { p.parent().map(PathBuf::from) }
    }

    fn open_targets(&mut self, reveal: bool) {
        let targets = self.target_paths();
        // Opening many files at once is almost always a mistake.
        for (id, path) in targets.into_iter().take(if reveal { 1 } else { 32 }) {
            let r = if reveal { fnc_platform::shell::reveal(&path) } else { fnc_platform::shell::open(&path) };
            match r {
                Ok(()) => {
                    self.record_history();
                    if !reveal && self.settings.run_history_enabled {
                        // The write lock may wait for a running search: never take it on
                        // the UI thread.
                        let (shared, engine) = (self.shared.clone(), self.engine.handle());
                        let _ = std::thread::Builder::new().name("fennec-run-count".into()).spawn(move || {
                            shared.write().record_run(id);
                            engine.refresh();
                        });
                    }
                }
                Err(e) => {
                    let msg = t_args(
                        "msg-open-failed",
                        &[("path", path.display().to_string().into()), ("error", e.to_string().into())],
                    );
                    self.say(msg);
                }
            }
        }
    }

    fn copy_text(&mut self, ctx: &egui::Context, kind: TextKind) {
        let targets = self.target_paths();
        if targets.is_empty() {
            return;
        }
        let snap = self.shared.try_snapshot_for(Duration::from_millis(50));
        let mut out = String::new();
        if matches!(kind, TextKind::Csv | TextKind::Efu) {
            out.push_str("Filename,Size,Date Modified,Date Created,Attributes\r\n");
        }
        for (id, p) in &targets {
            let full = p.display().to_string();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| full.clone());
            let folder = p.parent().map(|f| f.display().to_string()).unwrap_or_default();
            let line = match kind {
                TextKind::FullPath => full,
                TextKind::Quoted => format!("\"{full}\""),
                TextKind::Name => name,
                TextKind::Folder => folder,
                TextKind::Tsv | TextKind::Csv | TextKind::Efu => {
                    let (size, dm, dc, attr) = snap.as_ref().map_or((String::new(), 0, 0, 0), |s| {
                        (
                            s.size(*id).filter(|_| !s.is_dir(*id)).map(|v| v.to_string()).unwrap_or_default(),
                            s.date_modified(*id).0,
                            s.date_created(*id).0,
                            s.attributes(*id),
                        )
                    });
                    match kind {
                        TextKind::Tsv => format!("{name}\t{folder}\t{size}"),
                        _ => format!("\"{}\",{size},{dm},{dc},{attr}", full.replace('"', "\"\"")),
                    }
                }
            };
            out.push_str(&line);
            out.push_str("\r\n");
        }
        if targets.len() == 1 && !matches!(kind, TextKind::Csv | TextKind::Efu) {
            out.truncate(out.trim_end().len());
        }
        ctx.copy_text(out);
        self.say(t("msg-copied"));
    }

    fn start_rename(&mut self) {
        let Some((id, path)) = self.target_paths().into_iter().next() else { return };
        let text = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        if let Some(i) = self.results.ids.iter().position(|&r| r == id) {
            self.scroll_to = Some(i);
        }
        self.rename = Some(RenameState { id, path, text, focus: true });
    }

    fn commit_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        let old = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let new = r.text.trim().to_string();
        if new.is_empty() || new == old {
            return;
        }
        let owner = self.owner();
        let path = r.path;
        self.spawn_op(t_args("msg-renamed", &[("name", new.clone().into())]), move || {
            fnc_platform::fileops::rename(owner, &path, &new)
        });
    }

    fn context_menu(&mut self, ctx: &egui::Context) {
        let targets = self.target_paths();
        if targets.is_empty() {
            return;
        }
        let items = menus::shell_menu_items();
        let paths: Vec<PathBuf> = targets.iter().map(|(_, p)| p.clone()).collect();
        // The shell menu needs items of one folder; mixed selections use the first item's.
        let parent = paths[0].parent().map(PathBuf::from);
        let same: Vec<PathBuf> = paths.iter().filter(|p| p.parent().map(PathBuf::from) == parent).cloned().collect();
        let at = fnc_platform::shell::cursor_pos().unwrap_or((0, 0));
        match fnc_platform::fileops::context_menu(self.owner(), &same, at, &items) {
            Ok(fnc_platform::fileops::MenuOutcome::App(id)) => {
                if let Some(cmd) = Command::ALL.get(id as usize) {
                    self.run(ctx, *cmd);
                }
            }
            Ok(fnc_platform::fileops::MenuOutcome::Rename) => self.start_rename(),
            Ok(fnc_platform::fileops::MenuOutcome::Invoked(verb)) => {
                tracing::debug!(%verb, "shell verb invoked");
                if verb.eq_ignore_ascii_case("open") {
                    self.record_history();
                }
            }
            Ok(fnc_platform::fileops::MenuOutcome::None) => {}
            // Not on Windows (or the shell refused): the egui menu is used instead.
            Err(e) => {
                tracing::debug!(error = %e, "shell context menu unavailable");
                let at = ctx.pointer_latest_pos().unwrap_or_default();
                self.dialog = Some(dialogs::Dialog::ContextMenu { at });
            }
        }
    }

    // ------------------------------------------------------------------ keys

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let search_has_focus = ctx.memory(|m| m.has_focus(egui::Id::new(SEARCH_ID)));
        if search_has_focus {
            self.focus = Focus::Search;
        }
        let kctx = if self.focus == Focus::Search { KeyCtx::Search } else { KeyCtx::Results };
        let modal = self.dialog.is_some() || self.rename.is_some() || self.options_window.is_some();
        let mut cmds = Vec::new();
        let mut esc = false;
        let mut tab = false;
        let mut typed = String::new();
        let complete_open = self.complete.is_open();
        let mut complete_keys = Vec::new();
        let mut paste_seen = false;
        let mut shift_dels = fnc_platform::shell::take_shift_delete();
        let type_to_search = self.settings.type_to_search;
        ctx.input_mut(|i| {
            let mut keep = Vec::with_capacity(i.events.len());
            for e in std::mem::take(&mut i.events) {
                match &e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        if *key == Key::Escape && !modifiers.any() {
                            esc = true;
                            continue;
                        }
                        if modal {
                            keep.push(e);
                            continue;
                        }
                        if complete_open
                            && !modifiers.any()
                            && matches!(key, Key::ArrowDown | Key::ArrowUp | Key::Enter | Key::Tab | Key::Delete)
                        {
                            complete_keys.push(*key);
                            continue;
                        }
                        if *key == Key::Tab && !modifiers.ctrl && !modifiers.alt {
                            tab = true;
                            continue;
                        }
                        let chord = Chord::from_event(*key, *modifiers);
                        if let Some(cmd) = self.keymap.lookup(kctx, &chord) {
                            cmds.push(cmd);
                            continue;
                        }
                        keep.push(e);
                    }
                    egui::Event::Text(s) if !modal && kctx == KeyCtx::Results && type_to_search => {
                        typed.push_str(s);
                    }
                    // egui-winit delivers Ctrl+C/X/V as clipboard events, not keys; in the
                    // result list they act on files (in the search box on text).
                    egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
                        if !modal && kctx == KeyCtx::Results =>
                    {
                        // On Windows egui-winit also maps Shift+Del to Cut (told apart
                        // by the window subclass's count).
                        let key = |key| Chord { ctrl: true, shift: false, alt: false, key };
                        let chord = match &e {
                            egui::Event::Copy => key(Key::C),
                            egui::Event::Cut if shift_dels > 0 => {
                                shift_dels -= 1;
                                Chord { ctrl: false, shift: true, alt: false, key: Key::Delete }
                            }
                            egui::Event::Cut => key(Key::X),
                            _ => key(Key::V),
                        };
                        if chord.key == Key::V {
                            paste_seen = true;
                        }
                        if let Some(cmd) = self.keymap.lookup(kctx, &chord) {
                            cmds.push(cmd);
                        }
                    }
                    _ => keep.push(e),
                }
            }
            i.events = keep;
        });
        // Ctrl+V with only files on the clipboard produces no egui event at all; the
        // window subclass counts the raw key presses.
        let raw_presses = fnc_platform::shell::take_ctrl_v();
        if raw_presses > 0 && !paste_seen && kctx == KeyCtx::Results && !modal {
            cmds.push(Command::Paste);
        }
        for k in complete_keys {
            if k == Key::Delete {
                // Del removes the highlighted history entry (SPEC ch.3 §2).
                if let Some(text) = self.complete.selected_history() {
                    self.history.remove(&text);
                    self.complete.remove_selected();
                }
                continue;
            }
            if let Some(q) = self.complete.key(k, &self.query) {
                self.query = q;
                self.edited_at = Some(Instant::now());
                self.keep_focus = true;
                self.move_search_cursor_to_end(ctx);
            }
        }
        if esc {
            self.on_escape(ctx);
        }
        if tab {
            self.focus = if self.focus == Focus::Search { Focus::Results } else { Focus::Search };
            if self.focus == Focus::Search {
                self.keep_focus = true;
            } else {
                ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(SEARCH_ID)));
                if self.selected.is_none() && !self.results.is_empty() {
                    self.select_only(0);
                }
            }
        }
        if !typed.is_empty() {
            // Type-to-search: continue typing in the search box.
            self.query.push_str(&typed);
            self.edited_at = Some(Instant::now());
            self.focus = Focus::Search;
            self.keep_focus = true;
            self.move_search_cursor_to_end(ctx);
        }
        for c in cmds {
            self.run(ctx, c);
        }
    }

    fn move_search_cursor_to_end(&self, ctx: &egui::Context) {
        let id = egui::Id::new(SEARCH_ID);
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let end = egui::text::CCursor::new(self.query.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ctx, id);
    }

    fn on_escape(&mut self, ctx: &egui::Context) {
        if self.options_window.is_some() {
            self.options_window = None;
            return;
        }
        if self.dialog.take().is_some() {
            return;
        }
        if self.rename.take().is_some() {
            return;
        }
        if self.complete.is_open() {
            self.complete.close();
            self.keep_focus = true;
            return;
        }
        if self.query.is_empty() {
            self.run(ctx, Command::HideWindow);
        } else {
            self.set_query(String::new());
        }
        // egui drops text focus on Escape; Everything keeps typing in the search box.
        self.focus = Focus::Search;
        self.keep_focus = true;
    }

    // ------------------------------------------------------------------ widgets

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::new(SEARCH_ID);
        let precedence = self.search_opts.precedence;
        let filter_w = 150.0;
        let mut enter = false;
        ui.horizontal(|ui| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                let job = syntax_job(ui, buf.as_str(), precedence);
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let edit = egui::TextEdit::singleline(&mut self.query)
                .id(id)
                .hint_text(t("search-hint"))
                .desired_width((ui.available_width() - filter_w - 16.0).max(100.0))
                .font(egui::TextStyle::Body)
                .layouter(&mut layouter);
            let resp = ui.add(edit);
            if self.keep_focus {
                self.keep_focus = false;
                resp.request_focus();
            }
            if self.focus_search {
                self.focus_search = false;
                self.focus = Focus::Search;
                resp.request_focus();
                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.query.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
            if resp.gained_focus() {
                self.focus = Focus::Search;
            }
            enter = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if resp.changed() {
                self.edited_at = Some(Instant::now());
                if self.settings.auto_complete {
                    let cursor = egui::TextEdit::load_state(ui.ctx(), id)
                        .and_then(|s| s.cursor.char_range())
                        .map_or(self.query.chars().count(), |r| r.primary.index.into());
                    let snap = self.shared.try_snapshot_for(Duration::from_millis(2));
                    self.complete.update(&self.query, cursor, &self.history, snap.as_ref());
                }
            }
            if let Some(q) = self.complete.show(ui, &resp, &self.query) {
                self.query = q;
                self.edited_at = Some(Instant::now());
                self.keep_focus = true;
                self.move_search_cursor_to_end(ui.ctx());
            }
            if !resp.has_focus() && !self.complete.hovered() {
                self.complete.close();
            }
            self.filter_combo(ui, filter_w);
        });
        // `/key=value` and `/command` (SPEC ch.3 §13) run on Enter.
        if self.query.starts_with('/') && (enter || ui.input(|i| i.key_pressed(Key::Enter))) {
            let cmd = std::mem::take(&mut self.query);
            self.inline_command(ui.ctx(), &cmd);
            self.send_search();
        }
        // `search_delay` (default 0 ms, like Everything).
        if let Some(at) = self.edited_at {
            let delay = Duration::from_millis(u64::from(self.settings.search_delay));
            if self.query.starts_with('/') {
                self.edited_at = None;
            } else if at.elapsed() >= delay {
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

    /// `/search_delay=500`, `/reset search_delay`, `/open-path`, `/search_delay`.
    fn inline_command(&mut self, ctx: &egui::Context, text: &str) {
        let body = text.trim_start_matches('/').trim();
        if let Some((key, value)) = body.split_once('=') {
            let mut s = self.settings.clone();
            match s.set(key.trim(), value.trim()) {
                Ok(()) => {
                    self.apply_settings(ctx, s);
                    self.say(t_args(
                        "msg-setting-changed",
                        &[("key", key.trim().to_string().into()), ("value", value.trim().to_string().into())],
                    ));
                }
                Err(e) => self.say(e.to_string()),
            }
        } else if let Some(key) = body.strip_prefix("reset ") {
            let mut s = self.settings.clone();
            if s.reset(key.trim()) {
                self.apply_settings(ctx, s);
            }
        } else if let Some(cmd) = Command::from_id(body) {
            self.run(ctx, cmd);
        } else if let Some(v) = self.settings.get(body) {
            self.say(format!("{body} = {v}"));
        } else {
            self.say(t_args("msg-unknown-command", &[("command", body.to_string().into())]));
        }
    }

    fn filter_combo(&mut self, ui: &mut egui::Ui, width: f32) {
        let current = if self.active_filters.is_empty() {
            t("filter-everything")
        } else {
            self.active_filters
                .iter()
                .filter_map(|&i| self.filters.list.get(i))
                .map(|f| f.name.clone())
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let mut choice: Option<Option<usize>> = None;
        let mut organize = false;
        egui::ComboBox::from_id_salt("fennec-filter").selected_text(current).width(width).show_ui(ui, |ui| {
            if ui.selectable_label(self.active_filters.is_empty(), t("filter-everything")).clicked() {
                choice = Some(None);
            }
            for (i, f) in self.filters.list.iter().enumerate() {
                if ui.selectable_label(self.active_filters.contains(&i), &f.name).clicked() {
                    choice = Some(Some(i));
                }
            }
            ui.separator();
            if ui.button(t("cmd-organize-filters")).clicked() {
                organize = true;
            }
        });
        if organize {
            self.pending.push(Command::OrganizeFilters);
        }
        match choice {
            Some(None) => self.set_filters(Vec::new()),
            Some(Some(i)) => self.set_filters(vec![i]),
            None => {}
        }
    }

    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        let mut toggle: Option<(Option<usize>, bool)> = None;
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.active_filters.is_empty(), t("filter-everything")).clicked() {
                toggle = Some((None, false));
            }
            for (i, f) in self.filters.list.iter().enumerate() {
                let r = ui.selectable_label(self.active_filters.contains(&i), &f.name);
                if r.clicked() {
                    toggle = Some((Some(i), ui.input(|inp| inp.modifiers.ctrl)));
                }
            }
        });
        match toggle {
            Some((None, _)) => self.set_filters(Vec::new()),
            // Ctrl+click combines filters (any of them matches).
            Some((Some(i), true)) => {
                let mut a = self.active_filters.clone();
                if let Some(p) = a.iter().position(|&x| x == i) {
                    a.remove(p);
                } else {
                    a.push(i);
                }
                self.set_filters(a);
            }
            Some((Some(i), false)) => self.set_filters(vec![i]),
            None => {}
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let n = format::group_digits(self.results.len() as u64);
            ui.label(t_args("status-objects", &[("count", n.into())]));
            let sel = self.sel.count(self.results.len());
            if sel > 0 {
                ui.separator();
                let mut text = t_args("status-selected", &[("count", format::group_digits(sel as u64).into())]);
                if !self.sel.all
                    && sel <= 10_000
                    && let Some(s) = self.shared.try_snapshot_for(Duration::from_millis(1))
                {
                    let total: u64 = self
                        .sel
                        .ids
                        .iter()
                        .filter(|&&id| s.is_live(id) && !s.is_dir(id))
                        .filter_map(|&id| s.size(id))
                        .sum();
                    text.push_str(&format!(", {}", format::size(total)));
                }
                ui.label(text);
            }
            ui.separator();
            ui.label(t_args("status-elapsed", &[("ms", format!("{:.1}", self.elapsed.as_secs_f64() * 1000.0).into())]));
            if !self.active_filters.is_empty() {
                ui.separator();
                ui.label(self.active_filter_name());
            }
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
                let mut s = status_text(&self.status);
                if self.indexer.paused() {
                    s = format!("{s} ({})", t("status-paused"));
                }
                ui.label(s);
            });
        });
    }

    // ------------------------------------------------------------------ settings

    /// Applies edited settings (Options window, `/key=value`) and saves them.
    fn apply_settings(&mut self, ctx: &egui::Context, new: Settings) {
        let old = std::mem::replace(&mut self.settings, new);
        let s = &self.settings;
        if old.theme != s.theme {
            apply_theme(ctx, s.theme);
        }
        if (old.ui_scale - s.ui_scale).abs() > f32::EPSILON {
            ctx.set_zoom_factor(s.ui_scale.clamp(0.5, 3.0));
        }
        if old.key_bindings != s.key_bindings {
            let (km, bad) = Keymap::with_overrides(&s.key_bindings);
            self.keymap = km;
            if !bad.is_empty() {
                let msg = t_args("msg-bad-bindings", &[("bindings", bad.join(", ").into())]);
                self.say(msg);
            }
        }
        let s = &self.settings;
        if old.columns != s.columns {
            self.columns = crate::columns::parse_columns(&s.columns);
            self.rows.clear();
        }
        if old.sort != s.sort || old.folders_first != s.folders_first {
            self.sort = sort_from_settings(s);
        }
        #[cfg(windows)]
        if old.show_tray_icon != s.show_tray_icon || old.hotkey_show_window != s.hotkey_show_window {
            // Drop the old icon/hotkey first so the hotkey can be registered again.
            self.integration = None;
            self.integration = Some(crate::tray::install(ctx, s.show_tray_icon, &s.hotkey_show_window));
        }
        let s = &self.settings;
        if old.pause_updates != s.pause_updates {
            self.indexer.set_paused(s.pause_updates);
        }
        if old.date_format != s.date_format || old.size_format != s.size_format {
            self.rows.clear();
        }
        let restart = old.language != s.language
            || old.ntfs_auto_include != s.ntfs_auto_include
            || old.ntfs_exclude_volumes != s.ntfs_exclude_volumes
            || old.folders != s.folders
            || old.file_lists != s.file_lists
            || old.index_date_accessed != s.index_date_accessed
            || old.db_save_interval != s.db_save_interval;
        if restart {
            self.say(t("msg-restart-required"));
        }
        self.rebuild_search_options();
        self.send_search();
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!(error = %e, "cannot save settings");
        }
    }

    /// Persists window-derived state (columns, sort) into the settings.
    fn sync_view_settings(&mut self) {
        self.settings.columns = crate::columns::columns_setting(&self.columns);
        self.settings.sort = crate::columns::sort_setting(&self.sort);
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum MoveMode {
    Select,
    Extend,
    FocusOnly,
}

/// Text forms for the copy commands.
#[derive(Copy, Clone, PartialEq, Eq)]
enum TextKind {
    FullPath,
    Quoted,
    Name,
    Folder,
    Tsv,
    Csv,
    Efu,
}

fn sort_from_settings(s: &Settings) -> SortSpec {
    let keys = crate::columns::parse_sort(&s.sort);
    let mut spec = match keys.first() {
        Some(&(c, asc)) => SortSpec::by(c, asc, s.folders_first),
        None => SortSpec::by(SortColumn::Name, true, s.folders_first),
    };
    for (i, k) in keys.iter().skip(1).enumerate() {
        spec.then[i] = Some(*k);
    }
    spec
}

fn default_export_path() -> String {
    let dir =
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    dir.join("results.efu").display().to_string()
}

/// Search-box syntax highlighting from the parser's spans (SPEC ch.3 §2): operators,
/// modifiers, functions and macros get distinct colours; unclosed quotes/groups are
/// underlined.
fn syntax_job(ui: &egui::Ui, text: &str, precedence: fnc_core::settings::Precedence) -> LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let dark = ui.visuals().dark_mode;
    let normal = ui.visuals().text_color();
    let color = |k: fnc_query::SpanKind| match k {
        fnc_query::SpanKind::Or | fnc_query::SpanKind::Not | fnc_query::SpanKind::Group => ACCENT,
        fnc_query::SpanKind::Modifier => {
            if dark {
                Color32::from_rgb(0xC5, 0x86, 0xC0)
            } else {
                Color32::from_rgb(0x8E, 0x3B, 0x9A)
            }
        }
        fnc_query::SpanKind::Function => {
            if dark {
                Color32::from_rgb(0x56, 0x9C, 0xD6)
            } else {
                Color32::from_rgb(0x1F, 0x5F, 0xB0)
            }
        }
        fnc_query::SpanKind::Macro => {
            if dark {
                Color32::from_rgb(0x4E, 0xC9, 0xB0)
            } else {
                Color32::from_rgb(0x16, 0x7F, 0x6E)
            }
        }
        fnc_query::SpanKind::Text => normal,
    };
    let env = fnc_query::ParseEnv { precedence, ..fnc_query::ParseEnv::default() };
    let parsed = fnc_query::parse(text, &env);
    // Per-byte colour, later spans (more specific) win; then emit runs.
    let mut colors = vec![normal; text.len()];
    for s in &parsed.spans {
        for c in colors.iter_mut().take(s.range.end.min(text.len())).skip(s.range.start) {
            *c = color(s.kind);
        }
    }
    let mut underline = vec![false; text.len()];
    for d in &parsed.diagnostics {
        for u in underline.iter_mut().take(d.range.end.min(text.len())).skip(d.range.start) {
            *u = true;
        }
    }
    let mut job = LayoutJob::default();
    let mut start = 0;
    for i in 1..=text.len() {
        let boundary = i == text.len()
            || (text.is_char_boundary(i) && (colors[i] != colors[start] || underline[i] != underline[start]));
        if boundary && text.is_char_boundary(i) {
            let mut f = TextFormat { font_id: font.clone(), color: colors[start], ..Default::default() };
            if underline[start] {
                f.underline = egui::Stroke::new(1.0, ui.visuals().warn_fg_color);
            }
            job.append(&text[start..i], 0.0, f);
            start = i;
        }
    }
    job
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

/// Searches unchanged for this long count as "used" (search history).
const HISTORY_IDLE: Duration = Duration::from_secs(3);

impl eframe::App for FennecApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_channels(ctx);
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.settings.close_to_tray
            && self.can_hide_to_tray()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
        if !self.history_recorded
            && self.edited_at.is_none()
            && !self.results.is_empty()
            && self.sent_query.as_deref() == Some(self.query.as_str())
        {
            match self.history_idle_since {
                Some(at) if at.elapsed() >= HISTORY_IDLE => self.record_history(),
                Some(at) => ctx.request_repaint_after(HISTORY_IDLE.saturating_sub(at.elapsed())),
                None => self.history_idle_since = Some(Instant::now()),
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Track the geometry while visible (hidden/minimised windows report nonsense).
        let geometry = ctx.input(|i| {
            let v = i.viewport();
            match (v.outer_rect, v.inner_rect, v.minimized, v.maximized) {
                (Some(outer), Some(inner), Some(false) | None, maximized) => {
                    Some((egui::Rect::from_min_size(outer.min, inner.size()), maximized.unwrap_or(false)))
                }
                _ => None,
            }
        });
        if !self.hidden && geometry.is_some() {
            self.window_geometry = geometry;
        }
        let hwnd = native_handle(frame);
        if hwnd != self.hwnd && hwnd != 0 {
            fnc_platform::shell::watch_ctrl_v(hwnd);
        }
        self.hwnd = hwnd;
        self.handle_keys(&ctx);
        let alt_only = ctx.input(|i| i.modifiers.alt && !i.modifiers.ctrl && !i.modifiers.shift);
        self.menu_peek =
            !self.settings.show_menu_bar && (alt_only || (self.menu_peek && egui::Popup::is_any_open(&ctx)));
        if self.settings.show_menu_bar || self.menu_peek {
            egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        }
        egui::Panel::top("search").show(ui, |ui| {
            ui.add_space(4.0);
            self.search_bar(ui);
            if self.settings.show_filter_bar {
                self.filter_bar(ui);
            }
            ui.add_space(2.0);
        });
        if self.settings.show_status_bar {
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.results_table(ui));
        self.dialogs(&ctx);
        self.options_viewport(&ctx);
        for cmd in std::mem::take(&mut self.pending) {
            self.run(&ctx, cmd);
        }
        self.after_table(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some((rect, maximized)) = self.window_geometry {
            self.settings.window_maximized = maximized;
            if !maximized {
                self.settings.window_rect =
                    format!("{:.0},{:.0},{:.0},{:.0}", rect.min.x, rect.min.y, rect.width(), rect.height());
            }
        }
        self.indexer.stop();
        self.indexer.save();
        self.history.save();
        self.sync_view_settings();
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!(error = %e, "cannot save settings");
        }
    }
}

/// Parses the `window_rect` setting (`x,y,width,height`); `None` if empty or unusable.
pub fn parse_window_rect(s: &str) -> Option<[f32; 4]> {
    let v: Vec<f32> = s.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>().ok()?;
    let r: [f32; 4] = v.try_into().ok()?;
    // Ignore off-screen leftovers (e.g. a disconnected monitor) and absurd sizes.
    (r[2] >= 400.0 && r[3] >= 200.0 && r[2] < 20_000.0 && r[3] < 20_000.0 && r[0] > -2000.0 && r[1] > -200.0)
        .then_some(r)
}

/// The window's native handle (`HWND`), 0 if unavailable.
fn native_handle(frame: &eframe::Frame) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = frame.window_handle()
            && let RawWindowHandle::Win32(w) = h.as_raw()
        {
            return w.hwnd.get();
        }
    }
    let _ = frame;
    0
}

#[cfg(test)]
mod tests;

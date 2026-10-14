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
mod edit_ui;
mod findbar;
mod grid;
mod integration;
mod menus;
mod options;
mod preview;
mod remote_ui;
mod semantic_ui;
mod session;
mod sidebar;
pub(super) mod table;
mod tabs;
mod toolbar;
mod window;

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

/// Restarts indexing from scratch with the current settings.
pub type Reindex = Arc<dyn Fn(&mut Indexer, &Settings) + Send + Sync>;

/// Everything the window needs from `main`.
pub struct AppDeps {
    /// Shared index.
    pub shared: SharedIndex,
    /// Search service.
    pub engine: SearchEngine,
    /// Search results.
    pub engine_rx: Receiver<EngineEvent>,
    /// Creates the engine of another view (new window).
    pub new_engine: Arc<dyn Fn() -> (SearchEngine, Receiver<EngineEvent>) + Send + Sync>,
    /// Indexer progress.
    pub index_rx: Receiver<IndexEvent>,
    /// Single-instance ownership (mutex + command pipe); kept alive for the process
    /// lifetime. Forwarded command lines arrive on it.
    pub instance: Option<Primary>,
    /// Indexer handle (stopped on exit).
    pub indexer: Indexer,
    /// Rebuilds the index while running (Tools → Rebuild Index, IPC `REBUILD_DB`).
    pub reindex: Option<Reindex>,
    /// Property, content and hash stores (M3).
    pub stores: Arc<crate::stores::Stores>,
    /// Settings.
    pub settings: Settings,
    /// Where settings are saved (filters, bookmarks and history live next to it).
    pub settings_path: PathBuf,
    /// Initial search text.
    pub initial_search: Option<String>,
    /// The start-up command line (filters, sort, window commands).
    pub initial_cli: Option<crate::cli::Cli>,
    /// Start hidden (tray).
    pub start_hidden: bool,
    /// Run the IPC/Everything/HTTP servers for this instance (`None`: no servers).
    pub servers: Option<crate::servers::ServersContext>,
}

#[derive(Clone)]
struct Row {
    /// One text per visible column.
    cells: Vec<String>,
    /// Full path (icons, thumbnails).
    path: PathBuf,
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

/// The new id of `id` after a compaction (`None` = removed).
fn remapped(remap: &[u32], id: EntryId) -> Option<EntryId> {
    remap.get(id.idx()).copied().filter(|&n| n != u32::MAX).map(EntryId)
}

impl View {
    /// Renumbers everything this view keys by entry id (see `IndexEvent::Compacted`).
    pub(super) fn remap_ids(&mut self, old_instance: u64, remap: &[u32]) {
        // Views whose ids belong to another id space (a newer index) are left alone.
        if self.results.instance != old_instance && self.results.instance != 0 {
            return;
        }
        self.sel.ids = self.sel.ids.iter().filter_map(|&id| remapped(remap, id)).collect();
        self.selected_id = self.selected_id.and_then(|id| remapped(remap, id));
        self.omit = self.omit.iter().filter_map(|&id| remapped(remap, id)).collect();
        if let Some(r) = &mut self.rename {
            match remapped(remap, r.id) {
                Some(n) => r.id = n,
                None => self.rename = None,
            }
        }
        // Rows are cached by id; the results are re-run.
        self.rows.clear();
        self.selected = None;
    }
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
    /// Finished with something for the undo stack.
    Recorded(String, edit_ui::Recorded),
}

/// Everything one search view owns: its search, results, selection and engine.
pub(super) struct View {
    /// Distinguishes widget state of views in different windows.
    uid: u64,
    /// Full path to select once the results arrive (`-filename`).
    pending_select: Option<String>,
    engine: SearchEngine,
    engine_rx: Receiver<EngineEvent>,
    search_opts: Arc<SearchOptions>,
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
    /// Focused (cursor) row.
    selected: Option<usize>,
    selected_id: Option<EntryId>,
    sel: Selection,
    scroll_to: Option<usize>,
    rows: HashMap<EntryId, Row>,
    rows_generation: u64,
    omit: HashSet<EntryId>,
    /// Active filters (indices into `filters.list`; empty = Everything).
    active_filters: Vec<usize>,
    /// The current search was recorded in the history.
    history_recorded: bool,
    /// When the current search was sent (idle searches enter the history).
    history_idle_since: Option<Instant>,
    complete: complete::CompleteState,
    focus: Focus,
    focus_search: bool,
    keep_focus: bool,
    rename: Option<RenameState>,
    /// Columns of the thumbnail grid last frame (arrow keys move by rows).
    grid_cols: usize,
    /// Preview panel.
    preview: preview::PreviewState,
    /// Last refresh requested because the index moved on (tabs and windows other than
    /// the first view are not notified by the indexer).
    stale_refresh: Option<Instant>,
    /// Sort to restore when the search stops being semantic.
    sort_before_semantic: Option<SortSpec>,
    /// Find bar (Ctrl+F in find-bar mode).
    find: findbar::FindBar,
    /// Back/forward through the tab's searches.
    nav: toolbar::Nav,
    /// Connected to an ETP server: searches run there (Tools → Connect to ETP Server).
    remote: Option<remote_ui::RemoteView>,
}

/// Main application state.
pub struct FennecApp {
    shared: SharedIndex,
    index_rx: Receiver<IndexEvent>,
    instance: Option<Primary>,
    indexer: Indexer,
    reindex: Option<Reindex>,
    stores: Arc<crate::stores::Stores>,
    settings: Settings,
    settings_path: PathBuf,
    keymap: Keymap,

    columns: Vec<ColumnState>,

    /// Shell type names (`type:` and the Type column), shared with the engine.
    type_names: fnc_exec::TypeNames,
    /// Type column texts per `(extension, is_dir)`.
    type_cache: HashMap<(String, bool), String>,

    filters: Filters,
    bookmarks: Bookmarks,
    history: SearchHistory,

    status: IndexStatus,
    message: Option<(String, Instant)>,
    hidden: bool,
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
    /// Icons and thumbnails.
    thumbs: crate::thumbs::Thumbs,
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
    /// The active search view (others are swapped in for tabs and windows).
    v: View,
    /// Other windows (`-newwindow`, Ctrl+N), each with its own view.
    windows: Vec<window::WindowView>,
    /// A secondary window is being drawn (commands act on it).
    in_secondary: bool,
    /// The secondary window being drawn asked to close (Esc, Ctrl+W).
    close_secondary: bool,
    /// The UI context (dialogs that apply settings).
    ctx: egui::Context,
    /// Creates a search engine for a new view.
    new_engine: Arc<dyn Fn() -> (SearchEngine, Receiver<EngineEvent>) + Send + Sync>,
    /// Semantic search (SPEC ch.5).
    sem: semantic_ui::SemanticUi,
    /// Undo stack and advanced rename.
    edit: edit_ui::EditUi,
    /// Tabs of the window being drawn (swapped like `v`).
    tabs: tabs::Tabs,
    /// Folder tree cache of the sidebar.
    sidebar_cache: sidebar::SidebarCache,
    /// IPC, Everything-compatible and HTTP servers.
    servers: Option<integration::AppServers>,
}

impl FennecApp {
    /// Builds the app and kicks off the first search.
    pub fn new(cc: &eframe::CreationContext<'_>, deps: AppDeps) -> Self {
        let ctx = &cc.egui_ctx;
        crate::fonts::install_cjk_fallback(ctx.clone());
        deps.stores.cells.set_repaint(ctx.clone());
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
        crate::columns::set_custom_columns(&deps.settings.custom_columns);
        let columns = crate::columns::parse_columns(&deps.settings.columns);
        let (worker_tx, worker_rx) = crossbeam_channel::unbounded();
        let thumb_dir =
            (deps.settings.thumbnail_cache_mb > 0).then(|| crate::filters::data_file(&deps.settings_path, "thumbs"));
        if let Some(dir) = thumb_dir.clone() {
            let limit = u64::from(deps.settings.thumbnail_cache_mb) << 20;
            let _ = std::thread::Builder::new()
                .name("fennec-thumb-trim".into())
                .spawn(move || crate::thumbs::trim_disk_cache(&dir, limit));
        }
        if deps.settings.pause_updates {
            deps.indexer.set_paused(true);
        }
        let new_engine = deps.new_engine;
        let servers = deps.servers;
        let mut app = Self {
            shared: deps.shared,
            index_rx: deps.index_rx,
            instance: deps.instance,
            indexer: deps.indexer,
            reindex: deps.reindex,
            stores: deps.stores,
            keymap,
            columns,
            type_names: fnc_exec::TypeNames::new(|ext| fnc_platform::shell::type_name(ext, false)),
            type_cache: HashMap::new(),
            filters,
            bookmarks,
            history,
            status: IndexStatus::Starting,
            message: None,
            // Start-hidden is applied by `main` via `ViewportBuilder::with_visible(false)`
            // (no flash of the window).
            hidden: deps.start_hidden,
            dialog: None,
            options_window: None,
            pending: Vec::new(),
            after: Vec::new(),
            hwnd: 0,
            menu_peek: false,
            window_geometry: None,
            thumbs: crate::thumbs::Thumbs::new(2, thumb_dir),
            worker_tx,
            worker_rx,
            quitting: false,
            settings: deps.settings,
            settings_path: deps.settings_path,
            #[cfg(windows)]
            last_toggle: None,
            #[cfg(windows)]
            integration,

            v: View {
                uid: 0,
                pending_select: None,
                engine: deps.engine,
                engine_rx: deps.engine_rx,
                search_opts: Arc::new(SearchOptions::default()),
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
                sel: Selection::default(),
                scroll_to: None,
                rows: HashMap::new(),
                rows_generation: 0,
                omit: HashSet::new(),
                active_filters,
                history_recorded: true,
                history_idle_since: None,
                complete: complete::CompleteState::default(),
                focus: Focus::Search,
                focus_search: true,
                keep_focus: false,
                rename: None,
                grid_cols: 1,
                preview: preview::PreviewState::default(),
                stale_refresh: None,
                sort_before_semantic: None,
                find: Default::default(),
                nav: Default::default(),
                remote: None,
            },
            windows: Vec::new(),
            in_secondary: false,
            close_secondary: false,
            new_engine,
            ctx: ctx.clone(),
            sem: Default::default(),
            edit: Default::default(),
            tabs: Default::default(),
            sidebar_cache: Default::default(),
            servers: None,
        };
        // Data files that were set aside at load (ADR-038).
        let recovered = [app.filters.recovery.take(), app.bookmarks.recovery.take(), app.history.recovery.take()];
        for r in recovered.into_iter().flatten() {
            app.notice(crate::upgrade::notice(&r, false));
        }
        app.semantic_init();
        app.rebuild_search_options();
        app.start_servers(servers);
        app.send_search();
        // Sessions reopen the saved tabs unless the command line names a search.
        if !deps.initial_cli.as_ref().is_some_and(|c| c.search.is_some()) && app.v.query.is_empty() {
            app.restore_session();
        }
        if let Some(cli) = deps.initial_cli {
            app.apply_cli(ctx, cli, false);
        }
        app
    }

    // ------------------------------------------------------------------ searching

    /// Search options from settings plus the filters' macros, omitted results and the
    /// shell's type names.
    fn rebuild_search_options(&mut self) {
        let mut o = SearchOptions::from_settings(&self.settings);
        o.macros.extend(self.filters.macros());
        o.type_names = Some(self.type_names.clone());
        o.formulas = crate::columns::custom_formulas();
        self.stores.apply_options(&mut o);
        o.semantic = self.semantic_source();
        o.semantic_fuse = o.semantic.is_some() && self.settings.semantic_smart;
        if !self.v.omit.is_empty() {
            let instance = self.shared.try_snapshot_for(Duration::from_millis(20)).map_or(0, |s| s.instance());
            o.omit = Some(fnc_query::plan::OmitSet { instance, ids: self.v.omit.iter().copied().collect() });
        }
        self.v.search_opts = Arc::new(o);
        self.sync_host();
    }

    /// The active filters with their own match options (SPEC ch.2 §7).
    fn filter_set(&self) -> Option<fnc_query::FilterSet> {
        let filters: Vec<fnc_query::Filter> = self
            .v
            .active_filters
            .iter()
            .filter_map(|&i| self.filters.list.get(i))
            .map(crate::filters::Filter::to_query)
            .collect();
        let set = fnc_query::FilterSet { filters, and: false };
        (!set.is_empty()).then_some(set)
    }

    fn send_search(&mut self) {
        let id = self.v.next_id;
        self.v.next_id += 1;
        // Searches by meaning list the most similar first (SPEC ch.5 §3.1); the previous
        // order comes back with the next literal search (unless a header was clicked).
        let semantic = semantic_ui::is_semantic_query(&self.v.query, false);
        if semantic && self.v.sort_before_semantic.is_none() && self.v.sort.column != SortColumn::Relevance {
            self.v.sort_before_semantic = Some(self.v.sort);
            self.v.sort = SortSpec::by(SortColumn::Relevance, false, self.v.sort.folders_first);
        } else if !semantic
            && let Some(prev) = self.v.sort_before_semantic.take()
            && self.v.sort.column == SortColumn::Relevance
        {
            self.v.sort = prev;
        }
        if self.v.sent_query.as_deref() != Some(self.v.query.as_str()) && !self.v.omit.is_empty() {
            self.v.omit.clear();
            self.rebuild_search_options();
        }
        if self.v.sent_query.as_deref() != Some(self.v.query.as_str()) {
            self.v.history_recorded = false;
            self.v.history_idle_since = Some(Instant::now());
        }
        self.v.sent_query = Some(self.v.query.clone());
        self.v.edited_at = None;
        if self.send_remote_search(id) {
            return;
        }
        self.v.engine.search(SearchRequest {
            id,
            text: self.v.query.clone(),
            filter: self.filter_set(),
            sort: self.v.sort,
            options: self.v.search_opts.clone(),
        });
    }

    fn set_query(&mut self, q: String) {
        self.v.query = q;
        self.v.complete.close();
        self.send_search();
    }

    /// Rebuilds the index from scratch with the current settings (Tools → Rebuild Index,
    /// IPC `REBUILD_DB`, changed index sources).
    pub(super) fn rebuild_index(&mut self) {
        let Some(r) = self.reindex.clone() else {
            self.say(t("msg-restart-required"));
            return;
        };
        self.indexer.save();
        r(&mut self.indexer, &self.settings);
        self.indexer.set_paused(self.settings.pause_updates);
        self.status = IndexStatus::Starting;
        self.say(t("msg-rebuilding-index"));
    }

    /// The index was renumbered: every view keeps its selection, cursor, omitted entries
    /// and in-place rename on the same files, and searches again.
    fn on_compacted(&mut self, old_instance: u64, remap: &[u32]) {
        self.v.remap_ids(old_instance, remap);
        self.tabs.remap_ids(old_instance, remap);
        for w in &mut self.windows {
            w.view.remap_ids(old_instance, remap);
            w.tabs.remap_ids(old_instance, remap);
        }
        self.rebuild_search_options();
        self.v.engine.refresh();
    }

    /// Re-runs the view's search when the index changed since its results were computed
    /// and no search is in flight (at most every 250 ms).
    pub(super) fn refresh_if_stale(&mut self) {
        if self.is_remote() {
            return;
        }
        let in_flight = self.v.latest_shown + 1 < self.v.next_id;
        if in_flight || self.v.stale_refresh.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
            return;
        }
        let Some(s) = self.shared.try_snapshot_for(Duration::ZERO) else { return };
        let stale = s.generation() != self.v.results.generation || s.instance() != self.v.results.instance;
        drop(s);
        if stale && self.v.sent_query.is_some() {
            self.v.stale_refresh = Some(Instant::now());
            self.v.engine.refresh();
        }
    }

    fn record_history(&mut self) {
        if !self.v.history_recorded {
            let q = self.v.query.clone();
            self.v.nav.visit(&q);
            if self.settings.search_history_enabled {
                self.history.record(&q);
            }
            self.v.history_recorded = true;
        }
    }

    fn on_outcome(&mut self, o: SearchOutcome) {
        if o.id < self.v.latest_shown {
            return;
        }
        self.v.latest_shown = o.id;
        self.v.elapsed = o.elapsed;
        self.v.highlighter = self.settings.highlight.then(|| Highlighter::new(&o.plan)).filter(|h| !h.is_empty());
        self.v.preview.set_plan(&o.plan);
        self.v.diagnostics = diagnostics_text(&o);
        let same_set = Arc::ptr_eq(&self.v.results, &o.results);
        self.v.results = o.results;
        if !same_set {
            self.v.rows.clear();
        }
        if let Some(target) = self.v.pending_select.take() {
            let found = self.view_index().try_snapshot_for(Duration::from_millis(50)).and_then(|s| {
                let mut buf = String::new();
                self.v.results.ids.iter().position(|&id| {
                    s.full_path(id, &mut buf);
                    buf.eq_ignore_ascii_case(&target)
                })
            });
            match found {
                Some(i) => {
                    self.select_only(i);
                    self.v.focus = Focus::Results;
                }
                // Keep waiting while the folder search has not produced results yet.
                None => self.v.pending_select = self.v.results.is_empty().then_some(target),
            }
            return;
        }
        // Keep the cursor on the same entry if it is still listed.
        self.v.selected = self.v.selected_id.and_then(|sid| self.v.results.ids.iter().position(|&i| i == sid));
        if self.v.selected.is_none() {
            self.v.selected_id = None;
        }
        self.v.sel.all = false;
        // Deleted or no longer matching entries leave the selection.
        if !self.v.sel.ids.is_empty() {
            let listed: HashSet<EntryId> = self.v.results.ids.iter().copied().collect();
            self.v.sel.ids.retain(|id| listed.contains(id));
        }
    }

    fn drain_channels(&mut self, ctx: &egui::Context) {
        while let Ok(EngineEvent::Done(o)) = self.v.engine_rx.try_recv() {
            self.on_outcome(o);
        }
        self.drain_remote();
        while let Ok(ev) = self.index_rx.try_recv() {
            match ev {
                IndexEvent::Status(s) => self.status = s,
                IndexEvent::Message(m) => self.message = Some((m, Instant::now())),
                IndexEvent::Notice(m) => self.notice(m),
                IndexEvent::Compacted { old_instance, remap } => self.on_compacted(old_instance, &remap),
            }
        }
        for n in self.stores.take_notices() {
            self.notice(n);
        }
        while let Ok(m) = self.worker_rx.try_recv() {
            let text = match m {
                WorkerMsg::Done(s) | WorkerMsg::Failed(s) => s,
                WorkerMsg::Recorded(s, r) => {
                    self.on_recorded(r);
                    s
                }
            };
            self.message = Some((text, Instant::now()));
        }
        self.drain_host_events(ctx);
        let forwarded: Vec<Vec<String>> =
            self.instance.as_ref().map(|p| p.commands().try_iter().collect()).unwrap_or_default();
        for args in forwarded {
            let cli = crate::cli::parse(args);
            if !self.apply_cli(ctx, cli, true) {
                return;
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

    /// Applies a command line (start-up, or forwarded by a later launch). Returns `false`
    /// if Fennec is exiting.
    pub(super) fn apply_cli(&mut self, ctx: &egui::Context, cli: crate::cli::Cli, forwarded: bool) -> bool {
        use crate::cli::WindowAction;
        if cli.exit {
            self.quit(ctx);
            return false;
        }
        if forwarded && (!cli.file_lists.is_empty() || !cli.folders.is_empty()) {
            // Index sources are fixed at start-up.
            tracing::warn!("forwarded -filelist/-folder ignored by the running instance");
            self.say(t("msg-restart-required"));
        }
        if cli.save_db {
            self.indexer.save();
        }
        if cli.update {
            self.indexer.rescan_folders();
        }
        if forwarded && cli.new_window {
            self.open_window(cli.search.unwrap_or_default());
            return true;
        }
        if let Some(c) = &cli.connect {
            // Everything's `-connect [user:password@]host[:port]`.
            match fnc_etp::Target::parse(c, 21) {
                Some(target) => {
                    let link = self.settings.etp_client_link_type;
                    self.connect_etp(crate::remote::RemoteTarget { target, link });
                }
                None => self.say(t("msg-etp-bad-host")),
            }
        }
        if let Some(name) = &cli.filter {
            match self.filters.find(name) {
                Some(i) => self.set_filters(vec![i]),
                None if name.is_empty() || name.eq_ignore_ascii_case("everything") => self.set_filters(Vec::new()),
                None => self.say(t_args("msg-unknown-filter", &[("name", name.clone().into())])),
            }
        }
        if let Some(name) = &cli.bookmark {
            match self.bookmarks.list.iter().position(|b| &b.name == name) {
                Some(i) => self.open_bookmark(i),
                None => self.say(t_args("msg-unknown-bookmark", &[("name", name.clone().into())])),
            }
        }
        if let Some((col, asc)) = &cli.sort {
            let parsed = if col.is_empty() { Some(self.v.sort.column) } else { SortColumn::parse(col) };
            match parsed {
                Some(c) => {
                    let asc = asc.unwrap_or(matches!(c, SortColumn::Name | SortColumn::Path | SortColumn::Extension));
                    self.v.sort = SortSpec::by(c, asc, self.settings.folders_first);
                    self.send_search();
                }
                // Property columns (`-sort width`).
                None => match fnc_props::PropertyId::lookup(col) {
                    Some(p) => {
                        self.v.sort = SortSpec::by_property(p, asc.unwrap_or(false), self.settings.folders_first);
                        self.send_search();
                    }
                    None => self.say(t_args("msg-unknown-sort", &[("name", col.clone().into())])),
                },
            }
        }
        if let Some(f) = &cli.filename {
            // Explore in Fennec: the file's folder, with the file selected.
            // Split on both separators (paths come from Explorer or other platforms).
            let full = f.trim_end_matches(['\\', '/']);
            if let Some(cut) = full.rfind(['\\', '/']) {
                let dir = &full[..cut.max(1)];
                self.v.pending_select = Some(full.to_string());
                self.set_query(format!("parent:\"{dir}\""));
            }
        } else if let Some(s) = cli.search.filter(|_| forwarded) {
            self.set_query(s);
        }
        match cli.window {
            Some(WindowAction::Hide) => self.hide_window(ctx),
            Some(WindowAction::Show) => self.show_window(ctx),
            Some(WindowAction::Toggle) => {
                if self.hidden {
                    self.show_window(ctx);
                } else {
                    self.hide_window(ctx);
                }
            }
            Some(WindowAction::Close) => {
                if self.settings.close_to_tray && self.can_hide_to_tray() {
                    self.hide_window(ctx);
                } else {
                    self.quit(ctx);
                    return false;
                }
            }
            // A second `-startup` launch (e.g. autostart) must not pop the window up.
            None if forwarded && !cli.start_hidden => self.show_window(ctx),
            None => {}
        }
        true
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
        self.v.focus_search = true;
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

    /// Start-up notices about data files that were set aside (ADR-038): several in a row
    /// are shown together instead of replacing each other.
    pub(crate) fn notice(&mut self, text: String) {
        tracing::warn!("{text}");
        let text = match self.message.take() {
            Some((m, at)) if at.elapsed() < Duration::from_secs(8) => format!("{m}  ·  {text}"),
            _ => text,
        };
        self.message = Some((text, Instant::now()));
    }

    fn owner(&self) -> Owner {
        Owner(self.hwnd)
    }

    // ------------------------------------------------------------------ selection

    /// Entries the selection-based commands act on: the selection, or the cursor row.
    fn target_ids(&self) -> Vec<EntryId> {
        if self.v.sel.all {
            return self.v.results.ids.clone();
        }
        if !self.v.sel.ids.is_empty() {
            // In result order (stable for copy-as-text).
            return self.v.results.ids.iter().copied().filter(|id| self.v.sel.ids.contains(id)).collect();
        }
        self.v.selected.and_then(|i| self.v.results.ids.get(i).copied()).into_iter().collect()
    }

    /// Full paths of the target entries (roots get their trailing separator).
    fn target_paths(&self) -> Vec<(EntryId, PathBuf)> {
        let ids = self.target_ids();
        if ids.is_empty() {
            return Vec::new();
        }
        let Some(snap) = self.view_index().try_snapshot_for(Duration::from_millis(50)) else { return Vec::new() };
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
        self.v.sel.clear();
        if let Some(&id) = self.v.results.ids.get(i) {
            self.v.sel.ids.insert(id);
        }
        self.v.sel.anchor = Some(i);
        self.set_cursor(i);
    }

    fn set_cursor(&mut self, i: usize) {
        self.v.selected = Some(i);
        self.v.selected_id = self.v.results.ids.get(i).copied();
        self.v.scroll_to = Some(i);
    }

    fn select_range(&mut self, to: usize) {
        let n = self.v.results.len();
        if n == 0 {
            return;
        }
        let from = self.v.sel.anchor.or(self.v.selected).unwrap_or(to).min(n - 1);
        let (a, b) = (from.min(to), from.max(to).min(n - 1));
        self.v.sel.all = false;
        self.v.sel.ids = self.v.results.ids[a..=b].iter().copied().collect();
        self.v.sel.anchor = Some(from);
        self.set_cursor(to);
    }

    fn move_cursor(&mut self, delta: isize, mode: MoveMode) {
        let n = self.v.results.len();
        if n == 0 {
            return;
        }
        let cur = self.v.selected.map_or(-1, |s| s as isize);
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
        if self.remote_refuses(cmd) {
            return;
        }
        match cmd {
            K::FocusSearch => self.v.focus_search = true,
            K::Refresh if self.is_remote() => self.send_search(),
            K::Refresh => self.v.engine.refresh(),
            K::ConnectEtp => self.open_connect_dialog(),
            K::DisconnectEtp => self.disconnect_etp(),
            K::SemanticSetup => self.semantic_open_setup(),
            K::MoveDown => self.move_cursor(self.row_step(), MoveMode::Select),
            K::MoveUp => self.move_cursor(-self.row_step(), MoveMode::Select),
            K::ExtendDown => self.move_cursor(1, MoveMode::Extend),
            K::ExtendUp => self.move_cursor(-1, MoveMode::Extend),
            // Left/Right step through items in thumbnail views only.
            K::FocusNext if self.row_step() > 1 => self.move_cursor(1, MoveMode::Select),
            K::FocusPrev if self.row_step() > 1 => self.move_cursor(-1, MoveMode::Select),
            K::FocusNext | K::FocusPrev => {}
            K::FocusDown => self.move_cursor(1, MoveMode::FocusOnly),
            K::FocusUp => self.move_cursor(-1, MoveMode::FocusOnly),
            K::PageDown => self.move_cursor(page, MoveMode::Select),
            K::PageUp => self.move_cursor(-page, MoveMode::Select),
            K::First => {
                if !self.v.results.is_empty() {
                    self.select_only(0);
                }
            }
            K::Last => {
                if !self.v.results.is_empty() {
                    self.select_only(self.v.results.len() - 1);
                }
            }
            K::SelectAll => {
                self.v.sel.all = !self.v.results.is_empty();
                self.v.sel.ids.clear();
            }
            K::InvertSelection => {
                let all = self.v.sel.all;
                let cur: HashSet<EntryId> = std::mem::take(&mut self.v.sel.ids);
                self.v.sel.all = false;
                if !all {
                    self.v.sel.ids = self.v.results.ids.iter().copied().filter(|id| !cur.contains(id)).collect();
                }
            }
            K::ToggleSelection => {
                if let Some(id) = self.v.selected.and_then(|i| self.v.results.ids.get(i).copied()) {
                    if self.v.sel.all {
                        self.v.sel.all = false;
                        self.v.sel.ids = self.v.results.ids.iter().copied().collect();
                    }
                    if !self.v.sel.ids.remove(&id) {
                        self.v.sel.ids.insert(id);
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
                    self.paste_recorded(dest);
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
            // F2 on several items opens advanced rename (SPEC ch.3 §3).
            K::Rename if self.v.sel.count(self.v.results.len()) > 1 => self.open_advanced_rename(),
            K::Rename => self.start_rename(),
            K::AdvancedRename => self.open_advanced_rename(),
            K::Undo => self.undo_last(),
            K::NewTab => self.tab_new(String::new()),
            K::NavigateBack => self.navigate(true),
            K::NavigateForward => self.navigate(false),
            K::ToggleSidebar => self.toggle_setting(|s| &mut s.show_sidebar),
            K::ToggleToolbar => self.toggle_setting(|s| &mut s.show_toolbar),
            K::FindInResults if self.settings.findbar_ctrl_f => self.open_find_bar(),
            K::FindInResults => self.run(ctx, K::FocusSearch),
            K::FindNext => {
                if !self.find_step(true) {
                    self.run(ctx, K::FocusSearch);
                }
            }
            K::FindPrevious => {
                self.find_step(false);
            }
            K::NextTab => self.tab_step(1),
            K::PreviousTab => self.tab_step(-1),
            K::CloseTab => {
                if !self.tab_close() {
                    self.run(ctx, K::CloseWindow);
                }
            }
            K::Redo => self.redo_last(),
            K::NewFolder => {
                if let Some(parent) = self.paste_target() {
                    self.dialog = Some(dialogs::Dialog::NewFolder { parent, name: t("new-folder-name") });
                }
            }
            K::ExploreInFennec => {
                if let Some((id, p)) = self.target_paths().into_iter().next() {
                    let is_dir =
                        self.view_index().try_snapshot_for(Duration::from_millis(5)).is_some_and(|s| s.is_dir(id));
                    let folder = if is_dir { p.clone() } else { p.parent().map(PathBuf::from).unwrap_or(p) };
                    let mut s = folder.display().to_string();
                    if !s.ends_with(['\\', '/']) {
                        s.push(std::path::MAIN_SEPARATOR);
                    }
                    self.set_query(format!("\"{s}\" "));
                    self.v.focus_search = true;
                }
            }
            K::Omit => {
                let ids = self.target_ids();
                if !ids.is_empty() {
                    // Hidden until the search text changes (`omit:` lists them).
                    self.v.omit.extend(ids);
                    self.v.sel.clear();
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
                        name: self.v.query.clone(),
                        search: self.v.query.clone(),
                        filter: self.active_filter_name(),
                        sort: crate::columns::sort_setting(&self.v.sort),
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
                        name: self.v.query.clone(),
                        search: self.v.query.clone(),
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
            K::ImportSettings
            | K::ExportSettings
            | K::ImportEverythingIni
            | K::ExportFilters
            | K::ExportBookmarks
            | K::ExportHistory => {
                use dialogs::Transfer as T;
                let action = match cmd {
                    K::ImportSettings => T::ImportSettings,
                    K::ExportSettings => T::ExportSettings,
                    K::ImportEverythingIni => T::ImportEverythingIni,
                    K::ExportFilters => T::ExportFilters,
                    K::ExportBookmarks => T::ExportBookmarks,
                    _ => T::ExportHistory,
                };
                self.dialog = Some(dialogs::Dialog::Transfer { action, path: action.default_path() });
            }
            K::ToggleMenuBar => self.settings.show_menu_bar = !self.settings.show_menu_bar,
            K::ToggleStatusBar => self.settings.show_status_bar = !self.settings.show_status_bar,
            K::ToggleFilterBar => self.settings.show_filter_bar = !self.settings.show_filter_bar,
            K::TogglePreview => self.settings.show_preview = !self.settings.show_preview,
            K::ViewDetails | K::ViewMedium | K::ViewLarge | K::ViewExtraLarge => {
                use fnc_core::settings::ViewMode;
                self.settings.view_mode = match cmd {
                    K::ViewMedium => ViewMode::Medium,
                    K::ViewLarge => ViewMode::Large,
                    K::ViewExtraLarge => ViewMode::ExtraLarge,
                    _ => ViewMode::Details,
                };
                if let Some(i) = self.v.selected {
                    self.v.scroll_to = Some(i);
                }
            }
            K::ThemeLight | K::ThemeDark | K::ThemeSystem | K::ThemeHighContrast => {
                self.settings.theme = match cmd {
                    K::ThemeLight => Theme::Light,
                    K::ThemeDark => Theme::Dark,
                    K::ThemeHighContrast => Theme::HighContrast,
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
                self.v.focus_search = true;
                self.v.complete.open_history(&self.history);
            }
            K::ClearHistory => {
                self.history.clear();
                self.history.save();
                self.v.complete.close();
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
            K::RebuildIndex => self.rebuild_index(),
            K::PauseUpdates => self.toggle_pause(),
            K::IndexJournal => {
                let file = crate::filters::data_file(&self.settings_path, "journal.tsv");
                let mut records = crate::journal::read(&file, 10_000);
                records.reverse();
                self.dialog = Some(dialogs::Dialog::Journal { records, filter: String::new() });
            }
            K::Help => self.dialog = Some(dialogs::Dialog::Help),
            K::About => self.dialog = Some(dialogs::Dialog::About),
            K::HideWindow if self.in_secondary => self.close_secondary = true,
            K::NewWindow => {
                let q = self.v.query.clone();
                self.open_window(q);
            }
            K::CloseWindow if self.in_secondary => self.close_secondary = true,
            K::CloseWindow => self.run(ctx, Command::HideWindow),
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
        if !permanent {
            self.recycle_paths(paths);
            return;
        }
        let owner = self.owner();
        let n = paths.len();
        self.spawn_op(t_args("msg-deleted", &[("count", n.into())]), move || {
            fnc_platform::fileops::delete(owner, &paths, permanent).map(|_| ())
        });
    }

    /// Up/Down move one row: one entry in details view, a row of the grid otherwise.
    fn row_step(&self) -> isize {
        if self.settings.view_mode == fnc_core::settings::ViewMode::Details {
            1
        } else {
            self.v.grid_cols.max(1) as isize
        }
    }

    fn toggle_setting(&mut self, f: impl Fn(&mut Settings) -> &mut bool) {
        let v = f(&mut self.settings);
        *v = !*v;
        self.rebuild_search_options();
        self.send_search();
    }

    fn active_filter_name(&self) -> String {
        self.v
            .active_filters
            .first()
            .and_then(|&i| self.filters.list.get(i))
            .map(|f| f.name.clone())
            .unwrap_or_default()
    }

    fn set_filters(&mut self, active: Vec<usize>) {
        self.v.active_filters = active;
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
        if self.remote_open(reveal) {
            return;
        }
        let targets = self.target_paths();
        // Opening many files at once is almost always a mistake.
        for (id, path) in targets.into_iter().take(if reveal { 1 } else { 32 }) {
            let r = if reveal { fnc_platform::shell::reveal(&path) } else { fnc_platform::shell::open(&path) };
            match r {
                Ok(()) => {
                    self.record_history();
                    // Remote results have no run history here.
                    if !reveal && self.settings.run_history_enabled && !self.is_remote() {
                        // The write lock may wait for a running search: never take it on
                        // the UI thread.
                        let (shared, engine) = (self.shared.clone(), self.v.engine.handle());
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
        if matches!(kind, TextKind::Csv | TextKind::Efu) {
            // Same writers as File → Export and fes.
            let Some(snap) = self.view_index().try_snapshot_for(Duration::from_millis(50)) else { return };
            let ids: Vec<EntryId> = targets.iter().map(|(id, _)| *id).collect();
            let format = if kind == TextKind::Efu { fnc_ipc::Format::Efu } else { fnc_ipc::Format::Csv };
            ctx.copy_text(crate::export::to_text(format, &snap, &ids, Some(self.type_names.clone())));
            self.say(t("msg-copied"));
            return;
        }
        let snap = self.view_index().try_snapshot_for(Duration::from_millis(50));
        let mut out = String::new();
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
                    let size = snap
                        .as_ref()
                        .and_then(|s| s.size(*id).filter(|_| !s.is_dir(*id)))
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    format!("{name}\t{folder}\t{size}")
                }
            };
            out.push_str(&line);
            out.push_str("\r\n");
        }
        if targets.len() == 1 {
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
        if let Some(i) = self.v.results.ids.iter().position(|&r| r == id) {
            self.v.scroll_to = Some(i);
        }
        self.v.rename = Some(RenameState { id, path, text, focus: true });
    }

    fn commit_rename(&mut self) {
        let Some(r) = self.v.rename.take() else { return };
        let old = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let new = r.text.trim().to_string();
        if new.is_empty() || new == old {
            return;
        }
        self.rename_recorded(r.path, new);
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
        let search_has_focus = ctx.memory(|m| m.has_focus(self.search_id()));
        if search_has_focus {
            self.v.focus = Focus::Search;
        }
        let kctx = if self.v.focus == Focus::Search { KeyCtx::Search } else { KeyCtx::Results };
        let modal = self.dialog.is_some() || self.v.rename.is_some() || self.options_window.is_some();
        let mut cmds = Vec::new();
        let mut esc = false;
        let mut tab = false;
        let mut typed = String::new();
        let complete_open = self.v.complete.is_open();
        let mut complete_keys = Vec::new();
        let mut paste_seen = false;
        let mut shift_dels = fnc_platform::shell::take_shift_delete();
        let type_to_search = self.settings.type_to_search;
        // Enter runs a `/key=value` inline command (SPEC ch.3 §13) instead of opening.
        let inline_command = kctx == KeyCtx::Search && self.v.query.starts_with('/');
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
                        if inline_command && *key == Key::Enter && !modifiers.any() {
                            keep.push(e);
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
                if let Some(text) = self.v.complete.selected_history() {
                    self.history.remove(&text);
                    self.v.complete.remove_selected();
                }
                continue;
            }
            if let Some(q) = self.v.complete.key(k, &self.v.query) {
                self.v.query = q;
                self.v.edited_at = Some(Instant::now());
                self.v.keep_focus = true;
                self.move_search_cursor_to_end(ctx);
            }
        }
        if esc {
            self.on_escape(ctx);
        }
        if tab {
            self.v.focus = if self.v.focus == Focus::Search { Focus::Results } else { Focus::Search };
            if self.v.focus == Focus::Search {
                self.v.keep_focus = true;
            } else {
                ctx.memory_mut(|m| m.surrender_focus(self.search_id()));
                if self.v.selected.is_none() && !self.v.results.is_empty() {
                    self.select_only(0);
                }
            }
        }
        if !typed.is_empty() {
            // Type-to-search: continue typing in the search box.
            self.v.query.push_str(&typed);
            self.v.edited_at = Some(Instant::now());
            self.v.focus = Focus::Search;
            self.v.keep_focus = true;
            self.move_search_cursor_to_end(ctx);
        }
        for c in cmds {
            self.run(ctx, c);
        }
    }

    fn move_search_cursor_to_end(&self, ctx: &egui::Context) {
        let id = self.search_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let end = egui::text::CCursor::new(self.v.query.chars().count());
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
        if self.v.rename.take().is_some() {
            return;
        }
        if self.v.complete.is_open() {
            self.v.complete.close();
            self.v.keep_focus = true;
            return;
        }
        if self.v.query.is_empty() {
            self.run(ctx, Command::HideWindow);
        } else {
            self.set_query(String::new());
        }
        // egui drops text focus on Escape; Everything keeps typing in the search box.
        self.v.focus = Focus::Search;
        self.v.keep_focus = true;
    }

    // ------------------------------------------------------------------ widgets

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        let id = self.search_id();
        let precedence = self.v.search_opts.precedence;
        let filter_w = 150.0;
        let mut enter = false;
        ui.horizontal(|ui| {
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                let job = syntax_job(ui, buf.as_str(), precedence);
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let edit = egui::TextEdit::singleline(&mut self.v.query)
                .id(id)
                .hint_text(t("search-hint"))
                .desired_width((ui.available_width() - filter_w - 16.0).max(100.0))
                .font(egui::TextStyle::Body)
                .layouter(&mut layouter);
            let resp = ui.add(edit);
            if self.v.keep_focus {
                self.v.keep_focus = false;
                resp.request_focus();
            }
            if self.v.focus_search {
                self.v.focus_search = false;
                self.v.focus = Focus::Search;
                resp.request_focus();
                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(self.v.query.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), id);
            }
            if resp.gained_focus() {
                self.v.focus = Focus::Search;
            }
            enter = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if resp.changed() {
                self.v.edited_at = Some(Instant::now());
                if self.settings.auto_complete {
                    let cursor = egui::TextEdit::load_state(ui.ctx(), id)
                        .and_then(|s| s.cursor.char_range())
                        .map_or(self.v.query.chars().count(), |r| r.primary.index.into());
                    let snap = self.shared.try_snapshot_for(Duration::from_millis(2));
                    self.v.complete.update(&self.v.query, cursor, &self.history, snap.as_ref());
                }
            }
            if let Some(q) = self.v.complete.show(ui, &resp, &self.v.query) {
                self.v.query = q;
                self.v.edited_at = Some(Instant::now());
                self.v.keep_focus = true;
                self.move_search_cursor_to_end(ui.ctx());
            }
            if !resp.has_focus() && !self.v.complete.hovered() {
                self.v.complete.close();
            }
            self.semantic_toggle(ui);
            self.filter_combo(ui, filter_w);
        });
        // `/key=value` and `/command` (SPEC ch.3 §13) run on Enter.
        if self.v.query.starts_with('/') && (enter || ui.input(|i| i.key_pressed(Key::Enter))) {
            let cmd = std::mem::take(&mut self.v.query);
            self.inline_command(ui.ctx(), &cmd);
            self.send_search();
        }
        // `search_delay` (default 0 ms, like Everything).
        if let Some(at) = self.v.edited_at {
            let delay = Duration::from_millis(u64::from(self.settings.search_delay));
            if self.v.query.starts_with('/') {
                self.v.edited_at = None;
            } else if at.elapsed() >= delay {
                if self.v.sent_query.as_deref() != Some(self.v.query.as_str()) {
                    self.send_search();
                } else {
                    self.v.edited_at = None;
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
                    // Never echo passwords (`http_password`).
                    let shown = if key.trim().ends_with("password") { "••••" } else { value.trim() };
                    self.say(t_args(
                        "msg-setting-changed",
                        &[("key", key.trim().to_string().into()), ("value", shown.to_string().into())],
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
            let v = if body.ends_with("password") { "••••".to_string() } else { v };
            self.say(format!("{body} = {v}"));
        } else {
            self.say(t_args("msg-unknown-command", &[("command", body.to_string().into())]));
        }
    }

    fn filter_combo(&mut self, ui: &mut egui::Ui, width: f32) {
        let current = if self.v.active_filters.is_empty() {
            t("filter-everything")
        } else {
            self.v
                .active_filters
                .iter()
                .filter_map(|&i| self.filters.list.get(i))
                .map(|f| f.name.clone())
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let mut choice: Option<Option<usize>> = None;
        let mut organize = false;
        egui::ComboBox::from_id_salt("fennec-filter").selected_text(current).width(width).show_ui(ui, |ui| {
            if ui.selectable_label(self.v.active_filters.is_empty(), t("filter-everything")).clicked() {
                choice = Some(None);
            }
            for (i, f) in self.filters.list.iter().enumerate() {
                if ui.selectable_label(self.v.active_filters.contains(&i), &f.name).clicked() {
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
            if ui.selectable_label(self.v.active_filters.is_empty(), t("filter-everything")).clicked() {
                toggle = Some((None, false));
            }
            for (i, f) in self.filters.list.iter().enumerate() {
                let r = ui.selectable_label(self.v.active_filters.contains(&i), &f.name);
                if r.clicked() {
                    toggle = Some((Some(i), ui.input(|inp| inp.modifiers.ctrl)));
                }
            }
        });
        match toggle {
            Some((None, _)) => self.set_filters(Vec::new()),
            // Ctrl+click combines filters (any of them matches).
            Some((Some(i), true)) => {
                let mut a = self.v.active_filters.clone();
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
            let n = format::group_digits(self.v.results.len() as u64);
            ui.label(t_args("status-objects", &[("count", n.into())]));
            self.remote_status_ui(ui);
            let sel = self.v.sel.count(self.v.results.len());
            if sel > 0 {
                ui.separator();
                let mut text = t_args("status-selected", &[("count", format::group_digits(sel as u64).into())]);
                if !self.v.sel.all
                    && sel <= 10_000
                    && let Some(s) = self.view_index().try_snapshot_for(Duration::from_millis(1))
                {
                    let total: u64 = self
                        .v
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
            ui.label(t_args(
                "status-elapsed",
                &[("ms", format!("{:.1}", self.v.elapsed.as_secs_f64() * 1000.0).into())],
            ));
            if !self.v.active_filters.is_empty() {
                ui.separator();
                ui.label(self.active_filter_name());
            }
            if let Some(d) = self.v.diagnostics.first() {
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
                // Property/content extraction (SPEC ch.3 §4.6).
                if let Some(p) = self.stores.progress() {
                    ui.separator();
                    ui.label(t_args(
                        "index-extracting",
                        &[
                            ("done", format::group_digits(p.done as u64).into()),
                            ("total", format::group_digits(p.total as u64).into()),
                        ],
                    ));
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                }
                if let Some(sem) = self.semantic_status() {
                    ui.separator();
                    if ui.link(sem).clicked() {
                        self.semantic_open_setup();
                    }
                }
                self.server_status_ui(ui);
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
            self.v.rows.clear();
        }
        if old.custom_columns != s.custom_columns {
            crate::columns::set_custom_columns(&s.custom_columns);
            self.v.rows.clear();
        }
        if old.date_input_order != s.date_input_order || old.week_start != s.week_start {
            crate::i18n::apply_date_locale(s.date_input_order, s.week_start);
        }
        if old.name_collation != s.name_collation {
            crate::i18n::apply_collation(s.name_collation);
            // Ranks were built with the old order: rebuild them off the UI thread.
            let shared = self.shared.clone();
            let _ = std::thread::Builder::new().name("fennec-rank".into()).spawn(move || {
                let (fresh, generation) = {
                    let snap = shared.snapshot();
                    (fnc_index::NameOrder::build(&snap), snap.generation())
                };
                shared.write().set_name_order(fresh, generation);
            });
            self.v.rows.clear();
        }
        let s = &self.settings;
        if old.sort != s.sort || old.folders_first != s.folders_first {
            self.v.sort = sort_from_settings(s);
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
            self.v.rows.clear();
        }
        self.stores.configure(s);
        // What is indexed changed: rebuild in the background (searches keep the old
        // index until the new one is published).
        let sources = old.ntfs_auto_include != s.ntfs_auto_include
            || old.ntfs_exclude_volumes != s.ntfs_exclude_volumes
            || old.folders != s.folders
            || old.file_lists != s.file_lists
            || old.index_date_accessed != s.index_date_accessed
            || old.db_save_interval != s.db_save_interval
            || old.exclude_hidden != s.exclude_hidden
            || old.exclude_system != s.exclude_system
            || old.exclude_folders != s.exclude_folders
            || old.exclude_files != s.exclude_files
            || old.exclude_gitignore_roots != s.exclude_gitignore_roots
            || old.folder_watch != s.folder_watch
            || old.folder_rescan_interval != s.folder_rescan_interval;
        if sources && self.reindex.is_some() {
            self.rebuild_index();
        } else if sources || old.language != s.language {
            self.say(t("msg-restart-required"));
        }
        self.semantic_apply(&old);
        self.apply_server_settings();
        self.rebuild_search_options();
        self.send_search();
        if let Err(e) = self.settings.save(&self.settings_path) {
            tracing::warn!(error = %e, "cannot save settings");
        }
    }

    /// Persists window-derived state (columns, sort) into the settings.
    fn sync_view_settings(&mut self) {
        self.settings.columns = crate::columns::columns_setting(&self.columns);
        self.settings.sort = crate::columns::sort_setting(&self.v.sort);
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
    crate::columns::sort_spec(&s.sort, s.folders_first)
        .unwrap_or_else(|| SortSpec::by(SortColumn::Name, true, s.folders_first))
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
    // Warnings are also rendered into `errors`; show them localised instead.
    let warnings = &o.results.stats.warnings;
    let rendered: Vec<String> = warnings.iter().map(ToString::to_string).collect();
    for e in o.results.stats.errors.iter().filter(|e| !rendered.contains(e)) {
        v.push(t_args("diag-pattern", &[("message", e.clone().into())]));
    }
    v.extend(warnings.iter().map(warning_text));
    v
}

/// Localised text of a search warning (`ExecStats::warnings`).
fn warning_text(w: &fnc_exec::ExecWarning) -> String {
    match w {
        fnc_exec::ExecWarning::NotIndexed(name) => t_args("warn-not-indexed", &[("name", (*name).into())]),
        fnc_exec::ExecWarning::SlowLimit(n) => {
            t_args("warn-slow-limit", &[("count", format::group_digits(*n as u64).into())])
        }
        fnc_exec::ExecWarning::Semantic(reason) => t_args("warn-semantic", &[("reason", reason.clone().into())]),
        fnc_exec::ExecWarning::NoFileAccess(name) => t_args("warn-no-file-access", &[("name", (*name).into())]),
        fnc_exec::ExecWarning::SystemIndexUnavailable => t("warn-system-index-unavailable"),
        fnc_exec::ExecWarning::SystemIndexFailed(reason) => {
            t_args("warn-system-index-failed", &[("reason", reason.clone().into())])
        }
        fnc_exec::ExecWarning::SystemIndexLimit(n) => {
            t_args("warn-system-index-limit", &[("count", format::group_digits(*n as u64).into())])
        }
    }
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    // Reset the dark palette first (it may hold the high-contrast one).
    ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals::dark());
    ctx.set_theme(match theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark | Theme::HighContrast => egui::ThemePreference::Dark,
    });
    if theme == Theme::HighContrast {
        ctx.set_visuals_of(egui::Theme::Dark, high_contrast_visuals());
    }
}

/// Black background, white text, yellow selection, thick outlines.
fn high_contrast_visuals() -> egui::Visuals {
    use egui::{Color32, Stroke};
    let mut v = egui::Visuals::dark();
    let (fg, sel) = (Color32::WHITE, Color32::from_rgb(255, 255, 0));
    v.override_text_color = Some(fg);
    v.panel_fill = Color32::BLACK;
    v.window_fill = Color32::BLACK;
    v.extreme_bg_color = Color32::BLACK;
    v.faint_bg_color = Color32::from_gray(24);
    v.hyperlink_color = Color32::from_rgb(0, 255, 255);
    v.warn_fg_color = sel;
    v.error_fg_color = Color32::from_rgb(255, 96, 96);
    v.selection.bg_fill = sel;
    v.selection.stroke = Stroke::new(2.0, Color32::BLACK);
    v.window_stroke = Stroke::new(2.0, fg);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke = Stroke::new(1.5, fg);
        w.bg_stroke = Stroke::new(1.5, fg);
        w.bg_fill = Color32::BLACK;
        w.weak_bg_fill = Color32::BLACK;
    }
    v.widgets.hovered.bg_fill = Color32::from_gray(40);
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(40);
    v.widgets.active.bg_fill = Color32::from_rgb(0, 0, 160);
    v
}

impl FennecApp {
    /// The window body: menu bar (main window), search box, filter bar, status bar,
    /// preview and results.
    pub(super) fn draw_main(&mut self, ui: &mut egui::Ui, main: bool) {
        let ctx = ui.ctx().clone();
        let alt_only = ctx.input(|i| i.modifiers.alt && !i.modifiers.ctrl && !i.modifiers.shift);
        self.menu_peek =
            !self.settings.show_menu_bar && (alt_only || (self.menu_peek && egui::Popup::is_any_open(&ctx)));
        if main && (self.settings.show_menu_bar || self.menu_peek) {
            egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        }
        egui::Panel::top("search").show(ui, |ui| {
            ui.add_space(4.0);
            if self.settings.show_toolbar {
                self.toolbar(ui);
            }
            self.tab_bar(ui);
            self.search_bar(ui);
            if self.settings.show_filter_bar {
                self.filter_bar(ui);
            }
            ui.add_space(2.0);
        });
        if self.settings.show_status_bar {
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        }
        if self.v.find.open {
            egui::Panel::bottom("find-bar").show(ui, |ui| self.find_bar(ui));
        }
        self.thumbs.poll(&ctx);
        if self.settings.show_sidebar {
            let r = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(self.settings.sidebar_width.clamp(120.0, 800.0))
                .size_range(120.0..=800.0)
                .show(ui, |ui| self.sidebar(ui));
            self.settings.sidebar_width = r.response.rect.width();
        }
        if self.settings.show_preview {
            let r = egui::Panel::right("preview")
                .resizable(true)
                .default_size(self.settings.preview_width.clamp(160.0, 1200.0))
                .size_range(160.0..=1200.0)
                .show(ui, |ui| self.preview_panel(ui));
            self.settings.preview_width = r.response.rect.width();
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.settings.view_mode == fnc_core::settings::ViewMode::Details {
                self.results_table(ui);
            } else {
                self.results_grid(ui);
            }
        });
    }

    fn search_id(&self) -> egui::Id {
        egui::Id::new((SEARCH_ID, self.v.uid))
    }
}

/// Searches unchanged for this long count as "used" (search history).
const HISTORY_IDLE: Duration = Duration::from_secs(3);

impl eframe::App for FennecApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_channels(ctx);
        self.refresh_if_stale();
        self.semantic_tick(ctx);
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.settings.close_to_tray
            && self.can_hide_to_tray()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
        if !self.v.history_recorded
            && self.v.edited_at.is_none()
            && !self.v.results.is_empty()
            && self.v.sent_query.as_deref() == Some(self.v.query.as_str())
        {
            match self.v.history_idle_since {
                Some(at) if at.elapsed() >= HISTORY_IDLE => self.record_history(),
                Some(at) => ctx.request_repaint_after(HISTORY_IDLE.saturating_sub(at.elapsed())),
                None => self.v.history_idle_since = Some(Instant::now()),
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
        self.draw_main(ui, true);
        self.dialogs(&ctx);
        self.options_viewport(&ctx);
        self.semantic_setup_window(&ctx);
        self.advanced_rename_window(&ctx);
        for cmd in std::mem::take(&mut self.pending) {
            self.run(&ctx, cmd);
        }
        self.after_table(&ctx);
        self.secondary_windows(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for w in &mut self.windows {
            w.close();
        }
        if let Some((rect, maximized)) = self.window_geometry {
            self.settings.window_maximized = maximized;
            if !maximized {
                self.settings.window_rect =
                    format!("{:.0},{:.0},{:.0},{:.0}", rect.min.x, rect.min.y, rect.width(), rect.height());
            }
        }
        self.save_session();
        self.semantic_stop();
        self.stop_servers();
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
#[cfg(test)]
mod tests_edit;
#[cfg(test)]
mod tests_etp;
#[cfg(all(test, feature = "semantic"))]
mod tests_semantic;
#[cfg(test)]
mod tests_tabs;

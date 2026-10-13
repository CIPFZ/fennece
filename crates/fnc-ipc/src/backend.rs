//! Query execution for IPC, the Everything compatibility layer and HTTP: an
//! [`IndexBackend`] runs searches itself on index snapshots (parse → plan → execute → sort
//! → page → columns) without touching the UI's search engine, and keeps the last result
//! sets for paging.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fnc_core::EntryId;
use fnc_core::settings::FoldersFirst;
use fnc_exec::{Caches, CancelToken, ExecCtx, Highlighter, SearchOptions, SortSpec, execute, sort_ids};
use fnc_index::{IndexStore, SharedIndex};
use fnc_query::{Expr, ModSet, Modifier, ParseEnv, PlanEnv, PlannedQuery, Term, Value, parse, plan};
use parking_lot::Mutex;

use crate::proto::{
    BookmarkEntry, Column, DEFAULT_MAX_RESULTS, FilterEntry, IndexStatus, Mods, Op, Payload, Query, ResultColumns,
    Results, SearchSpec, Status, VersionInfo, VolumeEntry,
};
use crate::rows::{ColumnSet, entry_row, resolve_path};

/// Administrative commands forwarded to the [`Host`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AdminCommand {
    /// Rescan every index (`EVERYTHING_IPC_REBUILD_DB`, `-reindex`).
    Rebuild,
    /// Rescan folder indexes (`EVERYTHING_IPC_UPDATE_ALL_FOLDER_INDEXES`).
    Update,
    /// Save the database (`EVERYTHING_IPC_SAVE_DB`).
    SaveDb,
    /// Save the run history.
    SaveRunHistory,
    /// Delete the run history.
    DeleteRunHistory,
    /// Exit the application.
    Exit,
}

/// Where a request came from (for [`Host::authorize`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PeerKind {
    /// Native local socket.
    Pipe,
    /// Everything 1.4 `WM_COPYDATA` / `EVERYTHING_WM_IPC`.
    CopyData,
    /// HTTP server.
    Http,
    /// In-process caller.
    Local,
}

/// The requesting client.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Transport.
    pub kind: PeerKind,
    /// Client process id when the transport reports one (named pipes, Unix sockets).
    pub pid: Option<u32>,
}

impl Peer {
    /// An in-process caller.
    pub const LOCAL: Peer = Peer { kind: PeerKind::Local, pid: None };
}

/// Host failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The host does not implement the command.
    #[error("not supported")]
    Unsupported,
    /// The client may not run the command.
    #[error("access denied")]
    Denied,
    /// The command failed.
    #[error("{0}")]
    Failed(String),
}

/// Things only the application can answer or do. Every method has a conservative default,
/// so tests and headless hosts implement only what they need. Methods are called from IPC
/// connection threads, the Win32 window thread (compatibility layer) and HTTP worker threads;
/// they must not block for long.
pub trait Host: Send + Sync {
    /// Search options (precedence, macros, `*` semantics, pinyin, type names). Match
    /// case/whole word/path/diacritics/regex always come from the request.
    fn search_options(&self) -> SearchOptions {
        SearchOptions::default()
    }

    /// Folder placement when sorting.
    fn folders_first(&self) -> FoldersFirst {
        FoldersFirst::Ascending
    }

    /// The database is loaded (initial indexing done).
    fn is_db_loaded(&self) -> bool {
        true
    }

    /// Indexing/rebuilding is in progress.
    fn is_busy(&self) -> bool {
        false
    }

    /// The application runs elevated.
    fn is_admin(&self) -> bool {
        false
    }

    /// Settings and data are stored in `%APPDATA%` (not portable).
    fn is_appdata(&self) -> bool {
        true
    }

    /// Free-form status text for [`IndexStatus::message`].
    fn status_text(&self) -> String {
        String::new()
    }

    /// Decides whether `peer` may run `cmd` (the pipe DACL already limits callers to
    /// SYSTEM, administrators and interactive users; hosts may additionally check the
    /// client's session id / integrity level from `peer.pid`).
    fn authorize(&self, cmd: AdminCommand, peer: &Peer) -> bool {
        let _ = (cmd, peer);
        true
    }

    /// Runs an administrative command. Should return quickly (queue the work).
    fn command(&self, cmd: AdminCommand) -> Result<(), HostError> {
        let _ = cmd;
        Err(HostError::Unsupported)
    }

    /// Called after a run count changed through IPC (the backend already updated the
    /// index; the host may persist and refresh its view).
    fn run_count_changed(&self) {}

    /// Filters (Search → Organize Filters).
    fn filters(&self) -> Vec<FilterEntry> {
        Vec::new()
    }

    /// Bookmarks.
    fn bookmarks(&self) -> Vec<BookmarkEntry> {
        Vec::new()
    }

    /// `EVERYTHING_IPC_COPYDATA_COMMAND_LINE_UTF8`: an Everything-style command line
    /// (`-s abc`) with a `SW_*` show command. Returns `true` if handled.
    fn command_line(&self, show_command: u32, command_line: &str) -> bool {
        let _ = (show_command, command_line);
        false
    }

    /// Version reported to Everything 1.4 clients (`major, minor, revision, build`).
    fn everything_version(&self) -> [u32; 4] {
        crate::compat::DEFAULT_EVERYTHING_VERSION
    }
}

/// A host with all defaults (headless use, tests).
#[derive(Default, Debug, Clone, Copy)]
pub struct DefaultHost;

impl Host for DefaultHost {}

/// Backend limits.
#[derive(Clone, Debug)]
pub struct BackendConfig {
    /// Most rows per response (default [`DEFAULT_MAX_RESULTS`]).
    pub max_results: u64,
    /// Result sets kept for [`Op::QueryContinue`].
    pub cached_queries: usize,
    /// Memory budget of the kept result sets (bytes of entry ids).
    pub cache_bytes: usize,
    /// Time limit when the request gives none.
    pub default_timeout: Duration,
    /// Upper bound for request time limits.
    pub max_timeout: Duration,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_MAX_RESULTS,
            cached_queries: 16,
            cache_bytes: 64 << 20,
            default_timeout: Duration::from_secs(30),
            max_timeout: Duration::from_secs(300),
        }
    }
}

/// Request failure with a protocol [`Status`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{status:?}: {message}")]
pub struct IpcError {
    /// Status.
    pub status: Status,
    /// Message.
    pub message: String,
}

impl IpcError {
    /// New error.
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
}

impl From<HostError> for IpcError {
    fn from(e: HostError) -> Self {
        let status = match e {
            HostError::Unsupported => Status::Unsupported,
            HostError::Denied => Status::Denied,
            HostError::Failed(_) => Status::Internal,
        };
        IpcError::new(status, e.to_string())
    }
}

/// Identity of a search (the cache key).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchKey {
    /// Search text.
    pub search: String,
    /// Options.
    pub mods: Mods,
    /// Sort.
    pub sort: SortSpec,
}

/// A computed result set (shared by every query id that pages it).
#[derive(Debug)]
pub struct ResultSetRef {
    /// What was searched.
    pub key: SearchKey,
    /// Matching entries in sort order.
    pub ids: Arc<Vec<EntryId>>,
    /// Plan (for highlighting).
    pub plan: Arc<PlannedQuery>,
    /// Index instance the ids belong to.
    pub instance: u64,
    /// Index generation the ids were computed for.
    pub generation: u64,
}

impl ResultSetRef {
    /// Number of results.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn is_current(&self, s: &IndexStore) -> bool {
        self.instance == s.instance() && self.generation == s.generation()
    }
}

/// A query id: a result set plus the view (columns, highlighting) its pages use.
struct Entry {
    query_id: u64,
    rs: Arc<ResultSetRef>,
    columns: Vec<Column>,
    highlight: bool,
    used: Instant,
}

struct Cache {
    entries: Vec<Entry>,
}

impl Cache {
    /// Bytes of distinct result sets.
    fn bytes(&self) -> usize {
        let mut seen: Vec<*const ResultSetRef> = Vec::new();
        let mut total = 0;
        for e in &self.entries {
            let p = Arc::as_ptr(&e.rs);
            if !seen.contains(&p) {
                seen.push(p);
                total += e.rs.ids.len() * std::mem::size_of::<EntryId>();
            }
        }
        total
    }
}

/// Executes queries on an index snapshot for every non-UI front end.
pub struct IndexBackend {
    shared: SharedIndex,
    host: Arc<dyn Host>,
    caches: Caches,
    config: BackendConfig,
    cache: Mutex<Cache>,
    next_id: AtomicU64,
}

/// Converts the wire sort keys into a [`SortSpec`] (empty = name ascending).
pub fn sort_spec(keys: &[(Column, bool)], folders_first: FoldersFirst) -> SortSpec {
    let mut it = keys.iter().take(3);
    let (col, asc) = it.next().copied().unwrap_or((Column::Name, true));
    let mut spec = SortSpec::by(col.sort_column(), asc, folders_first);
    for (slot, &(c, a)) in spec.then.iter_mut().zip(it) {
        *slot = Some((c.sort_column(), a));
    }
    spec
}

/// Builds the AST of a search: the text parsed with `options`, or one regex term when
/// `mods.regex`.
pub fn search_ast(search: &str, mods: &Mods, options: &SearchOptions) -> Expr {
    if mods.regex && !search.is_empty() {
        return Expr::Term(Term {
            mods: ModSet::EMPTY.with(Modifier::Regex, true),
            func: None,
            value: Value::Text(search.to_string()),
            raw: 0..search.len(),
        });
    }
    let lookup = |name: &str| std::env::var(name).ok();
    let env = ParseEnv { precedence: options.precedence, user_macros: &options.macros, env_var: Some(&lookup) };
    parse(search, &env).ast
}

/// Default modifiers of a request: the request's flags override the host defaults.
pub fn request_mods(mods: &Mods, host_defaults: ModSet) -> ModSet {
    let mut d = host_defaults;
    d.set(Modifier::Case, mods.case);
    d.set(Modifier::WholeWord, mods.whole_word);
    d.set(Modifier::Path, mods.path);
    d.set(Modifier::Diacritics, mods.diacritics);
    for (m, on) in [
        (Modifier::Prefix, mods.prefix),
        (Modifier::Suffix, mods.suffix),
        (Modifier::IgnorePunctuation, mods.ignore_punctuation),
        (Modifier::IgnoreWhitespace, mods.ignore_whitespace),
    ] {
        if on {
            d.set(m, true);
        }
    }
    d
}

/// Cancels `token` after `timeout` unless dropped first.
struct Watchdog {
    done: Option<crossbeam_channel::Sender<()>>,
}

impl Watchdog {
    fn start(token: CancelToken, timeout: Duration) -> Self {
        let (tx, rx) = crossbeam_channel::bounded::<()>(1);
        let spawned = std::thread::Builder::new().name("fennec-ipc-watchdog".into()).spawn(move || {
            if let Err(crossbeam_channel::RecvTimeoutError::Timeout) = rx.recv_timeout(timeout) {
                token.cancel();
            }
        });
        if spawned.is_err() {
            tracing::warn!("cannot spawn query watchdog; query runs without time limit");
        }
        Self { done: Some(tx) }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(tx) = self.done.take() {
            let _ = tx.send(());
        }
    }
}

impl IndexBackend {
    /// Creates a backend over the application's index (`shared`) and its [`Host`].
    pub fn new(shared: SharedIndex, host: Arc<dyn Host>) -> Self {
        Self::with_config(shared, host, BackendConfig::default())
    }

    /// Creates a backend with explicit limits.
    pub fn with_config(shared: SharedIndex, host: Arc<dyn Host>, config: BackendConfig) -> Self {
        Self {
            shared,
            host,
            caches: Caches::new(),
            config,
            cache: Mutex::new(Cache { entries: Vec::new() }),
            next_id: AtomicU64::new(1),
        }
    }

    /// The host.
    pub fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    /// The index.
    pub fn index(&self) -> &SharedIndex {
        &self.shared
    }

    /// Limits.
    pub fn config(&self) -> &BackendConfig {
        &self.config
    }

    /// Caches shared by this backend's queries (type names, folder paths, ...).
    pub fn caches(&self) -> &Caches {
        &self.caches
    }

    fn timeout(&self, ms: u32) -> Duration {
        if ms == 0 {
            self.config.default_timeout
        } else {
            Duration::from_millis(u64::from(ms)).min(self.config.max_timeout)
        }
    }

    fn evict(&self, c: &mut Cache) {
        while c.entries.len() > self.config.cached_queries.max(1)
            || (c.entries.len() > 1 && c.bytes() > self.config.cache_bytes)
        {
            let oldest = c.entries.iter().enumerate().min_by_key(|(_, e)| e.used).map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    c.entries.remove(i);
                }
                None => break,
            }
        }
    }

    /// Registers a view of `rs` and returns its query id (reusing an id that shows the same
    /// result set with the same view, or a fresh id from [`Self::search`]).
    fn register(&self, rs: &Arc<ResultSetRef>, columns: &[Column], highlight: bool) -> u64 {
        let mut c = self.cache.lock();
        let now = Instant::now();
        if let Some(e) =
            c.entries.iter_mut().find(|e| Arc::ptr_eq(&e.rs, rs) && e.columns == columns && e.highlight == highlight)
        {
            e.used = now;
            return e.query_id;
        }
        if let Some(e) = c.entries.iter_mut().find(|e| Arc::ptr_eq(&e.rs, rs) && e.columns.is_empty()) {
            e.columns = columns.to_vec();
            e.highlight = highlight;
            e.used = now;
            return e.query_id;
        }
        let query_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        c.entries.push(Entry { query_id, rs: rs.clone(), columns: columns.to_vec(), highlight, used: now });
        self.evict(&mut c);
        query_id
    }

    /// Drops a cached query id.
    pub fn release(&self, query_id: u64) -> bool {
        let mut c = self.cache.lock();
        let before = c.entries.len();
        c.entries.retain(|e| e.query_id != query_id);
        c.entries.len() != before
    }

    /// Runs (or reuses) a search. Result sets are cached by search text, options and sort
    /// while the index is unchanged (same instance and generation).
    pub fn search(&self, key: &SearchKey, timeout: Duration) -> Result<Arc<ResultSetRef>, IpcError> {
        {
            let snap = self.shared.snapshot();
            let mut c = self.cache.lock();
            if let Some(e) = c.entries.iter_mut().find(|e| e.rs.key == *key && e.rs.is_current(&snap)) {
                e.used = Instant::now();
                return Ok(e.rs.clone());
            }
        }
        let rs = self.compute(key, timeout)?;
        let mut c = self.cache.lock();
        let query_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        c.entries.push(Entry { query_id, rs: rs.clone(), columns: Vec::new(), highlight: false, used: Instant::now() });
        self.evict(&mut c);
        Ok(rs)
    }

    fn compute(&self, key: &SearchKey, timeout: Duration) -> Result<Arc<ResultSetRef>, IpcError> {
        let cancel = CancelToken::new();
        let _dog = Watchdog::start(cancel.clone(), timeout);
        let options = self.host.search_options();
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ast = search_ast(&key.search, &key.mods, &options);
            let env = PlanEnv {
                defaults: request_mods(&key.mods, options.defaults),
                star_all: options.star_all,
                date: fnc_query::date::DateEnv::now(),
                pinyin_default: options.pinyin_default,
                omit: None,
            };
            self.caches.set_type_names(options.type_names.clone());
            let planned = plan(&ast, &env, None);
            let snap = self.shared.snapshot();
            let ctx = ExecCtx { cancel: &cancel, prev: None, caches: &self.caches };
            let mut rs = execute(&planned, &snap, &ctx)?;
            sort_ids(&mut rs.ids, &key.sort, &snap, &self.caches, &cancel)?;
            Ok::<_, fnc_exec::Cancelled>((rs, planned, snap.instance(), snap.generation()))
        }));
        let (rs, planned, instance, generation) = match run {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                return Err(IpcError::new(Status::Timeout, format!("query exceeded {} ms", timeout.as_millis())));
            }
            Err(_) => {
                tracing::error!(search = %key.search, "IPC query panicked");
                self.caches.clear();
                return Err(IpcError::new(Status::Internal, "query failed"));
            }
        };
        Ok(Arc::new(ResultSetRef {
            key: key.clone(),
            ids: Arc::new(rs.ids),
            plan: Arc::new(planned),
            instance,
            generation,
        }))
    }

    /// Builds result columns for `ids[offset..offset+limit]` of `rs` (`query_id` is echoed).
    pub fn page(
        &self,
        rs: &ResultSetRef,
        query_id: u64,
        offset: u64,
        limit: u64,
        columns: &[Column],
        highlight: bool,
    ) -> Results {
        let limit = limit.min(self.config.max_results);
        let start = (offset.min(rs.ids.len() as u64)) as usize;
        let end = (start as u64).saturating_add(limit).min(rs.ids.len() as u64) as usize;
        let cols = ColumnSet::of(columns);
        let hl = highlight.then(|| Highlighter::new(&rs.plan));
        let snap = self.shared.snapshot();
        let mut out = ResultColumns::with_columns(cols, highlight, end - start);
        let valid = rs.instance == snap.instance();
        for &id in &rs.ids[start..end] {
            if valid && snap.is_live(id) {
                out.push(entry_row(&snap, id, cols, &self.caches, hl.as_ref()));
            } else {
                // Entry vanished since the query ran: keep the row count stable.
                out.push(crate::rows::Row::default());
            }
        }
        Results {
            query_id,
            total: rs.ids.len() as u64,
            offset: start as u64,
            sort: rs.key.sort.keys().map(|(c, a)| (column_of(c), a)).collect(),
            columns: out,
        }
    }

    /// Runs a [`Query`].
    pub fn query(&self, q: &Query) -> Result<Results, IpcError> {
        let key =
            SearchKey { search: q.search.clone(), mods: q.mods, sort: sort_spec(&q.sort, self.host.folders_first()) };
        let rs = self.search(&key, self.timeout(q.timeout_ms))?;
        let cols = if q.columns.is_empty() { vec![Column::FullPath] } else { q.columns.clone() };
        let id = self.register(&rs, &cols, q.highlight);
        Ok(self.page(&rs, id, q.offset, q.limit, &cols, q.highlight))
    }

    /// Pages a query id with the columns of the query that created it (re-running the
    /// search if the index changed meanwhile).
    pub fn query_continue(&self, query_id: u64, offset: u64, limit: u64) -> Result<Results, IpcError> {
        let (rs, columns, highlight) = {
            let mut c = self.cache.lock();
            let e = c
                .entries
                .iter_mut()
                .find(|e| e.query_id == query_id)
                .ok_or_else(|| IpcError::new(Status::NotFound, format!("unknown query id {query_id}")))?;
            e.used = Instant::now();
            let cols = if e.columns.is_empty() { vec![Column::FullPath] } else { e.columns.clone() };
            (e.rs.clone(), cols, e.highlight)
        };
        let current = rs.is_current(&self.shared.snapshot());
        let rs = if current {
            rs
        } else {
            let fresh = self.compute(&rs.key, self.config.default_timeout)?;
            let mut c = self.cache.lock();
            if let Some(e) = c.entries.iter_mut().find(|e| e.query_id == query_id) {
                e.rs = fresh.clone();
            }
            fresh
        };
        Ok(self.page(&rs, query_id, offset, limit, &columns, highlight))
    }

    /// Number of results of a search.
    pub fn result_count(&self, spec: &SearchSpec) -> Result<u64, IpcError> {
        let key = SearchKey { search: spec.search.clone(), mods: spec.mods, sort: SortSpec::default() };
        Ok(self.search(&key, self.config.default_timeout)?.len() as u64)
    }

    /// Total size of the files matching a search (folders and unknown sizes count 0).
    pub fn total_size(&self, spec: &SearchSpec) -> Result<u64, IpcError> {
        let key = SearchKey { search: spec.search.clone(), mods: spec.mods, sort: SortSpec::default() };
        let rs = self.search(&key, self.config.default_timeout)?;
        let snap = self.shared.snapshot();
        if rs.instance != snap.instance() {
            return Ok(0);
        }
        Ok(rs
            .ids
            .iter()
            .filter(|&&id| snap.is_live(id) && !snap.is_dir(id))
            .filter_map(|&id| snap.size(id))
            .fold(0u64, u64::saturating_add))
    }

    /// Index status.
    pub fn index_status(&self) -> IndexStatus {
        let snap = self.shared.snapshot();
        let st = snap.stats();
        IndexStatus {
            db_loaded: self.host.is_db_loaded(),
            busy: self.host.is_busy(),
            entries: st.live as u64,
            folders: st.folders as u64,
            volumes: snap.volumes().iter().filter(|v| !v.is_retired()).count() as u32,
            generation: snap.generation(),
            message: self.host.status_text(),
        }
    }

    /// Indexed volumes.
    pub fn volumes(&self) -> Vec<VolumeEntry> {
        let snap = self.shared.snapshot();
        snap.volumes()
            .iter()
            .filter(|v| !v.is_retired())
            .map(|v| VolumeEntry {
                root_path: v.info.root_path.clone(),
                display_name: v.info.display_name.clone(),
                kind: v.info.kind.as_str().to_string(),
                fs_name: v.info.fs_name.clone(),
                online: v.online,
                included: v.include_in_index,
            })
            .collect()
    }

    /// Versions.
    pub fn version(&self) -> VersionInfo {
        VersionInfo {
            fennec: env!("CARGO_PKG_VERSION").to_string(),
            protocol: crate::proto::PROTOCOL_VERSION,
            everything: self.host.everything_version(),
        }
    }

    /// Run count of the entry at `path`.
    pub fn run_count(&self, path: &str) -> Result<u32, IpcError> {
        let snap = self.shared.snapshot();
        let id = resolve_path(&snap, path).ok_or_else(|| IpcError::new(Status::NotFound, "path not in the index"))?;
        Ok(snap.run_info(id).count)
    }

    /// Increments the run count of `path`; returns the new count.
    pub fn inc_run_count(&self, path: &str) -> Result<u32, IpcError> {
        let n = {
            let mut w = self.shared.write();
            let id = resolve_path(&w, path).ok_or_else(|| IpcError::new(Status::NotFound, "path not in the index"))?;
            w.record_run(id);
            w.run_info(id).count
        };
        self.host.run_count_changed();
        Ok(n)
    }

    /// Sets the run count of `path`.
    pub fn set_run_count(&self, path: &str, count: u32) -> Result<(), IpcError> {
        {
            let mut w = self.shared.write();
            let id = resolve_path(&w, path).ok_or_else(|| IpcError::new(Status::NotFound, "path not in the index"))?;
            w.set_run_count(id, count);
        }
        self.host.run_count_changed();
        Ok(())
    }

    /// Runs an admin command after [`Host::authorize`].
    pub fn admin(&self, cmd: AdminCommand, peer: &Peer) -> Result<(), IpcError> {
        if !self.host.authorize(cmd, peer) {
            return Err(IpcError::new(Status::Denied, "access denied"));
        }
        self.host.command(cmd).map_err(IpcError::from)
    }

    /// Executes one native-protocol operation.
    pub fn handle(&self, op: &Op, peer: &Peer) -> Result<Payload, IpcError> {
        let admin = |cmd| self.admin(cmd, peer).map(|()| Payload::None);
        match op {
            Op::Query(q) => self.query(q).map(Payload::Results),
            Op::QueryContinue { query_id, offset, limit } => {
                self.query_continue(*query_id, *offset, *limit).map(Payload::Results)
            }
            Op::ReleaseQuery { query_id } => Ok(Payload::Bool(self.release(*query_id))),
            Op::GetResultCount(s) => self.result_count(s).map(Payload::Count),
            Op::GetTotalSize(s) => self.total_size(s).map(Payload::Size),
            Op::GetVersion => Ok(Payload::Version(self.version())),
            Op::IsDbLoaded => Ok(Payload::Bool(self.host.is_db_loaded())),
            Op::GetIndexStatus => Ok(Payload::IndexStatus(self.index_status())),
            Op::Rebuild => admin(AdminCommand::Rebuild),
            Op::Update => admin(AdminCommand::Update),
            Op::SaveDb => admin(AdminCommand::SaveDb),
            Op::SaveRunHistory => admin(AdminCommand::SaveRunHistory),
            Op::DeleteRunHistory => admin(AdminCommand::DeleteRunHistory),
            Op::Exit => admin(AdminCommand::Exit),
            Op::GetRunCount { path } => self.run_count(path).map(Payload::RunCount),
            Op::IncRunCount { path } => self.inc_run_count(path).map(Payload::RunCount),
            Op::SetRunCount { path, count } => self.set_run_count(path, *count).map(|()| Payload::None),
            Op::ListVolumes => Ok(Payload::Volumes(self.volumes())),
            Op::ListFilters => Ok(Payload::Filters(self.host.filters())),
            Op::ListBookmarks => Ok(Payload::Bookmarks(self.host.bookmarks())),
        }
    }
}

/// Wire column of a sort column.
pub fn column_of(c: fnc_exec::SortColumn) -> Column {
    use fnc_exec::SortColumn as S;
    match c {
        S::Name | S::NameLength | S::Frn | S::Depth | S::IndexSource => Column::Name,
        S::Path => Column::Path,
        S::FullPath => Column::FullPath,
        S::Size => Column::Size,
        S::Extension => Column::Extension,
        S::DateModified => Column::DateModified,
        S::DateCreated => Column::DateCreated,
        S::DateAccessed => Column::DateAccessed,
        S::Attributes => Column::Attributes,
        S::RunCount => Column::RunCount,
        S::DateRun => Column::DateRun,
        S::DateRecentlyChanged => Column::DateRecentlyChanged,
        S::Volume => Column::FileListName,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn fixture() -> SharedIndex {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Data\",,,,16\n\
                   \"C:\\Data\\big.iso\",5000,133000000000000005,,32\n\
                   \"C:\\Data\\small.iso\",100,133000000000000001,,32\n\
                   \"C:\\Data\\mid.iso\",2500,133000000000000003,,32\n\
                   \"C:\\Data\\notes.txt\",10,133000000000000002,,32\n\
                   \"C:\\Data\\ABC report.pdf\",77,133000000000000004,,32\n\
                   \"C:\\Data\\isos\",,,,16\n\
                   \"C:\\Other\\abc.txt\",1,133000000000000000,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "fixture.efu").unwrap();
        s.rebuild_name_order();
        SharedIndex::new(s)
    }

    fn backend() -> IndexBackend {
        IndexBackend::new(fixture(), Arc::new(DefaultHost))
    }

    fn names(r: &Results) -> Vec<String> {
        r.columns.name.clone().unwrap()
    }

    #[test]
    fn query_sort_page() {
        let b = backend();
        let q = Query {
            search: "*.iso".into(),
            sort: vec![(Column::Size, false)],
            columns: vec![Column::Name, Column::Size, Column::FullPath],
            ..Query::default()
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.total, 3);
        assert_eq!(names(&r), ["big.iso", "mid.iso", "small.iso"]);
        assert_eq!(r.columns.size.as_deref(), Some(&[5000, 2500, 100][..]));
        assert_eq!(r.columns.full_path.as_ref().unwrap()[0], "C:\\Data\\big.iso");
        let p = b.query(&Query { offset: 1, limit: 1, ..q.clone() }).unwrap();
        assert_eq!(names(&p), ["mid.iso"]);
        assert_eq!(p.query_id, r.query_id, "same search reuses the cached result set");
        let c = b.query_continue(r.query_id, 2, 10).unwrap();
        assert_eq!(names(&c), ["small.iso"]);
        assert!(b.query_continue(999, 0, 1).is_err());
    }

    #[test]
    fn modifiers_counts_and_sizes() {
        let b = backend();
        let n = |search: &str, mods: Mods| b.result_count(&SearchSpec { search: search.into(), mods }).unwrap();
        assert_eq!(n("abc", Mods::default()), 2);
        assert_eq!(n("abc", Mods { case: true, ..Mods::default() }), 1);
        assert_eq!(n("data\\ abc", Mods { path: true, ..Mods::default() }), 1);
        assert_eq!(n("^[a-z]+\\.iso$", Mods { regex: true, ..Mods::default() }), 3);
        assert_eq!(n("iso", Mods { whole_word: true, ..Mods::default() }), 3);
        let size = b.total_size(&SearchSpec { search: "iso".into(), mods: Mods::default() }).unwrap();
        assert_eq!(size, 7600, "folders do not count");
    }

    #[test]
    fn run_counts_and_admin() {
        let b = backend();
        assert_eq!(b.inc_run_count("c:\\data\\notes.txt").unwrap(), 1);
        assert_eq!(b.inc_run_count("C:\\Data\\notes.txt").unwrap(), 2);
        b.set_run_count("C:\\Data\\notes.txt", 7).unwrap();
        assert_eq!(b.run_count("C:\\Data\\notes.txt").unwrap(), 7);
        assert_eq!(b.run_count("C:\\nope").unwrap_err().status, Status::NotFound);
        assert_eq!(b.handle(&Op::Rebuild, &Peer::LOCAL).unwrap_err().status, Status::Unsupported);
        let st = b.index_status();
        assert_eq!(st.entries, 10);
        assert_eq!(b.volumes().len(), 1);
        assert!(matches!(b.handle(&Op::GetVersion, &Peer::LOCAL).unwrap(), Payload::Version(_)));
    }

    #[test]
    fn highlight_and_requery_after_change() {
        let b = backend();
        let q = Query {
            search: "abc".into(),
            columns: vec![Column::Name, Column::Path],
            highlight: true,
            ..Query::default()
        };
        let r = b.query(&q).unwrap();
        assert_eq!(r.columns.highlighted_name.as_ref().unwrap()[0], "*ABC* report.pdf");
        b.inc_run_count("C:\\Other\\abc.txt").unwrap();
        // Generation changed: continuing re-runs the search transparently.
        let c = b.query_continue(r.query_id, 0, 10).unwrap();
        assert_eq!(c.total, 2);
        assert_eq!(c.query_id, r.query_id);
    }

    #[test]
    fn timeouts_cancel() {
        let b = backend();
        let key = SearchKey { search: "a".into(), mods: Mods::default(), sort: SortSpec::default() };
        // A zero time limit cancels (almost) immediately; either outcome is acceptable but
        // a timeout must be reported as such.
        match b.search(&key, Duration::ZERO) {
            Ok(r) => assert!(!r.is_empty()),
            Err(e) => assert_eq!(e.status, Status::Timeout),
        }
    }

    #[test]
    fn cache_is_bounded() {
        let shared = fixture();
        let b = IndexBackend::with_config(
            shared,
            Arc::new(DefaultHost),
            BackendConfig { cached_queries: 2, ..BackendConfig::default() },
        );
        let ids: Vec<u64> = ["a", "b", "c"]
            .iter()
            .map(|s| b.query(&Query { search: (*s).into(), ..Query::default() }).unwrap().query_id)
            .collect();
        assert!(b.query_continue(ids[0], 0, 1).is_err(), "oldest evicted");
        assert!(b.query_continue(ids[2], 0, 1).is_ok());
        assert!(b.release(ids[2]));
    }
}

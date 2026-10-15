//! `fes` — command-line search compatible with voidtools `es.exe` 1.1 (SPEC ch.4 §2).
//!
//! Queries a running Fennec over the native IPC protocol (`fnc_ipc::Client`), or — with
//! `-offline` / `-filelist` — loads `index.fnx` / EFU file lists and runs the same backend
//! in-process. Output formats come from [`fnc_ipc::export`].
//!
//! Exit codes: 0 success, 1 Fennec not running (IPC unavailable), 2 bad arguments,
//! 3 timeout; es codes are kept for the remaining cases: 5 export file cannot be created,
//! 7 query failed, 9 no results with `-no-result-error`.

#![forbid(unsafe_code)]

pub mod args;

#[cfg(test)]
mod tests;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fnc_index::{IndexStore, SharedIndex};
use fnc_ipc::backend::{IndexBackend, Peer};
use fnc_ipc::export::{Exporter, Format, Newline, resolve_columns};
use fnc_ipc::format::{SizeFormat, format_number, format_size};
use fnc_ipc::proto::{Column, Mods, Op, Payload, Query, Results, SearchSpec, Status};
use fnc_ipc::{Client, ClientError};

pub use args::{ArgError, Args, Command, Mode, parse_args};

/// Exit: success.
pub const EXIT_OK: u8 = 0;
/// Exit: no running instance.
pub const EXIT_NO_IPC: u8 = 1;
/// Exit: bad arguments.
pub const EXIT_ARGS: u8 = 2;
/// Exit: timeout.
pub const EXIT_TIMEOUT: u8 = 3;
/// Exit: cannot create the export file (es 5).
pub const EXIT_CREATE_FILE: u8 = 5;
/// Exit: the query failed (es 7).
pub const EXIT_IPC_ERROR: u8 = 7;
/// Exit: no results with `-no-result-error` (es 9).
pub const EXIT_NO_RESULTS: u8 = 9;

/// Rows fetched per IPC round trip.
pub const PAGE: u64 = 100_000;

/// Process environment of a run (injectable for tests).
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `-save-settings` file; `None` disables saved settings.
    pub settings_file: Option<PathBuf>,
    /// stdout is an interactive terminal (enables ANSI highlighting).
    pub stdout_is_terminal: bool,
    /// Default line terminator for stdout (`None` = CRLF like es).
    pub stdout_newline: Option<Newline>,
    /// Data root override for `-offline` (default: `fnc_core::paths::DataPaths`).
    pub data_root: Option<PathBuf>,
}

impl Env {
    /// The real environment of the `fes` process.
    pub fn from_process() -> Self {
        use std::io::IsTerminal;
        let root = fnc_core::paths::DataPaths::resolve("").root;
        Self {
            settings_file: Some(root.join("fes.args")),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            stdout_newline: if cfg!(windows) { None } else { Some(Newline::Lf) },
            data_root: None,
        }
    }
}

/// A failure with its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Exit code.
    pub code: u8,
    /// Message for stderr.
    pub message: String,
}

impl Failure {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        let code = match &e {
            ClientError::Unavailable(_) => EXIT_NO_IPC,
            ClientError::Timeout | ClientError::Server { status: Status::Timeout, .. } => EXIT_TIMEOUT,
            _ => EXIT_IPC_ERROR,
        };
        Failure::new(code, e.to_string())
    }
}

/// Where queries go.
pub enum Conn {
    /// A running instance.
    Remote(Client),
    /// An in-process backend (`-offline`, `-filelist`).
    Local(Box<IndexBackend>),
}

impl Conn {
    /// Sends one operation.
    pub fn call(&mut self, op: Op) -> Result<Payload, Failure> {
        match self {
            Conn::Remote(c) => c.call(op).map_err(Failure::from),
            Conn::Local(b) => b.handle(&op, &Peer::LOCAL).map_err(|e| {
                let code = if e.status == Status::Timeout { EXIT_TIMEOUT } else { EXIT_IPC_ERROR };
                Failure::new(code, e.message)
            }),
        }
    }

    fn results(&mut self, op: Op) -> Result<Results, Failure> {
        match self.call(op)? {
            Payload::Results(r) => Ok(r),
            p => Err(Failure::new(EXIT_IPC_ERROR, format!("unexpected reply {p:?}"))),
        }
    }

    fn number(&mut self, op: Op) -> Result<u64, Failure> {
        match self.call(op)? {
            Payload::Count(n) | Payload::Size(n) => Ok(n),
            Payload::RunCount(n) => Ok(u64::from(n)),
            p => Err(Failure::new(EXIT_IPC_ERROR, format!("unexpected reply {p:?}"))),
        }
    }
}

fn mods(a: &Args) -> Mods {
    Mods {
        case: a.case,
        whole_word: a.whole_word,
        path: a.match_path,
        regex: false,
        diacritics: a.diacritics,
        prefix: a.prefix,
        suffix: a.suffix,
        ignore_punctuation: a.ignore_punctuation,
        ignore_whitespace: a.ignore_whitespace,
    }
}

fn load_settings(env: &Env) -> Vec<String> {
    let Some(f) = &env.settings_file else { return Vec::new() };
    std::fs::read_to_string(f)
        .map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn save_settings(env: &Env, a: &Args) -> io::Result<()> {
    let Some(f) = &env.settings_file else { return Ok(()) };
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = a.settings.join("\n");
    text.push('\n');
    std::fs::write(f, text)
}

/// Opens the in-process backend for `-filelist` / `-offline`.
pub fn open_local(a: &Args, env: &Env) -> Result<IndexBackend, Failure> {
    let mut data_dir: Option<std::path::PathBuf> = None;
    let store = if !a.file_lists.is_empty() {
        let mut store = IndexStore::default();
        for f in &a.file_lists {
            fnc_index::efu::load_efu_file(&mut store, f).map_err(|e| Failure::new(EXIT_ARGS, e.to_string()))?;
        }
        store.rebuild_name_order();
        store
    } else {
        let path = match (&a.db, &env.data_root) {
            (Some(p), _) => p.clone(),
            (None, Some(root)) => root.join("index.fnx"),
            (None, None) => fnc_core::paths::DataPaths::resolve(&a.instance).index_file(),
        };
        data_dir = path.parent().map(std::path::Path::to_path_buf);
        fnc_index::persist::load(&path)
            .map_err(|e| Failure::new(EXIT_NO_IPC, format!("cannot load {}: {e}", path.display())))?
    };
    let host = OfflineHost { options: offline_options(&store, data_dir.as_deref()) };
    Ok(IndexBackend::new(SharedIndex::new(store), Arc::new(host)))
}

/// `-offline` / `-filelist` searches: the saved property and content indexes next to
/// `index.fnx`, the built-in extractors for unindexed files and the Windows Search system
/// index (like the window).
fn offline_options(store: &IndexStore, dir: Option<&std::path::Path>) -> fnc_exec::SearchOptions {
    let mut o = fnc_exec::SearchOptions::default();
    if let Some(dir) = dir {
        if let Ok(p) = fnc_props::store::PropertyStore::load(&dir.join("props.fnx"), store) {
            o.props = Some(fnc_props::store::PropertyIndex::new(p));
        }
        if let Ok(c) = fnc_props::content::ContentStore::load(&dir.join("content.fnx"), store) {
            o.content = Some(fnc_props::content::ContentIndex::new(c));
        }
        if let Ok(h) = fnc_props::hash::HashCache::load(&dir.join("hashes.fnx")) {
            o.hashes = Some(fnc_exec::Shared::new(Arc::new(h)));
        }
    }
    let on_demand: Arc<dyn fnc_props::ondemand::OnDemand> = Arc::new(fnc_props::ondemand::DefaultOnDemand::default());
    o.on_demand = Some(fnc_exec::Shared::new(on_demand));
    let system_index: Arc<dyn fnc_exec::SystemIndexSource> = Arc::new(fnc_platform::sysindex::WinSearch::default());
    o.system_index = Some(fnc_exec::Shared::new(system_index));
    o
}

struct OfflineHost {
    options: fnc_exec::SearchOptions,
}

impl fnc_ipc::backend::Host for OfflineHost {
    fn search_options(&self) -> fnc_exec::SearchOptions {
        self.options.clone()
    }
}

/// Connects to the running instance (honouring `-timeout`: waits for the instance and
/// for its database to load).
pub fn connect(a: &Args) -> Result<Client, Failure> {
    let wait = a.timeout_ms.map(Duration::from_millis);
    let start = Instant::now();
    // Without -timeout fail fast like es (exit 1); with it, keep retrying until it expires.
    let mut c = Client::connect(&a.instance, wait.unwrap_or(Duration::from_millis(300)))?;
    // The server limits each query (30 s by default); this only guards against a hung peer.
    c.set_timeout(Some(Duration::from_secs(120)));
    if let Some(w) = wait {
        loop {
            if c.is_db_loaded()? {
                break;
            }
            if start.elapsed() >= w {
                return Err(Failure::new(EXIT_TIMEOUT, "timed out waiting for the Fennec database to load"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(c)
}

fn writeln_nl(out: &mut dyn Write, text: &str, nl: Newline) -> Result<(), Failure> {
    out.write_all(text.as_bytes())
        .and_then(|()| out.write_all(nl.as_str().as_bytes()))
        .map_err(|e| Failure::new(EXIT_IPC_ERROR, e.to_string()))
}

/// Runs `fes` with already-parsed arguments.
pub fn run_args(a: &Args, env: &Env, out: &mut dyn Write, conn: Option<&mut Conn>) -> Result<u8, Failure> {
    let nl = a.export.newline.or(env.stdout_newline).unwrap_or_default();
    match &a.mode {
        Mode::Help => {
            writeln_nl(out, args::HELP.trim_end(), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::Version => {
            writeln_nl(out, &format!("fes {}", env!("CARGO_PKG_VERSION")), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::ListProperties => {
            for c in Column::ALL {
                writeln_nl(out, c.header(), nl)?;
            }
            return Ok(EXIT_OK);
        }
        _ => {}
    }
    fnc_core::crash::record_query(&a.full_search());
    if a.debug_panic {
        panic!("-debug-panic: deliberate panic in fes (crash report test)");
    }
    if a.debug_crash {
        fnc_platform::os::raise_debug_exception();
    }
    let mut owned;
    let conn = match conn {
        Some(c) => c,
        None => {
            owned = if a.offline || !a.file_lists.is_empty() {
                Conn::Local(Box::new(open_local(a, env)?))
            } else {
                Conn::Remote(connect(a)?)
            };
            &mut owned
        }
    };
    for cmd in &a.commands {
        let op = match cmd {
            Command::Exit => Op::Exit,
            Command::Rebuild => Op::Rebuild,
            Command::Update => Op::Update,
            Command::SaveDb => Op::SaveDb,
        };
        conn.call(op)?;
    }
    let spec = SearchSpec { search: a.full_search(), mods: mods(a) };
    match &a.mode {
        Mode::ResultCount => {
            let n = conn.number(Op::GetResultCount(spec))?;
            writeln_nl(out, &n.to_string(), nl)?;
            return Ok(if a.no_result_error && n == 0 { EXIT_NO_RESULTS } else { EXIT_OK });
        }
        Mode::TotalSize => {
            let n = conn.number(Op::GetTotalSize(spec))?;
            writeln_nl(out, &n.to_string(), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::FolderSize(folder) => {
            let f = folder.trim_end_matches(['\\', '/']);
            let sep = if f.starts_with('/') { '/' } else { '\\' };
            let spec = SearchSpec { search: format!("\"{f}{sep}\""), mods: Mods { path: true, ..Mods::default() } };
            let n = conn.number(Op::GetTotalSize(spec))?;
            writeln_nl(out, &n.to_string(), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::EverythingVersion => {
            let v = match conn.call(Op::GetVersion)? {
                Payload::Version(v) => v,
                p => return Err(Failure::new(EXIT_IPC_ERROR, format!("unexpected reply {p:?}"))),
            };
            let e = v.everything;
            writeln_nl(out, &format!("{}.{}.{}.{}", e[0], e[1], e[2], e[3]), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::GetRunCount(path) => {
            let n = match conn.number(Op::GetRunCount { path: path.clone() }) {
                Ok(n) => n,
                // Everything reports 0 for files without history.
                Err(f) if f.code == EXIT_IPC_ERROR => 0,
                Err(f) => return Err(f),
            };
            writeln_nl(out, &n.to_string(), nl)?;
            return Ok(EXIT_OK);
        }
        Mode::IncRunCount(path) => {
            conn.number(Op::IncRunCount { path: path.clone() })?;
            return Ok(EXIT_OK);
        }
        Mode::SetRunCount(path, n) => {
            conn.call(Op::SetRunCount { path: path.clone(), count: *n })?;
            return Ok(EXIT_OK);
        }
        Mode::Search | Mode::Help | Mode::Version | Mode::ListProperties => {}
    }
    if !a.commands.is_empty() && a.search.is_empty() && a.filter.is_empty() {
        // `fes -reindex` etc. only send the command (es does the same).
        return Ok(EXIT_OK);
    }
    search(a, env, out, conn)
}

fn search(a: &Args, env: &Env, out: &mut dyn Write, conn: &mut Conn) -> Result<u8, Failure> {
    let mut opts = a.export.clone();
    let to_file = a.export_file.is_some();
    if !to_file && opts.newline.is_none() {
        opts.newline = env.stdout_newline;
    }
    // es only highlights on a console; redirected or exported output is plain text.
    let highlight = a.highlight && opts.format == Format::Plain && !to_file && env.stdout_is_terminal;
    if highlight {
        opts.ansi_highlight = Some(a.highlight_color);
    }
    let footer = a.footer == Some(true) && opts.format == Format::Plain;
    let mut cols = resolve_columns(&opts);
    if footer && !cols.contains(&Column::Size) {
        cols.push(Column::Size);
    }
    let (sort_col, asc) = a.effective_sort();
    let want = a.max.unwrap_or(u64::MAX);
    let q = Query {
        search: a.full_search(),
        mods: mods(a),
        sort: vec![(sort_col, asc)],
        offset: a.offset,
        limit: want.min(PAGE),
        columns: cols,
        highlight,
        // es -timeout only bounds waiting for the database; queries use the server limit.
        timeout_ms: 0,
    };
    let mut file;
    let sink: &mut dyn Write = match &a.export_file {
        Some(p) => {
            file = io::BufWriter::new(
                std::fs::File::create(p)
                    .map_err(|e| Failure::new(EXIT_CREATE_FILE, format!("cannot create {}: {e}", p.display())))?,
            );
            &mut file
        }
        None => out,
    };
    let io_err = |e: io::Error| Failure::new(EXIT_IPC_ERROR, e.to_string());
    let mut ex = Exporter::new(sink, opts).map_err(io_err)?;
    let mut page = conn.results(Op::Query(q))?;
    let total = page.total;
    let available = total.saturating_sub(a.offset).min(want);
    let mut written = 0u64;
    let mut total_size = 0u64;
    loop {
        for i in 0..page.len() {
            if written >= available {
                break;
            }
            let r = page.row(i);
            if !r.is_folder() && r.size != fnc_ipc::proto::UNKNOWN {
                total_size = total_size.saturating_add(r.size);
            }
            ex.write_row(&r).map_err(io_err)?;
            written += 1;
        }
        if written >= available || page.is_empty() {
            break;
        }
        let limit = (available - written).min(PAGE);
        page = conn.results(Op::QueryContinue { query_id: page.query_id, offset: a.offset + written, limit })?;
    }
    if let Conn::Remote(_) = conn {
        let _ = conn.call(Op::ReleaseQuery { query_id: page.query_id });
    }
    if footer {
        let size = format_size(total_size, SizeFormat::Auto, true);
        let items = format_number(written, true);
        let s = if written == 1 { "" } else { "s" };
        ex.write_line(&format!("{items} item{s}{}{size}", if size.is_empty() { "" } else { " " })).map_err(io_err)?;
    }
    ex.finish().map_err(io_err)?;
    Ok(if a.no_result_error && total == 0 { EXIT_NO_RESULTS } else { EXIT_OK })
}

/// Full `fes` run: saved settings + `raw` arguments; messages go to `err`. Returns the
/// exit code.
pub fn run(raw: Vec<String>, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let clear = raw.iter().any(|r| args::option_eq(r.trim_start_matches(['-', '/']), "clear-settings"));
    let mut all = if clear { Vec::new() } else { load_settings(env) };
    all.extend(raw);
    let a = match parse_args(all) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "fes: {e}");
            return EXIT_ARGS;
        }
    };
    // Crash reports go to the logs folder of the instance searched (REL-02).
    if fnc_core::crash::dir().is_some() {
        if !a.instance.is_empty() {
            fnc_core::crash::set_dir(fnc_core::paths::DataPaths::resolve(&a.instance).logs_dir());
        }
        let local = a.offline || !a.file_lists.is_empty();
        let state = format!(
            "fes {} instance '{}'",
            if local { "in-process index (-offline / -filelist) of" } else { "client of the running" },
            a.instance
        );
        fnc_core::crash::set_state_provider(Arc::new(move || state.clone()));
    }
    if a.clear_settings
        && let Some(f) = &env.settings_file
    {
        let _ = std::fs::remove_file(f);
    }
    if a.save_settings
        && let Err(e) = save_settings(env, &a)
    {
        let _ = writeln!(err, "fes: cannot save settings: {e}");
    }
    match run_args(&a, env, out, None) {
        Ok(code) => code,
        Err(f) => {
            let _ = writeln!(err, "fes: {}", f.message);
            f.code
        }
    }
}

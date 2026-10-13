//! ETP client sessions (Tools → Connect to ETP Server; SPEC ch.4 §5, ADR-036).
//!
//! The remote server runs every search. Its results are turned into a small read-only
//! index of their own — a file-list volume built from the paths, sizes and dates the
//! server sends — so the window shows, sorts, copies and exports them exactly like local
//! results, and nothing on the local disk is touched for them. The volume is offline:
//! no icons, thumbnails or properties are read from the paths.
//!
//! Threads: `fennec-etp-client` owns the connection and runs one search at a time
//! (newer requests replace queued ones); each download opens its own connection on a
//! `fennec-etp-download` thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use fnc_core::settings::EtpLinkType;
use fnc_etp::{ClientError, EtpClient, ResultItem, SearchState, Target};
use fnc_exec::{ResultSet, SearchOutcome, SortSpec};
use fnc_index::{IndexStore, SharedIndex};
use fnc_ipc::proto::{Column, Mods, UNKNOWN};

/// Network time limit of one request.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Results fetched before the first page is shown.
const FIRST_PAGE: u32 = 256;
/// Results per request after the first page.
const CHUNK: u32 = 4096;

/// Where and how to connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    /// Server, port and credentials.
    pub target: Target,
    /// How results are opened.
    pub link: EtpLinkType,
}

impl RemoteTarget {
    /// `host:port` for display.
    pub fn label(&self) -> String {
        let h = &self.target.host;
        if h.contains(':') { format!("[{h}]:{}", self.target.port) } else { format!("{h}:{}", self.target.port) }
    }
}

/// One remote search.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteRequest {
    /// View request id (results with older ids are dropped by the view).
    pub id: u64,
    /// Search text.
    pub text: String,
    /// Match options.
    pub mods: Mods,
    /// Active filter: search text and options (`FILTER_SEARCH`).
    pub filter: Option<(String, Mods)>,
    /// Sort (columns ETP cannot sort by are sorted locally after fetching).
    pub sort: SortSpec,
    /// Most results fetched.
    pub max_results: u32,
}

/// What the session reports to the window.
#[derive(Debug)]
pub enum RemoteEvent {
    /// Logged in.
    Connected {
        /// Server greeting.
        welcome: String,
    },
    /// Results of a request (sent after the first page and again when complete).
    Results {
        /// Outcome for the view (ids index [`RemoteSession::index`]).
        outcome: SearchOutcome,
        /// All results of the search on the server.
        total: u64,
    },
    /// A search failed; the connection stays open.
    QueryFailed(String),
    /// The connection failed or was lost (the session is over).
    Disconnected(String),
    /// A download finished.
    Downloaded {
        /// Local copy.
        path: PathBuf,
        /// Show it in its folder instead of opening it.
        reveal: bool,
    },
    /// A download failed.
    DownloadFailed(String),
}

enum Cmd {
    Search(RemoteRequest),
    Shutdown,
}

/// A connection to an ETP server and the index of its latest results.
pub struct RemoteSession {
    target: RemoteTarget,
    index: SharedIndex,
    tx: Sender<Cmd>,
    events_tx: Sender<RemoteEvent>,
    events: Receiver<RemoteEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteSession {
    /// Starts connecting (in the background; [`RemoteEvent::Connected`] or
    /// [`RemoteEvent::Disconnected`] follows). `wake` repaints the UI after every event.
    pub fn connect(target: RemoteTarget, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (events_tx, events) = crossbeam_channel::unbounded();
        let index = SharedIndex::default();
        let (t, ix, ev, w) = (target.clone(), index.clone(), events_tx.clone(), wake.clone());
        let thread = std::thread::Builder::new()
            .name("fennec-etp-client".into())
            .spawn(move || worker(&t, &ix, &rx, &ev, &*w))
            .map_err(|e| tracing::error!(error = %e, "cannot start the ETP client"))
            .ok();
        if thread.is_none() {
            let _ = events_tx.send(RemoteEvent::Disconnected("cannot start the ETP client thread".into()));
        }
        Self { target, index, tx, events_tx, events, wake, thread }
    }

    /// Server and options.
    pub fn target(&self) -> &RemoteTarget {
        &self.target
    }

    /// The index holding the latest results.
    pub fn index(&self) -> &SharedIndex {
        &self.index
    }

    /// Events (drained by the UI every frame).
    pub fn events(&self) -> &Receiver<RemoteEvent> {
        &self.events
    }

    /// Queues a search (replaces a queued, not yet started one).
    pub fn search(&self, req: RemoteRequest) {
        let _ = self.tx.send(Cmd::Search(req));
    }

    /// Downloads `server_path` (a path of the results) into the temporary folder on its own
    /// connection; reports [`RemoteEvent::Downloaded`].
    pub fn download(&self, server_path: String, reveal: bool) {
        let (target, ev, wake) = (self.target.target.clone(), self.events_tx.clone(), self.wake.clone());
        let dir = download_dir(&self.target);
        let spawned = std::thread::Builder::new().name("fennec-etp-download".into()).spawn(move || {
            let r = download(&target, &server_path, &dir);
            let _ = ev.send(match r {
                Ok(path) => RemoteEvent::Downloaded { path, reveal },
                Err(e) => RemoteEvent::DownloadFailed(e),
            });
            wake();
        });
        if let Err(e) = spawned {
            let _ = self.events_tx.send(RemoteEvent::DownloadFailed(e.to_string()));
        }
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        // Not joined: the thread may wait for the network; it ends after its request.
        drop(self.thread.take());
    }
}

/// The ETP sort of a view sort (`None`: not available, sort locally).
pub fn etp_sort(sort: &SortSpec) -> Option<(Column, bool)> {
    use fnc_exec::SortColumn as S;
    if sort.props[0].is_some() || sort.custom[0].is_some() {
        return None;
    }
    let col = match sort.column {
        S::Name => Column::Name,
        S::Path | S::FullPath => Column::Path,
        S::Size => Column::Size,
        S::Extension => Column::Extension,
        S::DateModified => Column::DateModified,
        S::DateCreated => Column::DateCreated,
        S::Attributes => Column::Attributes,
        S::DateRecentlyChanged => Column::DateRecentlyChanged,
        _ => return None,
    };
    Some((col, sort.ascending))
}

/// The path the window shows (and opens) for a server path (Everything's link types).
pub fn client_path(server_path: &str, host: &str, link: EtpLinkType) -> String {
    let b = server_path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\');
    match link {
        EtpLinkType::Ftp | EtpLinkType::Server => server_path.to_string(),
        _ if !drive => server_path.to_string(),
        EtpLinkType::Share | EtpLinkType::AdminShare => {
            let dollar = if link == EtpLinkType::AdminShare { "$" } else { "" };
            format!("\\\\{host}\\{}{dollar}{}", &server_path[..1], &server_path[2..])
        }
    }
}

fn quote_csv(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Builds the read-only result index and the view's result set (ids in server order,
/// re-sorted locally when the server cannot sort by the view's column).
pub fn build_results(
    items: &[ResultItem],
    req: &RemoteRequest,
    target: &RemoteTarget,
) -> Result<(IndexStore, ResultSet), String> {
    let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
    let mut paths = Vec::with_capacity(items.len());
    let known = |v: Option<u64>| v.filter(|&x| x != UNKNOWN).map(|x| x.to_string()).unwrap_or_default();
    for it in items {
        let path = client_path(&it.full_path(), &target.target.host, target.link);
        if path.is_empty() || path.contains(['\r', '\n']) {
            continue;
        }
        let attr = it.attributes.unwrap_or(0) | if it.folder { fnc_core::flags::attr::DIRECTORY } else { 0 };
        let size = if it.folder { String::new() } else { known(it.size) };
        efu.push_str(&format!(
            "{},{size},{},{},{attr}\n",
            quote_csv(&path),
            known(it.date_modified),
            known(it.date_created)
        ));
        paths.push(path);
    }
    let mut store = IndexStore::default();
    fnc_index::efu::load_efu(&mut store, efu.as_bytes(), &format!("etp://{}", target.label()))
        .map_err(|e| e.to_string())?;
    store.rebuild_name_order();
    let mut seen = HashSet::with_capacity(paths.len());
    let mut ids: Vec<_> =
        paths.iter().filter_map(|p| fnc_ipc::rows::resolve_path(&store, p)).filter(|id| seen.insert(*id)).collect();
    if etp_sort(&req.sort).is_none() {
        let sx = fnc_exec::SortExt { props: None, formulas: &[] };
        let _ = fnc_exec::sort_ids_with(
            &mut ids,
            &req.sort,
            &store,
            &fnc_exec::Caches::new(),
            &fnc_exec::CancelToken::new(),
            sx,
        );
    }
    let rs = ResultSet {
        ids,
        generation: store.generation(),
        instance: store.instance(),
        sort: Some(req.sort),
        ..ResultSet::default()
    };
    Ok((store, rs))
}

fn search_state(req: &RemoteRequest, offset: u32, count: u32) -> SearchState {
    let mut s = SearchState {
        search: req.text.clone(),
        mods: req.mods,
        sort: etp_sort(&req.sort).unwrap_or((Column::Name, true)),
        offset,
        count,
        ..SearchState::default()
    };
    if let Some((f, m)) = &req.filter {
        s.filter_search = f.clone();
        s.filter_mods = *m;
    }
    s.fields.path = true;
    s.fields.size = true;
    s.fields.date_modified = true;
    s.fields.date_created = true;
    s.fields.attributes = true;
    s
}

fn outcome(req: &RemoteRequest, rs: ResultSet, started: Instant) -> SearchOutcome {
    let parsed = fnc_query::parse(&req.text, &fnc_query::ParseEnv::default());
    let plan = fnc_query::plan(&parsed.ast, &fnc_query::PlanEnv::default(), None);
    SearchOutcome {
        id: req.id,
        text: req.text.clone(),
        results: Arc::new(rs),
        plan: Arc::new(plan),
        parse_diagnostics: Vec::new(),
        plan_diagnostics: Vec::new(),
        elapsed: started.elapsed(),
    }
}

fn worker(
    target: &RemoteTarget,
    index: &SharedIndex,
    rx: &Receiver<Cmd>,
    ev: &Sender<RemoteEvent>,
    wake: &(dyn Fn() + Send + Sync),
) {
    let send = |e: RemoteEvent| {
        let _ = ev.send(e);
        wake();
    };
    let mut client = match EtpClient::connect(&target.target, TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            send(RemoteEvent::Disconnected(e.to_string()));
            return;
        }
    };
    tracing::info!(server = %target.label(), "connected to ETP server");
    send(RemoteEvent::Connected { welcome: client.welcome().to_string() });
    let mut pending: Option<RemoteRequest> = None;
    loop {
        let req = match pending.take() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(Cmd::Search(r)) => r,
                Ok(Cmd::Shutdown) | Err(_) => break,
            },
        };
        // Only the newest queued search matters.
        let mut req = req;
        let mut stop = false;
        while let Ok(c) = rx.try_recv() {
            match c {
                Cmd::Search(r) => req = r,
                Cmd::Shutdown => stop = true,
            }
        }
        if stop {
            break;
        }
        match run(&mut client, &req, target, index, rx, &mut pending, &send) {
            Ok(()) => {}
            Err(RunError::Stop) => break,
            Err(RunError::Client(ClientError::Reply { code, text })) => {
                send(RemoteEvent::QueryFailed(format!("{code} {text}")));
            }
            Err(RunError::Client(e)) => {
                send(RemoteEvent::Disconnected(e.to_string()));
                return;
            }
        }
    }
    client.quit();
}

enum RunError {
    Stop,
    Client(ClientError),
}

impl From<ClientError> for RunError {
    fn from(e: ClientError) -> Self {
        RunError::Client(e)
    }
}

fn run(
    client: &mut EtpClient,
    req: &RemoteRequest,
    target: &RemoteTarget,
    index: &SharedIndex,
    rx: &Receiver<Cmd>,
    pending: &mut Option<RemoteRequest>,
    send: &dyn Fn(RemoteEvent),
) -> Result<(), RunError> {
    let started = Instant::now();
    let max = req.max_results.max(1);
    let page = client.query(&search_state(req, 0, FIRST_PAGE.min(max)))?;
    let total = page.total;
    let mut items = page.items;
    let publish = |items: &[ResultItem]| match build_results(items, req, target) {
        Ok((store, rs)) => {
            index.replace(store);
            send(RemoteEvent::Results { outcome: outcome(req, rs, started), total });
        }
        Err(e) => send(RemoteEvent::QueryFailed(e)),
    };
    publish(&items);
    let want = total.min(u64::from(max));
    let mut more = false;
    while (items.len() as u64) < want {
        // A newer search makes the rest of this one useless.
        if let Ok(c) = rx.try_recv() {
            match c {
                Cmd::Search(r) => {
                    *pending = Some(r);
                    break;
                }
                Cmd::Shutdown => return Err(RunError::Stop),
            }
        }
        let offset = items.len() as u32;
        let count = CHUNK.min(max - offset);
        let p = client.query(&search_state(req, offset, count))?;
        if p.items.is_empty() {
            break;
        }
        items.extend(p.items);
        more = true;
    }
    if more && pending.is_none() {
        publish(&items);
    }
    Ok(())
}

/// Temporary folder for downloads from `target`.
pub fn download_dir(target: &RemoteTarget) -> PathBuf {
    let host: String = target
        .label()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    std::env::temp_dir().join("Fennec ETP").join(host)
}

/// Local file for a server path below `dir` (drive letters become folders; names that
/// could leave `dir` are rejected).
pub fn local_copy_path(dir: &Path, server_path: &str) -> Option<PathBuf> {
    let mut p = dir.to_path_buf();
    let mut any = false;
    for part in server_path.split(['\\', '/']).filter(|s| !s.is_empty()) {
        let clean: String =
            part.chars().filter(|c| !matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*') && !c.is_control()).collect();
        let clean = clean.trim_end_matches(['.', ' ']).to_string();
        if clean.is_empty() || clean == "." || clean == ".." {
            return None;
        }
        p.push(clean);
        any = true;
    }
    any.then_some(p)
}

fn download(target: &Target, server_path: &str, dir: &Path) -> Result<PathBuf, String> {
    let local = local_copy_path(dir, server_path).ok_or_else(|| format!("invalid path {server_path}"))?;
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = local.with_extension(format!(
        "{}fennec-part",
        local.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut c = EtpClient::connect(target, TIMEOUT).map_err(|e| e.to_string())?;
    let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let r = c.retr(server_path, &mut f);
    drop(f);
    c.quit();
    match r {
        Ok(_) => {
            let _ = std::fs::remove_file(&local);
            std::fs::rename(&part, &local).map_err(|e| e.to_string())?;
            Ok(local)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_core::settings::FoldersFirst;
    use fnc_exec::SortColumn;

    fn item(path: &str, name: &str, folder: bool, size: u64) -> ResultItem {
        let mut i = ResultItem::new(name, folder);
        i.path = Some(path.into());
        i.size = Some(size);
        i.date_modified = Some(133_000_000_000_000_000);
        i
    }

    fn target(link: EtpLinkType) -> RemoteTarget {
        RemoteTarget { target: Target { host: "srv".into(), port: 21, ..Target::default() }, link }
    }

    #[test]
    fn link_types_rewrite_drive_paths() {
        let p = "C:\\Data\\a.txt";
        assert_eq!(client_path(p, "srv", EtpLinkType::Ftp), p);
        assert_eq!(client_path(p, "srv", EtpLinkType::Server), p);
        assert_eq!(client_path(p, "srv", EtpLinkType::Share), "\\\\srv\\C\\Data\\a.txt");
        assert_eq!(client_path(p, "srv", EtpLinkType::AdminShare), "\\\\srv\\C$\\Data\\a.txt");
        assert_eq!(client_path("C:", "srv", EtpLinkType::Share), "\\\\srv\\C");
        // Non-drive paths are kept.
        assert_eq!(client_path("\\\\nas\\x\\y", "srv", EtpLinkType::Share), "\\\\nas\\x\\y");
        assert_eq!(client_path("/home/u", "srv", EtpLinkType::Share), "/home/u");
    }

    #[test]
    fn results_keep_server_order_and_metadata() {
        let items = [
            item("C:\\Data", "zeta.txt", false, 5),
            item("C:\\Data", "alpha", true, UNKNOWN),
            item("C:\\Data\\alpha", "mid.doc", false, 77),
            item("C:\\Data", "zeta.txt", false, 5),
        ];
        let req = RemoteRequest {
            id: 7,
            text: "a".into(),
            mods: Mods::default(),
            filter: None,
            sort: SortSpec::by(SortColumn::Size, false, FoldersFirst::Ascending),
            max_results: 100,
        };
        let (store, rs) = build_results(&items, &req, &target(EtpLinkType::Share)).unwrap();
        assert_eq!(rs.ids.len(), 3, "duplicates collapse");
        assert_eq!(rs.instance, store.instance());
        let paths: Vec<String> = rs
            .ids
            .iter()
            .map(|&id| {
                let mut s = String::new();
                store.full_path(id, &mut s);
                s
            })
            .collect();
        assert_eq!(
            paths,
            ["\\\\srv\\C\\Data\\zeta.txt", "\\\\srv\\C\\Data\\alpha", "\\\\srv\\C\\Data\\alpha\\mid.doc"]
        );
        assert_eq!(store.size(rs.ids[0]), Some(5));
        assert!(store.is_dir(rs.ids[1]));
        assert_eq!(store.date_modified(rs.ids[2]).0, 133_000_000_000_000_000);
        // Read-only and offline: nothing is read from these paths.
        assert!(store.flags(rs.ids[0]).contains(fnc_core::EntryFlags::OFFLINE));
        // A sort ETP does not have is applied locally.
        let req = RemoteRequest { sort: SortSpec::by(SortColumn::Type, true, FoldersFirst::Ascending), ..req };
        assert_eq!(etp_sort(&req.sort), None);
        let (store, rs) = build_results(&items, &req, &target(EtpLinkType::Ftp)).unwrap();
        assert_eq!(store.name_string(rs.ids[0]), "alpha", "folders first");
    }

    #[test]
    fn download_paths_stay_in_the_folder() {
        let dir = Path::new("/tmp/x");
        assert_eq!(local_copy_path(dir, "C:\\Data\\a.txt"), Some(dir.join("C").join("Data").join("a.txt")));
        assert_eq!(local_copy_path(dir, "\\\\srv\\share\\b"), Some(dir.join("srv").join("share").join("b")));
        assert_eq!(local_copy_path(dir, "C:\\..\\x"), None);
        assert_eq!(local_copy_path(dir, ""), None);
        assert!(download_dir(&target(EtpLinkType::Ftp)).ends_with("srv_21"));
    }

    /// The session against a real server: connect, search (first page, then the rest),
    /// download a result.
    #[test]
    fn session_searches_and_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
        for i in 0..600 {
            efu.push_str(&format!("\"C:\\Data\\file{i:03}.txt\",{i},,,32\n"));
        }
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        // A real file for the download (folder index volume of the temp dir).
        std::fs::write(dir.path().join("hello.txt"), b"hi there").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let vol =
            s.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Folder, root.clone()), Some(1)).unwrap();
        let mut b = s.bulk_load(vol);
        let units: Vec<u16> = "hello.txt".encode_utf16().collect();
        b.push(2, 1, &units, &fnc_index::Meta { attributes: Some(32), size: Some(8), ..Default::default() }).unwrap();
        b.finish().unwrap();
        s.rebuild_name_order();
        let backend = Arc::new(fnc_ipc::IndexBackend::new(SharedIndex::new(s), Arc::new(fnc_ipc::DefaultHost)));
        let cfg = fnc_etp::EtpConfig { port: 0, password: "pw".into(), ..fnc_etp::EtpConfig::default() };
        let h = fnc_etp::start(cfg, backend).unwrap();
        let t = RemoteTarget {
            target: Target {
                host: "127.0.0.1".into(),
                port: h.local_addr().port(),
                username: "me".into(),
                password: "pw".into(),
            },
            link: EtpLinkType::Ftp,
        };
        let session = RemoteSession::connect(t, Arc::new(|| {}));
        let next = || session.events().recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(next(), RemoteEvent::Connected { .. }));
        session.search(RemoteRequest {
            id: 1,
            text: "file".into(),
            mods: Mods::default(),
            filter: None,
            sort: SortSpec::by(SortColumn::Name, true, FoldersFirst::Ascending),
            max_results: 500,
        });
        let RemoteEvent::Results { outcome, total } = next() else { panic!() };
        assert_eq!((total, outcome.results.len(), outcome.id), (600, 256, 1));
        let RemoteEvent::Results { outcome, .. } = next() else { panic!() };
        assert_eq!(outcome.results.len(), 500, "capped at max_results");
        let snap = session.index().snapshot();
        assert_eq!(snap.instance(), outcome.results.instance);
        assert_eq!(snap.name_string(outcome.results.ids[0]), "file000.txt");
        drop(snap);
        let server_path = format!("{root}{}hello.txt", std::path::MAIN_SEPARATOR);
        session.download(server_path, false);
        let RemoteEvent::Downloaded { path, reveal } = next() else { panic!() };
        assert!(!reveal);
        assert_eq!(std::fs::read(&path).unwrap(), b"hi there");
        let _ = std::fs::remove_file(&path);
        session.download("C:\\Data\\file001.txt".into(), false);
        assert!(matches!(next(), RemoteEvent::DownloadFailed(_)), "file lists are not downloadable");
        // A failing search keeps the connection.
        session.search(RemoteRequest {
            id: 2,
            text: "regex:(".into(),
            mods: Mods { regex: false, ..Mods::default() },
            filter: None,
            sort: SortSpec::default(),
            max_results: 10,
        });
        match next() {
            RemoteEvent::Results { outcome, .. } => assert_eq!(outcome.id, 2),
            RemoteEvent::QueryFailed(_) => {}
            e => panic!("{e:?}"),
        }
        drop(h);
    }
}

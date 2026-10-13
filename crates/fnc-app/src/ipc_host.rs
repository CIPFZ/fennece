//! The application's [`fnc_ipc::Host`]: what IPC, Everything-compatible and HTTP clients
//! see of the running app and the administrative commands they may run.
//!
//! The UI thread pushes its state (search options, filters, bookmarks, index status) with
//! [`AppHost::update`]; client threads read it. Commands that need the UI (exit, command
//! lines, refreshing the views) travel as [`HostEvent`]s and wake the UI; database work
//! runs on short-lived worker threads so no client thread blocks.

use std::sync::Arc;

use crossbeam_channel::Sender;
use fnc_core::settings::FoldersFirst;
use fnc_exec::SearchOptions;
use fnc_index::SharedIndex;
use fnc_ipc::proto::{BookmarkEntry, FilterEntry, Mods};
use fnc_ipc::{AdminCommand, Host, HostError, Peer, PeerKind};
use parking_lot::RwLock;

use crate::indexer::IndexerControl;

/// Requests for the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// Exit Fennec (`EVERYTHING_IPC_EXIT`, `Op::Exit`).
    Exit,
    /// Run counts changed: refresh every view.
    Refresh,
    /// An Everything command line (`COMMAND_LINE_UTF8`), split into arguments.
    CommandLine(Vec<String>),
}

/// State mirrored from the UI.
#[derive(Clone)]
pub struct HostState {
    /// Search options (settings, filter macros, type names; never the UI's omit set).
    pub options: SearchOptions,
    /// Folder placement.
    pub folders_first: FoldersFirst,
    /// Filters.
    pub filters: Vec<FilterEntry>,
    /// Bookmarks.
    pub bookmarks: Vec<BookmarkEntry>,
    /// Initial index built (searchable).
    pub db_loaded: bool,
    /// Indexing in progress.
    pub busy: bool,
    /// Status bar text of the index.
    pub status: String,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            options: SearchOptions::default(),
            folders_first: FoldersFirst::Ascending,
            filters: Vec::new(),
            bookmarks: Vec::new(),
            db_loaded: false,
            busy: false,
            status: String::new(),
        }
    }
}

/// [`Host`] of `fennec.exe`.
pub struct AppHost {
    shared: SharedIndex,
    control: Option<IndexerControl>,
    events: Sender<HostEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    state: RwLock<HostState>,
    admin: bool,
    appdata: bool,
}

impl AppHost {
    /// A host over `shared`; `control` saves/rescans (absent in tests), `events` reach the
    /// UI, which `wake` repaints.
    pub fn new(
        shared: SharedIndex,
        control: Option<IndexerControl>,
        events: Sender<HostEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
        portable: bool,
    ) -> Self {
        Self {
            shared,
            control,
            events,
            wake,
            state: RwLock::new(HostState::default()),
            admin: fnc_platform::privilege::is_elevated(),
            appdata: !portable,
        }
    }

    /// Changes the mirrored state.
    pub fn update(&self, f: impl FnOnce(&mut HostState)) {
        f(&mut self.state.write());
    }

    fn send(&self, e: HostEvent) {
        if self.events.send(e).is_ok() {
            (self.wake)();
        }
    }

    /// Runs `f` on a worker thread (database saves, run history).
    fn background(&self, name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), HostError> {
        std::thread::Builder::new().name(name.into()).spawn(f).map(|_| ()).map_err(|e| HostError::Failed(e.to_string()))
    }
}

/// Filters in their IPC form.
pub fn filter_entries(list: &[crate::filters::Filter]) -> Vec<FilterEntry> {
    list.iter()
        .map(|f| FilterEntry {
            name: f.name.clone(),
            search: f.search.clone(),
            macro_name: f.macro_name.clone(),
            mods: Mods {
                case: f.case,
                whole_word: f.whole_word,
                path: f.path,
                diacritics: f.diacritics,
                regex: f.regex,
                ..Mods::default()
            },
        })
        .collect()
}

/// Bookmarks in their IPC form.
pub fn bookmark_entries(list: &[crate::filters::Bookmark]) -> Vec<BookmarkEntry> {
    list.iter()
        .map(|b| BookmarkEntry {
            name: b.name.clone(),
            search: b.search.clone(),
            filter: b.filter.clone(),
            sort: b.sort.clone(),
        })
        .collect()
}

/// Splits an Everything command line (`-s "a b" -newwindow`) like Windows does for
/// `argv`: whitespace separates, double quotes group, `""` inside quotes is a quote. A
/// leading program name (`…\Everything.exe`, `fennec.exe`) is dropped.
pub fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                cur.push('"');
            }
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_arg {
                    out.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            c => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        out.push(cur);
    }
    if out.first().is_some_and(|a| a.to_ascii_lowercase().ends_with(".exe")) {
        out.remove(0);
    }
    out
}

impl Host for AppHost {
    fn search_options(&self) -> SearchOptions {
        self.state.read().options.clone()
    }

    fn folders_first(&self) -> FoldersFirst {
        self.state.read().folders_first
    }

    fn is_db_loaded(&self) -> bool {
        self.state.read().db_loaded
    }

    fn is_busy(&self) -> bool {
        self.state.read().busy
    }

    fn is_admin(&self) -> bool {
        self.admin
    }

    fn is_appdata(&self) -> bool {
        self.appdata
    }

    fn status_text(&self) -> String {
        self.state.read().status.clone()
    }

    /// Native-pipe clients must run in this session with at least medium integrity
    /// (SPEC ch.4 §3.1); window messages never cross sessions (and UIPI applies), so
    /// Everything-compatible clients are allowed; HTTP clients never run admin commands.
    fn authorize(&self, cmd: AdminCommand, peer: &Peer) -> bool {
        let ok = match peer.kind {
            PeerKind::Local | PeerKind::CopyData => true,
            PeerKind::Http => false,
            PeerKind::Pipe => peer.pid.is_some_and(fnc_platform::session::is_trusted_peer),
        };
        if !ok {
            tracing::warn!(?cmd, ?peer, "IPC command denied");
        }
        ok
    }

    fn command(&self, cmd: AdminCommand) -> Result<(), HostError> {
        tracing::info!(?cmd, "IPC command");
        match cmd {
            // SPEC-GAP: the indexer cannot restart while running (sources are fixed at
            // start-up, like `-reindex`); rebuilding needs a restart with `-reindex`.
            AdminCommand::Rebuild => Err(HostError::Unsupported),
            AdminCommand::Update => {
                let c = self.control.as_ref().ok_or(HostError::Unsupported)?;
                c.rescan_folders();
                Ok(())
            }
            AdminCommand::SaveDb | AdminCommand::SaveRunHistory => {
                // Run history is part of the database.
                let c = self.control.clone().ok_or(HostError::Unsupported)?;
                self.background("fennec-ipc-save", move || c.save())
            }
            AdminCommand::DeleteRunHistory => {
                let (shared, control, events, wake) =
                    (self.shared.clone(), self.control.clone(), self.events.clone(), self.wake.clone());
                self.background("fennec-ipc-run-history", move || {
                    shared.write().clear_run_history();
                    if let Some(c) = control {
                        c.save();
                    }
                    if events.send(HostEvent::Refresh).is_ok() {
                        wake();
                    }
                })
            }
            AdminCommand::Exit => {
                self.send(HostEvent::Exit);
                Ok(())
            }
        }
    }

    fn run_count_changed(&self) {
        self.send(HostEvent::Refresh);
    }

    fn filters(&self) -> Vec<FilterEntry> {
        self.state.read().filters.clone()
    }

    fn bookmarks(&self) -> Vec<BookmarkEntry> {
        self.state.read().bookmarks.clone()
    }

    fn command_line(&self, _show_command: u32, command_line: &str) -> bool {
        let args = split_command_line(command_line);
        if args.is_empty() {
            return false;
        }
        self.send(HostEvent::CommandLine(args));
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fnc_ipc::IndexBackend;
    use fnc_ipc::proto::{Op, Payload};

    use super::*;

    fn host() -> (Arc<AppHost>, crossbeam_channel::Receiver<HostEvent>, SharedIndex) {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\a\\b.txt\",1,,,32\n";
        let mut s = fnc_index::IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        let shared = SharedIndex::new(s);
        let (tx, rx) = crossbeam_channel::unbounded();
        (Arc::new(AppHost::new(shared.clone(), None, tx, Arc::new(|| {}), true)), rx, shared)
    }

    #[test]
    fn command_lines_split_like_windows() {
        assert_eq!(split_command_line(r#"-s "a b" -newwindow"#), ["-s", "a b", "-newwindow"]);
        assert_eq!(split_command_line(r#""C:\Program Files\Everything\Everything.exe" -s x"#), ["-s", "x"]);
        assert_eq!(split_command_line(r#"-s "say ""hi""" """#), ["-s", "say \"hi\"", ""]);
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn filters_bookmarks_and_events() {
        let (host, rx, shared) = host();
        let f = crate::filters::Filter {
            name: "Docs".into(),
            search: "ext:doc".into(),
            macro_name: "docs".into(),
            case: true,
            ..Default::default()
        };
        let b = crate::filters::Bookmark { name: "B".into(), search: "abc".into(), ..Default::default() };
        host.update(|s| {
            s.filters = filter_entries(&[f]);
            s.bookmarks = bookmark_entries(&[b]);
            s.db_loaded = true;
        });
        let backend = IndexBackend::new(shared.clone(), host.clone());
        match backend.handle(&Op::ListFilters, &Peer::LOCAL).unwrap() {
            Payload::Filters(v) => {
                assert_eq!(v[0].name, "Docs");
                assert!(v[0].mods.case);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(backend.handle(&Op::ListBookmarks, &Peer::LOCAL).unwrap(), Payload::Bookmarks(v) if v[0].search == "abc")
        );
        assert!(matches!(backend.handle(&Op::IsDbLoaded, &Peer::LOCAL).unwrap(), Payload::Bool(true)));
        // A run count change refreshes the views.
        backend.inc_run_count("C:\\a\\b.txt").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), HostEvent::Refresh);
        // DeleteRunHistory clears every count (on a worker thread) and refreshes.
        backend.handle(&Op::DeleteRunHistory, &Peer::LOCAL).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), HostEvent::Refresh);
        assert_eq!(backend.run_count("C:\\a\\b.txt").unwrap(), 0);
        // Exit and command lines go to the UI.
        backend.handle(&Op::Exit, &Peer::LOCAL).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), HostEvent::Exit);
        assert!(host.command_line(1, "-s abc"));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            HostEvent::CommandLine(vec!["-s".into(), "abc".into()])
        );
        assert!(backend.handle(&Op::Rebuild, &Peer::LOCAL).is_err());
    }

    #[test]
    fn pipe_clients_need_this_session() {
        let (host, _rx, _) = host();
        let me = Peer { kind: PeerKind::Pipe, pid: Some(std::process::id()) };
        if cfg!(any(windows, target_os = "linux")) {
            assert!(host.authorize(AdminCommand::Exit, &me));
        }
        assert!(!host.authorize(AdminCommand::Exit, &Peer { kind: PeerKind::Pipe, pid: None }));
        assert!(!host.authorize(AdminCommand::Exit, &Peer { kind: PeerKind::Pipe, pid: Some(u32::MAX - 1) }));
        assert!(!host.authorize(AdminCommand::SaveDb, &Peer { kind: PeerKind::Http, pid: None }));
        assert!(host.authorize(AdminCommand::SaveDb, &Peer { kind: PeerKind::CopyData, pid: None }));
    }
}

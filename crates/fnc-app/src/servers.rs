//! Server lifecycle (SPEC ch.4 §3–5): the native IPC pipe, the Everything-compatible
//! window, the HTTP server and the ETP/FTP server, started and stopped from the settings.
//!
//! All three share one [`IndexBackend`] (queries on index snapshots, independent of the
//! UI's engines). Starting and stopping happen on the `fennec-servers` thread — stopping
//! the HTTP server waits up to 2 s for running requests — so the UI thread only sends the
//! wanted configuration ([`Servers::apply`]) and reads [`Servers::status`].
//!
//! Zero cost when off: the HTTP runtime exists only while the server runs; the IPC accept
//! thread and the hidden window sleep in the kernel until a client arrives.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use fnc_core::settings::Settings;
use fnc_etp::{EtpConfig, EtpHandle};
use fnc_http::{HttpConfig, HttpHandle, HttpLabels};
use fnc_ipc::IndexBackend;
use fnc_ipc::server::{ServerConfig, ServerHandle};
use parking_lot::Mutex;

use crate::columns::ColumnId;
use crate::i18n::{t, t_args};

/// Where the servers run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServersContext {
    /// `-instance` name (pipe `Fennec.<instance>.ipc`, window class suffix).
    pub instance: String,
    /// Portable mode (`IS_APPDATA` reports `false`).
    pub portable: bool,
}

/// What should run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Desired {
    /// Native IPC server.
    pub ipc: bool,
    /// Everything-compatible window.
    pub compat: bool,
    /// HTTP server configuration (`Err`: invalid settings), `None` = off.
    pub http: Option<Result<HttpConfig, String>>,
    /// ETP/FTP server configuration (`Err`: invalid settings), `None` = off.
    pub etp: Option<Result<EtpConfig, String>>,
}

impl Desired {
    /// From the settings (`server` and `etp` pages).
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            ipc: s.ipc_server,
            compat: s.everything_ipc,
            http: s.http_enabled.then(|| http_config(s)),
            etp: s.etp_enabled.then(|| etp_config(s)),
        }
    }
}

/// The ETP/FTP server configuration of `s`.
pub fn etp_config(s: &Settings) -> Result<EtpConfig, String> {
    let bind: IpAddr =
        s.etp_bind.trim().parse().map_err(|_| t_args("msg-etp-bad-bind", &[("value", s.etp_bind.clone().into())]))?;
    let log = s.etp_log_file.trim();
    Ok(EtpConfig {
        bind,
        port: s.etp_port,
        username: s.etp_username.clone(),
        password: s.etp_password.clone(),
        allow_downloads: s.etp_allow_downloads,
        allow_port: s.etp_allow_port,
        max_connections: s.etp_max_connections.max(1) as usize,
        welcome: s.etp_welcome_message.clone(),
        log_file: (!log.is_empty()).then(|| log.into()),
        log_max_bytes: u64::from(s.etp_log_max_kb.max(4)) * 1024,
        ..EtpConfig::default()
    })
}

/// Localised ETP start failure.
fn etp_error(e: &fnc_etp::EtpError) -> String {
    match e {
        fnc_etp::EtpError::NoPassword => t("msg-etp-no-password"),
        fnc_etp::EtpError::Bind { addr, source } => {
            t_args("msg-http-bind-failed", &[("addr", addr.to_string().into()), ("error", source.to_string().into())])
        }
        other => other.to_string(),
    }
}

/// The HTTP server configuration of `s`.
pub fn http_config(s: &Settings) -> Result<HttpConfig, String> {
    let bind: IpAddr = s
        .http_bind
        .trim()
        .parse()
        .map_err(|_| t_args("msg-http-bad-bind", &[("value", s.http_bind.clone().into())]))?;
    let labels = HttpLabels {
        search: t("http-search"),
        results: t_args("http-results", &[("count", "{count}".into())]),
        previous: t("http-previous"),
        next: t("http-next"),
        name: ColumnId::Name.title(),
        path: ColumnId::Path.title(),
        size: ColumnId::Size.title(),
        date_modified: ColumnId::DateModified.title(),
        date_created: ColumnId::DateCreated.title(),
        attributes: ColumnId::Attributes.title(),
    };
    let root = s.http_static_root.trim();
    Ok(HttpConfig {
        bind,
        port: s.http_port,
        username: s.http_username.clone(),
        password: s.http_password.clone(),
        allow_downloads: s.http_allow_downloads,
        page_size: u64::from(s.http_page_size.max(1)),
        static_root: (!root.is_empty()).then(|| root.into()),
        max_results: u64::from(s.http_max_results.max(1)),
        max_concurrency: s.http_max_concurrency.max(1) as usize,
        request_timeout: Duration::from_millis(u64::from(s.http_timeout_ms.max(100))),
        access_log: s.http_access_log,
        labels,
        ..HttpConfig::default()
    })
}

/// Localised start failure.
fn http_error(e: &fnc_http::HttpError) -> String {
    match e {
        fnc_http::HttpError::NoPassword => t("msg-http-no-password"),
        fnc_http::HttpError::Bind { addr, source } => {
            t_args("msg-http-bind-failed", &[("addr", addr.to_string().into()), ("error", source.to_string().into())])
        }
        other => other.to_string(),
    }
}

/// HTTP server state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HttpStatus {
    /// Disabled.
    #[default]
    Off,
    /// Listening.
    Running(SocketAddr),
    /// Could not start.
    Failed(String),
}

/// ETP/FTP server state (like [`HttpStatus`]).
pub type EtpStatus = HttpStatus;

/// Everything-compatible window state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CompatStatus {
    /// Disabled.
    #[default]
    Off,
    /// The window exists.
    Running,
    /// Everything (or another Fennec) owns the window class.
    Conflict,
    /// Could not be created (or not on Windows).
    Failed(String),
}

/// Current state of every server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServersStatus {
    /// Native IPC: `None` off, `Some(Ok)` listening, `Some(Err)` failed.
    pub ipc: Option<Result<(), String>>,
    /// Compatibility window.
    pub compat: CompatStatus,
    /// HTTP server.
    pub http: HttpStatus,
    /// ETP/FTP server.
    pub etp: EtpStatus,
    /// Number of configurations applied so far (tests wait for it).
    pub applied: u64,
}

/// The server threads; dropping stops every server.
pub struct Servers {
    tx: Option<Sender<Desired>>,
    status: Arc<Mutex<ServersStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl Servers {
    /// Starts the `fennec-servers` thread with `desired`; `wake` repaints the UI after
    /// every status change.
    pub fn start(
        ctx: &ServersContext,
        backend: Arc<IndexBackend>,
        desired: Desired,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let _ = tx.send(desired);
        let status = Arc::new(Mutex::new(ServersStatus::default()));
        let (st, instance) = (status.clone(), ctx.instance.clone());
        let thread = std::thread::Builder::new()
            .name("fennec-servers".into())
            .spawn(move || worker(&instance, &backend, &rx, &st, &*wake));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::error!(error = %e, "cannot start the server thread");
                None
            }
        };
        Self { tx: Some(tx), status, thread }
    }

    /// Requests a configuration (applied asynchronously; unchanged parts keep running).
    pub fn apply(&self, desired: Desired) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(desired);
        }
    }

    /// Current state.
    pub fn status(&self) -> ServersStatus {
        self.status.lock().clone()
    }

    /// Stops every server and waits for the thread.
    pub fn shutdown(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Everything-compatible window handler: `fnc_ipc::compat` on the backend.
struct CompatBridge(Arc<IndexBackend>);

impl fnc_platform::compat::CompatHandler for CompatBridge {
    fn wm_ipc(&self, wparam: usize, lparam: isize) -> isize {
        fnc_ipc::compat::handle_wm_ipc(wparam, lparam, &self.0)
    }

    fn copydata(&self, dw_data: usize, bytes: Vec<u8>) -> fnc_platform::compat::CopyDataAction {
        use fnc_ipc::compat::{CopyDataRequest, decode_copydata, execute_copydata};
        use fnc_platform::compat::{CopyDataAction, CopyDataReply};
        match decode_copydata(dw_data, &bytes) {
            Err(e) => {
                tracing::debug!(error = %e, "ignored WM_COPYDATA");
                CopyDataAction::Done(0)
            }
            Ok(req @ (CopyDataRequest::Query { .. } | CopyDataRequest::Query2 { .. })) => {
                let backend = self.0.clone();
                CopyDataAction::Query(Box::new(move || {
                    execute_copydata(&req, &backend).reply.map(|r| CopyDataReply {
                        hwnd: r.hwnd,
                        copydata_id: r.copydata_id,
                        bytes: r.bytes,
                    })
                }))
            }
            Ok(other) => CopyDataAction::Done(execute_copydata(&other, &self.0).result),
        }
    }

    fn close(&self) {
        // Like EVERYTHING_IPC_EXIT (window messages come from this session).
        let peer = fnc_ipc::Peer { kind: fnc_ipc::PeerKind::CopyData, pid: None };
        if let Err(e) = self.0.admin(fnc_ipc::AdminCommand::Exit, &peer) {
            tracing::warn!(error = %e, "WM_CLOSE on the Everything IPC window");
        }
    }
}

#[derive(Default)]
struct Running {
    ipc: Option<ServerHandle>,
    compat: Option<fnc_platform::compat::CompatWindow>,
    http: Option<HttpHandle>,
    etp: Option<EtpHandle>,
}

fn worker(
    instance: &str,
    backend: &Arc<IndexBackend>,
    rx: &Receiver<Desired>,
    status: &Mutex<ServersStatus>,
    wake: &(dyn Fn() + Send + Sync),
) {
    let mut cur = Desired::default();
    let mut run = Running::default();
    loop {
        // While a real Everything owns the IPC window, try again every 30 s (it may exit).
        let conflict = status.lock().compat == CompatStatus::Conflict;
        let (mut want, retry) = if conflict {
            match rx.recv_timeout(COMPAT_RETRY) {
                Ok(w) => (w, false),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => (cur.clone(), true),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(w) => (w, false),
                Err(_) => break,
            }
        };
        // Only the latest configuration matters.
        while let Ok(w) = rx.try_recv() {
            want = w;
        }
        let mut st = status.lock().clone();
        if want.ipc != cur.ipc || (want.ipc && run.ipc.is_none()) {
            run.ipc = None;
            st.ipc =
                want.ipc.then(|| match fnc_ipc::server::spawn(instance, backend.clone(), ServerConfig::default()) {
                    Ok(h) => {
                        run.ipc = Some(h);
                        Ok(())
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start the IPC server");
                        Err(e.to_string())
                    }
                });
        }
        if want.compat != cur.compat
            || (want.compat && run.compat.is_none() && (st.compat != CompatStatus::Conflict || retry))
        {
            run.compat = None;
            st.compat = if want.compat { start_compat(instance, backend, &mut run) } else { CompatStatus::Off };
        }
        if want.http != cur.http || (want.http.is_some() && run.http.is_none()) {
            // Stop first: the new configuration may use the same port.
            run.http = None;
            st.http = match &want.http {
                None => HttpStatus::Off,
                Some(Err(e)) => HttpStatus::Failed(e.clone()),
                Some(Ok(cfg)) => match fnc_http::start(cfg.clone(), backend.clone()) {
                    Ok(h) => {
                        let addr = h.local_addr();
                        run.http = Some(h);
                        HttpStatus::Running(addr)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start the HTTP server");
                        HttpStatus::Failed(http_error(&e))
                    }
                },
            };
        }
        if want.etp != cur.etp || (want.etp.is_some() && run.etp.is_none()) {
            run.etp = None;
            st.etp = match &want.etp {
                None => EtpStatus::Off,
                Some(Err(e)) => EtpStatus::Failed(e.clone()),
                Some(Ok(cfg)) => match fnc_etp::start(cfg.clone(), backend.clone()) {
                    Ok(h) => {
                        let addr = h.local_addr();
                        run.etp = Some(h);
                        EtpStatus::Running(addr)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start the ETP/FTP server");
                        EtpStatus::Failed(etp_error(&e))
                    }
                },
            };
        }
        cur = want;
        st.applied += 1;
        *status.lock() = st;
        wake();
    }
    // The app is exiting.
    drop(run.etp.take());
    drop(run.http.take());
    drop(run.compat.take());
    drop(run.ipc.take());
}

/// Retry interval for the Everything IPC window while Everything itself owns it.
const COMPAT_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

fn start_compat(instance: &str, backend: &Arc<IndexBackend>, run: &mut Running) -> CompatStatus {
    use fnc_platform::compat::{CompatConfig, CompatError, CompatWindow};
    let cfg = CompatConfig {
        class: fnc_ipc::compat::window_class(instance),
        ipc_message: fnc_ipc::compat::EVERYTHING_WM_IPC,
        announce: Some(fnc_ipc::compat::EVERYTHING_IPC_CREATED.to_string()),
    };
    match CompatWindow::start(&cfg, Arc::new(CompatBridge(backend.clone()))) {
        Ok(w) => {
            run.compat = Some(w);
            CompatStatus::Running
        }
        Err(CompatError::Conflict(pid)) => {
            tracing::info!(pid, class = %cfg.class, "Everything IPC window exists in another process; not created");
            CompatStatus::Conflict
        }
        Err(e) => {
            if !matches!(e, CompatError::Unsupported) {
                tracing::warn!(error = %e, "Everything IPC window");
            }
            CompatStatus::Failed(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use fnc_ipc::client::Client;
    use fnc_ipc::proto::{Column, Query};

    use super::*;

    fn wait(s: &Servers, n: u64) -> ServersStatus {
        let t = Instant::now();
        loop {
            let st = s.status();
            if st.applied >= n {
                return st;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "servers never applied {n}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn http_config_from_settings() {
        crate::i18n::init("en");
        let s =
            Settings { http_bind: "0.0.0.0".into(), http_port: 9000, http_password: "p".into(), ..Settings::default() };
        let c = http_config(&s).unwrap();
        assert_eq!(c.bind.to_string(), "0.0.0.0");
        assert_eq!(c.port, 9000);
        assert!(c.labels.results.contains("{count}"), "{}", c.labels.results);
        assert!(http_config(&Settings { http_bind: "nope".into(), ..Settings::default() }).is_err());
        assert!(Desired::from_settings(&Settings::default()).http.is_none(), "HTTP is off by default");
    }

    /// The native IPC server and the HTTP server start, answer and stop from the
    /// settings (the compatibility window is Windows-only, see `fnc_platform::compat`).
    #[test]
    fn servers_follow_the_settings() {
        crate::i18n::init("en");
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\x\\abc.iso\",5,,,32\n";
        let mut store = fnc_index::IndexStore::default();
        fnc_index::efu::load_efu(&mut store, efu.as_bytes(), "t.efu").unwrap();
        let backend = Arc::new(IndexBackend::new(fnc_index::SharedIndex::new(store), Arc::new(fnc_ipc::DefaultHost)));
        let ctx = ServersContext { instance: format!("servers-test-{}", std::process::id()), portable: true };
        let mut settings = Settings {
            everything_ipc: false,
            http_enabled: true,
            http_port: 0,
            http_username: "u".into(),
            http_password: "p".into(),
            ..Settings::default()
        };
        let servers = Servers::start(&ctx, backend, Desired::from_settings(&settings), Arc::new(|| {}));
        let st = wait(&servers, 1);
        assert_eq!(st.ipc, Some(Ok(())));
        let HttpStatus::Running(addr) = st.http else { panic!("{:?}", st.http) };
        assert!(addr.ip().is_loopback());
        let mut c = Client::connect(&ctx.instance, Duration::from_secs(5)).unwrap();
        let r = c.query(&Query { search: "abc".into(), columns: vec![Column::Name], ..Query::default() }).unwrap();
        assert_eq!(r.total, 1);
        drop(c);
        // No password: the server refuses to start and says why.
        settings.http_password.clear();
        servers.apply(Desired::from_settings(&settings));
        assert!(matches!(wait(&servers, 2).http, HttpStatus::Failed(_)));
        settings.http_enabled = false;
        settings.ipc_server = false;
        servers.apply(Desired::from_settings(&settings));
        let st = wait(&servers, 3);
        assert_eq!((st.ipc, st.http), (None, HttpStatus::Off));
        assert!(Client::connect(&ctx.instance, Duration::from_millis(300)).is_err(), "pipe closed");
    }

    /// ETP/FTP: off by default; starts with a password, answers an ETP query, stops.
    #[test]
    fn etp_server_follows_the_settings() {
        crate::i18n::init("en");
        assert!(Desired::from_settings(&Settings::default()).etp.is_none(), "ETP is off by default");
        assert!(etp_config(&Settings { etp_bind: "x".into(), ..Settings::default() }).is_err());
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\"C:\\x\\abc.iso\",5,,,32\n";
        let mut store = fnc_index::IndexStore::default();
        fnc_index::efu::load_efu(&mut store, efu.as_bytes(), "t.efu").unwrap();
        let backend = Arc::new(IndexBackend::new(fnc_index::SharedIndex::new(store), Arc::new(fnc_ipc::DefaultHost)));
        let ctx = ServersContext { instance: format!("etp-test-{}", std::process::id()), portable: true };
        let mut settings = Settings {
            ipc_server: false,
            everything_ipc: false,
            etp_enabled: true,
            etp_port: 0,
            etp_username: "u".into(),
            etp_password: fnc_core::secret::hash("p"),
            ..Settings::default()
        };
        let servers = Servers::start(&ctx, backend, Desired::from_settings(&settings), Arc::new(|| {}));
        let st = wait(&servers, 1);
        let HttpStatus::Running(addr) = st.etp else { panic!("{:?}", st.etp) };
        assert!(addr.ip().is_loopback());
        let target =
            fnc_etp::Target { host: "127.0.0.1".into(), port: addr.port(), username: "u".into(), password: "p".into() };
        let mut c = fnc_etp::EtpClient::connect(&target, Duration::from_secs(10)).unwrap();
        let page = c.query(&fnc_etp::SearchState { search: "abc".into(), ..Default::default() }).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "abc.iso");
        drop(c);
        settings.etp_password.clear();
        servers.apply(Desired::from_settings(&settings));
        assert_eq!(wait(&servers, 2).etp, HttpStatus::Failed(t("msg-etp-no-password")));
        settings.etp_enabled = false;
        servers.apply(Desired::from_settings(&settings));
        assert_eq!(wait(&servers, 3).etp, HttpStatus::Off);
        assert!(std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(), "port closed");
    }
}

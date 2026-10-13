//! The ETP/FTP server: one thread accepts, one thread per control connection; data
//! connections (passive or active) are opened per transfer on the session thread.
//!
//! Everything a client sees comes from the index: listings are built from index
//! entries ([`crate::namespace`]), searches run through [`IndexBackend`] (so property,
//! content and other functions work as over IPC/HTTP), and downloads open the path the
//! index holds for the requested entry — never the client's text.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fnc_core::FileTime;
use fnc_ipc::backend::{IndexBackend, SearchKey, sort_spec};
use fnc_ipc::proto::{Column, FLAG_ROOT, Mods, Status, UNKNOWN};
use parking_lot::Mutex;

use crate::namespace::{self, Node};
use crate::proto::{self, EverythingAction, MAX_LINE, ResultItem, SearchState};

/// Server settings (Options → ETP/FTP Server).
#[derive(Clone, Debug, PartialEq)]
pub struct EtpConfig {
    /// Bind address (default 127.0.0.1).
    pub bind: IpAddr,
    /// Port (default 21; 0 = any free port).
    pub port: u16,
    /// User name; empty accepts any user name (Everything).
    pub username: String,
    /// Password, or its hash from `fnc_core::secret::hash`; required.
    pub password: String,
    /// Allow `RETR`.
    pub allow_downloads: bool,
    /// Allow active mode (`PORT`/`EPRT`; only to the client's own address).
    pub allow_port: bool,
    /// Most connections at once (`421` beyond).
    pub max_connections: usize,
    /// Welcome message (`%n`, `%v`, `%%`; empty = default).
    pub welcome: String,
    /// Command log file.
    pub log_file: Option<PathBuf>,
    /// Log size limit in bytes (the oldest half is dropped beyond it).
    pub log_max_bytes: u64,
    /// Time limit of one search.
    pub query_timeout: Duration,
    /// Idle time after which a control connection is closed.
    pub idle_timeout: Duration,
    /// Time to wait for a data connection.
    pub data_timeout: Duration,
    /// Most results per `QUERY` reply.
    pub max_results: u64,
    /// Pause after a wrong password (slows down guessing).
    pub login_delay: Duration,
}

impl Default for EtpConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 21,
            username: String::new(),
            password: String::new(),
            allow_downloads: true,
            allow_port: true,
            max_connections: 16,
            welcome: String::new(),
            log_file: None,
            log_max_bytes: 4 << 20,
            query_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
            data_timeout: Duration::from_secs(30),
            max_results: 1_000_000,
            login_delay: Duration::from_millis(800),
        }
    }
}

/// Start failures.
#[derive(Debug, thiserror::Error)]
pub enum EtpError {
    /// No password configured.
    #[error("the ETP/FTP server needs a password")]
    NoPassword,
    /// The address could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// Address.
        addr: SocketAddr,
        /// Cause.
        source: io::Error,
    },
    /// Thread creation failed.
    #[error("cannot start the ETP/FTP server: {0}")]
    Thread(io::Error),
}

/// Command log (Everything's "Enable logging").
struct Log {
    path: PathBuf,
    max: u64,
    file: Mutex<Option<(File, u64)>>,
}

impl Log {
    fn open(path: PathBuf, max: u64) -> Self {
        Self { path, max: max.max(4096), file: Mutex::new(None) }
    }

    fn write(&self, peer: SocketAddr, text: &str) {
        let line =
            format!("{} [{peer}] {}\r\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), proto::one_line(text));
        let mut g = self.file.lock();
        if g.is_none() {
            let f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path);
            match f {
                Ok(f) => {
                    let len = f.metadata().map_or(0, |m| m.len());
                    *g = Some((f, len));
                }
                Err(e) => {
                    tracing::warn!(path = %self.path.display(), error = %e, "cannot open the ETP log");
                    return;
                }
            }
        }
        let Some((f, len)) = g.as_mut() else { return };
        if f.write_all(line.as_bytes()).is_ok() {
            *len += line.len() as u64;
        }
        if *len > self.max {
            *g = None;
            self.trim();
        }
    }

    /// Keeps the newest half of the log.
    fn trim(&self) {
        let Ok(bytes) = std::fs::read(&self.path) else { return };
        let keep = (self.max / 2) as usize;
        let mut start = bytes.len().saturating_sub(keep);
        if let Some(nl) = bytes[start..].iter().position(|&b| b == b'\n') {
            start += nl + 1;
        }
        if let Err(e) = std::fs::write(&self.path, &bytes[start..]) {
            tracing::warn!(error = %e, "cannot trim the ETP log");
        }
    }
}

struct Shared {
    config: EtpConfig,
    backend: Arc<IndexBackend>,
    stop: AtomicBool,
    active: AtomicUsize,
    next_id: AtomicU64,
    /// Control sockets of open sessions (shut down on stop).
    sessions: Mutex<HashMap<u64, TcpStream>>,
    log: Option<Log>,
}

impl Shared {
    fn log(&self, peer: SocketAddr, text: &str) {
        if let Some(l) = &self.log {
            l.write(peer, text);
        }
    }
}

/// A running server; dropping it stops the server.
pub struct EtpHandle {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl EtpHandle {
    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Connections currently open.
    pub fn connections(&self) -> usize {
        self.shared.active.load(Ordering::Relaxed)
    }

    /// Stops accepting and closes every connection.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wake the blocking accept.
        let wake = match self.addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.addr.port()),
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), self.addr.port())
            }
            _ => self.addr,
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_secs(1));
        for s in self.shared.sessions.lock().values() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        tracing::info!("ETP/FTP server stopped");
    }
}

impl Drop for EtpHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the server. Binds synchronously, so address errors are reported here.
pub fn start(config: EtpConfig, backend: Arc<IndexBackend>) -> Result<EtpHandle, EtpError> {
    if config.password.is_empty() {
        return Err(EtpError::NoPassword);
    }
    let want = SocketAddr::new(config.bind, config.port);
    let listener = TcpListener::bind(want).map_err(|source| EtpError::Bind { addr: want, source })?;
    let addr = listener.local_addr().map_err(EtpError::Thread)?;
    let log = config.log_file.clone().map(|p| Log::open(p, config.log_max_bytes));
    let shared = Arc::new(Shared {
        config,
        backend,
        stop: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        next_id: AtomicU64::new(1),
        sessions: Mutex::new(HashMap::new()),
        log,
    });
    let sh = shared.clone();
    let thread = std::thread::Builder::new()
        .name("fennec-etp".into())
        .spawn(move || accept_loop(&listener, &sh))
        .map_err(EtpError::Thread)?;
    tracing::info!(%addr, "ETP/FTP server listening");
    Ok(EtpHandle { addr, shared, thread: Some(thread) })
}

fn accept_loop(listener: &TcpListener, sh: &Arc<Shared>) {
    for conn in listener.incoming() {
        if sh.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut stream = match conn {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "ETP accept");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        if sh.active.load(Ordering::SeqCst) >= sh.config.max_connections.max(1) {
            let _ = stream.write_all(b"421 Too many connections, try again later.\r\n");
            continue;
        }
        let id = sh.next_id.fetch_add(1, Ordering::Relaxed);
        let Ok(clone) = stream.try_clone() else { continue };
        sh.active.fetch_add(1, Ordering::SeqCst);
        sh.sessions.lock().insert(id, clone);
        let sh2 = sh.clone();
        let spawned = std::thread::Builder::new().name("fennec-etp-session".into()).spawn(move || {
            if let Err(e) = Session::run(&sh2, stream) {
                tracing::debug!(error = %e, "ETP session ended");
            }
            sh2.sessions.lock().remove(&id);
            sh2.active.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            sh.sessions.lock().remove(&id);
            sh.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// How the next transfer connects.
enum DataSetup {
    None,
    Passive(TcpListener),
    Active(SocketAddr),
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Login {
    NeedUser,
    /// The user name did not match: ask for a password anyway (do not reveal which part
    /// was wrong).
    BadUserNeedPass,
    NeedPass,
    LoggedIn,
}

struct Session<'a> {
    sh: &'a Shared,
    ctrl: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
    login: Login,
    failures: u32,
    cwd: String,
    rest: u64,
    data: DataSetup,
    search: SearchState,
    quit: bool,
}

fn same_ip(a: IpAddr, b: IpAddr) -> bool {
    let canon = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    canon(a) == canon(b)
}

/// Reads one line of at most [`MAX_LINE`] bytes; `Ok(None)` at end of stream. Longer
/// lines are consumed and reported as `Err(InvalidData)`.
fn read_line(r: &mut impl BufRead, out: &mut Vec<u8>) -> io::Result<Option<()>> {
    out.clear();
    let mut too_long = false;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            // End of stream: a last line without a terminator still counts.
            return Ok((!out.is_empty() || too_long).then_some(()));
        }
        let (chunk, used, end) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1, true),
            None => (buf, buf.len(), false),
        };
        if out.len() + chunk.len() > MAX_LINE {
            too_long = true;
        } else {
            out.extend_from_slice(chunk);
        }
        r.consume(used);
        if end {
            if too_long {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            if out.last() == Some(&b'\r') {
                out.pop();
            }
            return Ok(Some(()));
        }
    }
}

/// Drops `-la`-style options and quotes from a `LIST`/`NLST` argument (Total Commander
/// sends `LIST -la`).
fn list_argument(arg: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut was_ws = true;
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && c.is_ascii_whitespace() {
            was_ws = true;
        } else if was_ws && c == '-' && !quoted {
            while chars.peek().is_some_and(|c| !c.is_ascii_whitespace()) {
                chars.next();
            }
        } else {
            out.push(c);
            was_ws = false;
        }
    }
    out
}

/// `PWD` quoting: embedded quotes are doubled (RFC 959).
fn quote_path(p: &str) -> String {
    p.replace('"', "\"\"")
}

/// The search text with the secondary `FILTER_SEARCH` ANDed, each with its own match
/// options. With a filter the expression is rebuilt from the parsed parts (a regex
/// search becomes a `regex:` term), so the returned options have `regex` off.
pub fn combined_search(state: &SearchState, options: &fnc_exec::SearchOptions) -> (String, Mods) {
    if state.filter_search.trim().is_empty() {
        return (state.search.clone(), state.mods);
    }
    use fnc_query::{ModSet, Modifier};
    let env = fnc_query::ParseEnv { precedence: options.precedence, user_macros: &options.macros, env_var: None };
    let m = &state.filter_mods;
    let filter = fnc_query::Filter {
        name: String::new(),
        search: state.filter_search.clone(),
        macro_name: None,
        case: m.case,
        whole_word: m.whole_word,
        path: m.path,
        diacritics: m.diacritics,
        regex: m.regex,
    };
    let mut extra = ModSet::EMPTY;
    for (on, modifier) in [
        (m.prefix, Modifier::Prefix),
        (m.suffix, Modifier::Suffix),
        (m.ignore_punctuation, Modifier::IgnorePunctuation),
        (m.ignore_whitespace, Modifier::IgnoreWhitespace),
    ] {
        if on {
            extra.set(modifier, true);
        }
    }
    let filter = fnc_query::filter::with_default_mods(filter.expr(&env), extra);
    let text = fnc_ipc::backend::search_ast(&state.search, &state.mods, options);
    let e = fnc_query::filter::combine(filter, text);
    (fnc_query::to_query_string(&e, options.precedence), Mods { regex: false, ..state.mods })
}

impl<'a> Session<'a> {
    fn run(sh: &'a Shared, stream: TcpStream) -> io::Result<()> {
        let peer = stream.peer_addr()?;
        let local = stream.local_addr()?;
        stream.set_read_timeout(Some(sh.config.idle_timeout.max(Duration::from_secs(1))))?;
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;
        let mut reader = BufReader::with_capacity(16 * 1024, stream.try_clone()?);
        let mut s = Session {
            sh,
            ctrl: stream,
            peer,
            local,
            login: Login::NeedUser,
            failures: 0,
            cwd: "/".into(),
            rest: 0,
            data: DataSetup::None,
            search: SearchState::default(),
            quit: false,
        };
        sh.log(peer, "connected");
        let banner = proto::welcome(&sh.config.welcome, env!("CARGO_PKG_VERSION"));
        s.ctrl.write_all(banner.as_bytes())?;
        let mut line = Vec::new();
        while !s.quit && !sh.stop.load(Ordering::SeqCst) {
            match read_line(&mut reader, &mut line) {
                Ok(None) => break,
                Ok(Some(())) => {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    s.command(&text)?;
                }
                Err(e) if e.kind() == io::ErrorKind::InvalidData => s.send("500 Line too long.")?,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    let _ = s.send("421 Timeout, closing control connection.");
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        sh.log(peer, "disconnected");
        Ok(())
    }

    fn send(&mut self, reply: &str) -> io::Result<()> {
        let mut r = reply.to_string();
        if !r.ends_with("\r\n") {
            r.push_str("\r\n");
        }
        self.ctrl.write_all(r.as_bytes())
    }

    fn command(&mut self, line: &str) -> io::Result<()> {
        let (verb, param) = proto::split_command(line);
        let verb = verb.to_ascii_uppercase();
        let shown = if verb == "PASS" { "PASS ****".to_string() } else { format!("{verb} {param}") };
        self.sh.log(self.peer, &shown);
        tracing::trace!(peer = %self.peer, command = %shown, "ETP");
        if verb.is_empty() {
            return Ok(());
        }
        // OPTS before login: Windows ftp.exe sends `OPTS UTF8 ON` right after connecting
        // and only decodes names as UTF-8 if that succeeds.
        let open = matches!(verb.as_str(), "USER" | "PASS" | "QUIT" | "NOOP" | "FEAT" | "HELP" | "AUTH" | "OPTS");
        if self.login != Login::LoggedIn && !open {
            return self.send("530 Not logged on.");
        }
        match verb.as_str() {
            "USER" => self.user(param),
            "PASS" => self.pass(param),
            "QUIT" => {
                self.quit = true;
                self.send("221 Goodbye.")
            }
            "NOOP" => self.send("200 OK."),
            "FEAT" => self.send(
                "211-Features:\r\n MDTM\r\n REST STREAM\r\n SIZE\r\n MLST type*;size*;modify*;\r\n MLSD\r\n UTF8\r\n EPSV\r\n EVERYTHING\r\n211 End",
            ),
            "HELP" => self.send("214-Site commands:\r\n EVERYTHING\r\n214 End"),
            "AUTH" => self.send("502 TLS is not supported."),
            "SYST" => self.send("215 UNIX emulated by Fennec"),
            "OPTS" => {
                let p = param.trim().to_ascii_uppercase();
                match p.as_str() {
                    "UTF8 ON" | "UTF-8 ON" => self.send("200 UTF8 mode enabled."),
                    "UTF8 OFF" | "UTF-8 OFF" => self.send("200 UTF8 mode disabled."),
                    p if p.starts_with("MLST") => self.send("200 MLST OPTS type;size;modify;"),
                    _ => self.send("501 Invalid option."),
                }
            }
            "PWD" | "XPWD" => {
                let r = format!("257 \"{}\" is current directory.", quote_path(&self.cwd));
                self.send(&r)
            }
            "CWD" | "XCWD" => self.cwd(param),
            "CDUP" | "XCUP" => {
                self.cwd = namespace::normalize(&self.cwd, "..");
                let r = format!("250 CDUP successful. \"{}\" is current directory.", quote_path(&self.cwd));
                self.send(&r)
            }
            "TYPE" => match param.trim().to_ascii_uppercase().as_str() {
                "A" | "A N" => self.send("200 Type set to ASCII (A)."),
                "I" | "L 8" => self.send("200 Type set to Image (I)."),
                "E" => self.send("504 Type E not implemented."),
                _ => {
                    let r = format!("500 Type {} not supported.", proto::one_line(param));
                    self.send(&r)
                }
            },
            "MODE" => match param.trim().to_ascii_uppercase().as_str() {
                "S" => self.send("200 Mode set to S."),
                "B" | "C" => self.send("504 Mode not implemented."),
                _ => self.send("501 Invalid mode."),
            },
            "STRU" => match param.trim().to_ascii_uppercase().as_str() {
                "F" => self.send("200 Structure set to F."),
                "R" | "P" => self.send("504 File structure not implemented."),
                _ => self.send("501 Invalid file structure."),
            },
            "REST" => {
                self.rest = param.trim().parse().unwrap_or(0);
                let r = format!("350 Restarting at {}.", self.rest);
                self.send(&r)
            }
            "ALLO" => self.send("202 No storage allocation necessary."),
            "ABOR" => self.send("226 No transfer to abort."),
            "STAT" if param.trim().is_empty() => self.send("211 Fennec ETP/FTP server."),
            "PASV" => self.pasv(),
            "EPSV" => self.epsv(param),
            "PORT" => self.port(param, false),
            "EPRT" => self.port(param, true),
            "LIST" | "NLST" | "MLSD" => self.listing(&verb, param),
            "MLST" => self.mlst(param),
            "SIZE" => self.size(param),
            "MDTM" => self.mdtm(param),
            "RETR" => self.retr(param),
            "STOR" | "STOU" | "APPE" | "DELE" | "RMD" | "XRMD" | "MKD" | "XMKD" | "RNFR" | "RNTO" | "MFMT" => {
                self.send("550 Permission denied (read-only server).")
            }
            "EVERYTHING" => {
                let (sub, p) = proto::split_command(param);
                self.everything(sub, p)
            }
            "SITE" => {
                let (sub, p) = proto::split_command(param);
                if sub.eq_ignore_ascii_case("EVERYTHING") {
                    let (sub, p) = proto::split_command(p);
                    self.everything(sub, p)
                } else if sub.eq_ignore_ascii_case("HELP") {
                    self.send("214-Site commands:\r\n EVERYTHING\r\n214 End")
                } else if sub.is_empty() {
                    self.send("500 SITE: Missing argument.")
                } else {
                    self.send("500 Unknown SITE command.")
                }
            }
            _ => self.send("500 Syntax error, command unrecognized."),
        }
    }

    fn user(&mut self, name: &str) -> io::Result<()> {
        let cfg = &self.sh.config;
        self.login =
            if !cfg.username.is_empty() && cfg.username != name { Login::BadUserNeedPass } else { Login::NeedPass };
        let r = format!("331 Password required for {}", proto::one_line(name));
        self.send(&r)
    }

    fn pass(&mut self, password: &str) -> io::Result<()> {
        match self.login {
            Login::LoggedIn => self.send("503 Invalid sequence of commands."),
            Login::NeedUser => self.send("503 Login with USER first."),
            state => {
                let ok = state == Login::NeedPass && fnc_core::secret::verify(password, &self.sh.config.password);
                if ok {
                    self.login = Login::LoggedIn;
                    self.failures = 0;
                    self.sh.log(self.peer, "logged on");
                    return self.send("230 Logged on.");
                }
                self.failures += 1;
                self.login = Login::NeedUser;
                std::thread::sleep(self.sh.config.login_delay);
                if self.failures >= 3 {
                    self.quit = true;
                    return self.send("421 Too many failed logins.");
                }
                self.send("530 Login or password incorrect!")
            }
        }
    }

    fn cwd(&mut self, arg: &str) -> io::Result<()> {
        let target = namespace::normalize(&self.cwd, arg);
        let snap = self.sh.backend.index().snapshot();
        let ok = match namespace::resolve(&snap, &target) {
            Some(Node::Virtual) => true,
            Some(Node::Entry(id)) => snap.is_dir(id),
            None => false,
        };
        drop(snap);
        if ok {
            self.cwd = target;
            let r = format!("250 CWD successful. \"{}\" is current directory.", quote_path(&self.cwd));
            self.send(&r)
        } else {
            self.send("550 Directory not found.")
        }
    }

    fn pasv(&mut self) -> io::Result<()> {
        let ip = match self.local.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(v6) => v6.to_ipv4_mapped(),
        };
        let Some(ip) = ip else {
            return self.send("500 PASV is not available when using IPv6, please use IPv4 or EPSV.");
        };
        match TcpListener::bind(SocketAddr::new(IpAddr::V4(ip), 0)).and_then(|l| l.local_addr().map(|a| (l, a))) {
            Ok((l, a)) => {
                self.data = DataSetup::Passive(l);
                self.send(&proto::pasv_reply(SocketAddrV4::new(ip, a.port())))
            }
            Err(e) => {
                let r = format!("500 Failed to enter passive mode: {e}");
                self.send(&r)
            }
        }
    }

    fn epsv(&mut self, arg: &str) -> io::Result<()> {
        if arg.trim().eq_ignore_ascii_case("ALL") {
            return self.send("200 EPSV ALL command successful.");
        }
        match TcpListener::bind(SocketAddr::new(self.local.ip(), 0)).and_then(|l| l.local_addr().map(|a| (l, a))) {
            Ok((l, a)) => {
                self.data = DataSetup::Passive(l);
                let r = format!("229 Entering Extended Passive Mode (|||{}|)", a.port());
                self.send(&r)
            }
            Err(e) => {
                let r = format!("500 Failed to enter passive mode: {e}");
                self.send(&r)
            }
        }
    }

    fn port(&mut self, arg: &str, extended: bool) -> io::Result<()> {
        if !self.sh.config.allow_port {
            return self.send("501 Invalid option.");
        }
        let addr = if extended { proto::parse_eprt(arg) } else { proto::parse_host_port(arg).map(SocketAddr::V4) };
        let Some(addr) = addr else {
            return self.send(if extended {
                "522 Extended Port Failure - unknown network protocol. Supported protocols: (1,2)"
            } else {
                "501 Syntax error in parameters."
            });
        };
        // No FTP bounce: data connections only go back to the client.
        if !same_ip(addr.ip(), self.peer.ip()) || addr.port() == 0 {
            return self.send("500 Failed to connect to IP.");
        }
        self.data = DataSetup::Active(addr);
        self.send("200 Port command successful.")
    }

    /// Opens the prepared data connection.
    fn open_data(&mut self) -> Result<TcpStream, &'static str> {
        let timeout = self.sh.config.data_timeout;
        let stream = match std::mem::replace(&mut self.data, DataSetup::None) {
            DataSetup::None => return Err("425 Use PORT or PASV first."),
            DataSetup::Active(addr) => TcpStream::connect_timeout(&addr, timeout.min(Duration::from_secs(10)))
                .map_err(|_| "425 Can't open data connection.")?,
            DataSetup::Passive(l) => {
                l.set_nonblocking(true).map_err(|_| "425 Can't open data connection.")?;
                let deadline = Instant::now() + timeout;
                loop {
                    match l.accept() {
                        // Only the client may connect (no data connection theft).
                        Ok((s, a)) if same_ip(a.ip(), self.peer.ip()) => break s,
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            if Instant::now() > deadline || self.sh.stop.load(Ordering::SeqCst) {
                                return Err("425 Can't open data connection.");
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return Err("425 Can't open data connection."),
                    }
                }
            }
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    /// Sends `body` over a new data connection with the 150/226 replies.
    fn transfer(&mut self, body: &mut dyn Read) -> io::Result<()> {
        let mut data = match self.open_data() {
            Ok(d) => d,
            Err(r) => return self.send(r),
        };
        self.send("150 Data connection accepted; transfer starting.")?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    drop(data);
                    return self.send("451 Transfer aborted: read error.");
                }
            };
            if data.write_all(&buf[..n]).is_err() {
                drop(data);
                return self.send("426 Connection closed; transfer aborted.");
            }
        }
        let _ = data.flush();
        let _ = data.shutdown(std::net::Shutdown::Write);
        drop(data);
        self.send("226 Transfer complete.")
    }

    fn listing(&mut self, verb: &str, arg: &str) -> io::Result<()> {
        let arg = if verb == "MLSD" { arg.to_string() } else { list_argument(arg) };
        let target = namespace::normalize(&self.cwd, &arg);
        let items = {
            let snap = self.sh.backend.index().snapshot();
            namespace::list(&snap, &target)
        };
        let Some(mut items) = items else {
            // Like Everything, a closed data setup stays usable for the next command.
            return self.send("550 Directory not found.");
        };
        items.sort_by_cached_key(|i| (!i.is_dir, i.name.to_lowercase()));
        let now = FileTime::now();
        let mut text = String::new();
        for i in &items {
            match verb {
                "NLST" => {
                    text.push_str(&proto::one_line(&i.name));
                    text.push_str("\r\n");
                }
                "MLSD" => {
                    text.push_str(&proto::mlsx_facts(i.is_dir, i.size, i.modified, &i.name));
                    text.push_str("\r\n");
                }
                _ => text.push_str(&proto::list_line(i.is_dir, i.size, i.modified, &i.name, now)),
            }
        }
        self.transfer(&mut io::Cursor::new(text.into_bytes()))
    }

    fn mlst(&mut self, arg: &str) -> io::Result<()> {
        let target = namespace::normalize(&self.cwd, arg);
        let facts = {
            let snap = self.sh.backend.index().snapshot();
            match namespace::resolve(&snap, &target) {
                Some(Node::Virtual) => Some(proto::mlsx_facts(true, None, FileTime::UNKNOWN, &target)),
                Some(Node::Entry(id)) => {
                    let dir = snap.is_dir(id);
                    Some(proto::mlsx_facts(dir, snap.size(id), snap.date_modified(id), &target))
                }
                None => None,
            }
        };
        match facts {
            Some(f) => {
                let r = format!("250-Listing {}\r\n {f}\r\n250 End.", proto::one_line(&target));
                self.send(&r)
            }
            None => self.send("550 File or Directory not found."),
        }
    }

    fn file_entry<T>(
        &self,
        arg: &str,
        f: impl FnOnce(&fnc_index::IndexStore, fnc_core::EntryId) -> Option<T>,
    ) -> Option<T> {
        let target = namespace::normalize(&self.cwd, arg);
        let snap = self.sh.backend.index().snapshot();
        match namespace::resolve(&snap, &target)? {
            Node::Entry(id) => f(&snap, id),
            Node::Virtual => None,
        }
    }

    fn size(&mut self, arg: &str) -> io::Result<()> {
        match self.file_entry(arg, |s, id| if s.is_dir(id) { None } else { s.size(id) }) {
            Some(n) => self.send(&format!("213 {n}")),
            None => self.send("550 File not found."),
        }
    }

    fn mdtm(&mut self, arg: &str) -> io::Result<()> {
        match self.file_entry(arg, |s, id| proto::mdtm_time(s.date_modified(id))) {
            Some(t) => self.send(&format!("213 {t}")),
            None => self.send("550 File not found."),
        }
    }

    fn retr(&mut self, arg: &str) -> io::Result<()> {
        let rest = std::mem::take(&mut self.rest);
        if !self.sh.config.allow_downloads {
            self.data = DataSetup::None;
            return self.send("550 File not found (downloads are disabled).");
        }
        let target = namespace::normalize(&self.cwd, arg);
        let path = {
            let snap = self.sh.backend.index().snapshot();
            namespace::downloadable(&snap, &target)
        };
        let file = path.as_ref().and_then(|p| File::open(p).ok()).filter(|f| f.metadata().is_ok_and(|m| m.is_file()));
        let Some(mut file) = file else {
            self.data = DataSetup::None;
            return self.send("550 File not found.");
        };
        if rest > 0 && file.seek(SeekFrom::Start(rest)).is_err() {
            self.data = DataSetup::None;
            return self.send("451 Transfer aborted: rest error.");
        }
        self.sh.log(self.peer, &format!("download {}", path.unwrap_or_default()));
        self.transfer(&mut file)
    }

    fn everything(&mut self, sub: &str, param: &str) -> io::Result<()> {
        match self.search.apply(sub, param) {
            EverythingAction::Reply(r) => self.send(&r),
            EverythingAction::Query => {
                let mut out = String::new();
                match self.query(&mut out) {
                    Ok(()) => self.ctrl.write_all(out.as_bytes()),
                    Err(e) => {
                        let r = format!("500 Query failed: {}", proto::one_line(&e));
                        self.send(&r)
                    }
                }
            }
        }
    }

    fn query(&mut self, out: &mut String) -> Result<(), String> {
        let st = &self.search;
        if st.hide_empty && st.search.trim().is_empty() && st.filter_search.trim().is_empty() {
            proto::write_query_reply(out, 0, &[]);
            return Ok(());
        }
        let backend = &self.sh.backend;
        let (search, mods) = combined_search(st, &backend.host().search_options());
        let key = SearchKey { search, mods, sort: sort_spec(&[st.sort], backend.host().folders_first()) };
        let rs = backend
            .search(&key, self.sh.config.query_timeout)
            .map_err(|e| if e.status == Status::Timeout { "timed out".to_string() } else { e.message.clone() })?;
        let f = st.fields;
        let mut cols = vec![Column::Name];
        for (on, c) in [
            (f.path, Column::Path),
            (f.size, Column::Size),
            (f.date_modified, Column::DateModified),
            (f.date_created, Column::DateCreated),
            (f.attributes, Column::Attributes),
            (f.file_list, Column::FileListName),
            (f.date_recently_changed, Column::DateRecentlyChanged),
        ] {
            if on {
                cols.push(c);
            }
        }
        let count = u64::from(st.count).min(self.sh.config.max_results);
        let page = backend.page(&rs, 0, u64::from(st.offset), count, &cols, false);
        let n = page.columns.flags.len();
        let mut items = Vec::with_capacity(n);
        for i in 0..n {
            let r = page.row(i);
            // Folder-index roots are named by their full path (`D:\x\data`): send them as
            // Everything does (`PATH D:\x`, `FOLDER data`); drive roots stay `C:`.
            let (path, name) = if r.flags & FLAG_ROOT != 0 && r.path.is_empty() {
                split_root(&r.name)
            } else {
                (r.path.clone(), r.name.clone())
            };
            let mut item = ResultItem::new(name, r.is_folder());
            item.path = f.path.then_some(path);
            item.size = f.size.then_some(if r.is_folder() { UNKNOWN } else { r.size });
            item.date_modified = f.date_modified.then_some(r.date_modified);
            item.date_created = f.date_created.then_some(r.date_created);
            item.attributes = f.attributes.then_some(if r.attributes == u32::MAX { 0 } else { r.attributes });
            item.file_list = f.file_list.then(|| r.file_list_name.clone());
            item.date_recently_changed = f.date_recently_changed.then_some(r.date_recently_changed);
            items.push(item);
        }
        proto::write_query_reply(out, page.total, &items);
        Ok(())
    }
}

/// Parent path and name of a root given by its full path (`D:\x\data` → `D:\x`, `data`;
/// `/home/u` → `/home`, `u`; `C:` and `/` unchanged).
fn split_root(full: &str) -> (String, String) {
    let t = full.trim_end_matches(['\\', '/']);
    match t.rfind(['\\', '/']) {
        Some(i) if i + 1 < t.len() && !t[..i].trim_start_matches(['\\', '/']).is_empty() => {
            (t[..i].to_string(), t[i + 1..].to_string())
        }
        Some(0) if t.len() > 1 => ("/".to_string(), t[1..].to_string()),
        _ => (String::new(), full.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_arguments_drop_options() {
        assert_eq!(list_argument("-la"), "");
        assert_eq!(list_argument("-la /C:/Data"), "/C:/Data");
        assert_eq!(list_argument("\"-dir with dash\""), "-dir with dash");
        assert_eq!(list_argument("a-b"), "a-b");
    }

    #[test]
    fn lines_are_bounded() {
        let mut r = io::Cursor::new(b"USER a\r\nNOOP\n".to_vec());
        let mut buf = Vec::new();
        assert!(read_line(&mut r, &mut buf).unwrap().is_some());
        assert_eq!(buf, b"USER a");
        assert!(read_line(&mut r, &mut buf).unwrap().is_some());
        assert_eq!(buf, b"NOOP");
        assert!(read_line(&mut r, &mut buf).unwrap().is_none());
        let long = format!("{}\nNOOP\n", "x".repeat(MAX_LINE + 10));
        let mut r = BufReader::with_capacity(64, io::Cursor::new(long.into_bytes()));
        assert_eq!(read_line(&mut r, &mut buf).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(read_line(&mut r, &mut buf).unwrap().is_some());
        assert_eq!(buf, b"NOOP", "the next command is still read");
    }

    #[test]
    fn filter_search_is_anded() {
        let o = fnc_exec::SearchOptions::default();
        let mut s = SearchState { search: "abc".into(), ..SearchState::default() };
        assert_eq!(combined_search(&s, &o).0, "abc");
        s.filter_search = "*.mp3|*.flac".into();
        // OR binds tighter than AND (Everything default).
        assert_eq!(combined_search(&s, &o).0, "*.mp3|*.flac abc");
        s.filter_mods.case = true;
        s.search.clear();
        assert_eq!(combined_search(&s, &o).0, "case:*.mp3|case:*.flac");
        // A regex search stays one regex next to a filter.
        s.search = "^a.c$".into();
        s.mods.regex = true;
        let (text, mods) = combined_search(&s, &o);
        assert!(!mods.regex);
        assert!(text.ends_with("regex:^a.c$") || text.contains("regex:\"^a.c$\""), "{text}");
    }

    #[test]
    fn roots_split_like_everything() {
        let s = |p: &str| {
            let (a, b) = split_root(p);
            (a, b)
        };
        assert_eq!(s("D:\\fennec-etp\\data"), ("D:\\fennec-etp".into(), "data".into()));
        assert_eq!(s("C:"), (String::new(), "C:".into()));
        assert_eq!(s("C:\\"), (String::new(), "C:\\".into()));
        assert_eq!(s("/home/u"), ("/home".into(), "u".into()));
        assert_eq!(s("/tmp"), ("/".into(), "tmp".into()));
        assert_eq!(s("\\\\srv\\share"), ("\\\\srv".into(), "share".into()));
    }

    #[test]
    fn mapped_addresses_compare_equal() {
        let v4: IpAddr = "127.0.0.1".parse().unwrap();
        let v6: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(same_ip(v4, v6));
        assert!(!same_ip(v4, "127.0.0.2".parse().unwrap()));
    }
}

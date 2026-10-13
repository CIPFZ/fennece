//! IFilter content extraction in a separate host process (SPEC ch.1 §9.2,
//! `content_ifilter_separate_process`; ADR-035).
//!
//! Text of formats the built-in parsers of `fnc-props` do not read (`.doc`, `.xls`,
//! `.ppt`, `.rtf`, `.odt`, …) comes from the IFilters installed on the system
//! (`LoadIFilter` → `IFilter::Init` / `GetChunk` / `GetText`). Filters are third-party
//! in-process COM servers that may crash, hang or leak, so they never run inside
//! Fennec: an [`IFilterClient`] starts a host process (`fennec.exe -ifilter-host`, see
//! [`host_main`]) and talks to it over its anonymous stdin/stdout pipes.
//!
//! * **Protocol** — length-prefixed little-endian frames; one [`Request`] per file
//!   (path + byte limit) answered by one [`Reply`] (UTF-8 text cut at the limit, "no
//!   text", or an error). Replies carry the request id; stale replies are dropped.
//! * **Isolation** — every request has a deadline ([`ClientOptions::timeout`]); on a
//!   timeout the host is killed (and the file remembered as poisoned, so it is not
//!   retried this session); on a crash (pipe closed) the host is reaped. The next
//!   request starts a fresh host. Hosts are recycled after
//!   [`ClientOptions::recycle_after`] files to bound leaks in filters.
//! * **Containment (Windows)** — hosts run in a job object (killed when Fennec exits
//!   for any reason, 1 GiB memory limit, no Windows Error Reporting dialog), with
//!   `BELOW_NORMAL_PRIORITY_CLASS` and, inside the host, `PROCESS_MODE_BACKGROUND_BEGIN`
//!   (low CPU, I/O and memory priority) and `SEM_NOGPFAULTERRORBOX`.
//! * **Registration check** — [`IFilterClient::has_filter`] reads the extension's
//!   persistent handler from `HKEY_CLASSES_ROOT` in the parent (no filter code loaded)
//!   and ignores the null filter, so only extensions with a real filter reach the host.
//!
//! Off Windows the protocol, the client and the host loop work (tests use the
//! test-only [`Kind::Echo`] / [`Kind::Sleep`] / [`Kind::Crash`] requests), but there are
//! no filters: [`filter_text`] fails and [`IFilterClient::has_filter`] is `false`.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

/// Command-line flag of the hidden host mode of `fennec.exe`.
pub const HOST_ARG: &str = "-ifilter-host";

/// Written by a host before its first reply: identifies the protocol and skips any
/// start-up noise on stdout (a host that does not send it in time is killed).
pub const MAGIC: &[u8; 8] = b"FNCIFH01";
/// Most bytes of stdout searched for [`MAGIC`].
const MAX_PREAMBLE: usize = 64 * 1024;

/// Largest frame accepted on either side (a reply holds at most the byte limit).
const MAX_FRAME: usize = 64 << 20;
/// Longest path accepted in a request (bytes of UTF-8).
const MAX_PATH_BYTES: usize = 64 * 1024;
/// Longest error message sent in a reply.
const MAX_ERROR: usize = 1024;

/// Request kinds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Extract the text of `path` with its IFilter (`arg` = byte limit).
    Extract = 1,
    /// Test only: sleep `arg` milliseconds, then reply "no text" (a hung filter).
    Sleep = 2,
    /// Test only: abort the host process (a crashing filter).
    Crash = 3,
    /// Test only: reply with the file's bytes as UTF-8 (lossy), cut at `arg` bytes.
    Echo = 4,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Extract,
            2 => Kind::Sleep,
            3 => Kind::Crash,
            4 => Kind::Echo,
            _ => return None,
        })
    }
}

/// One request: a file and a limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Id echoed by the reply.
    pub id: u32,
    /// What to do.
    pub kind: Kind,
    /// Byte limit of the text ([`Kind::Extract`], [`Kind::Echo`]) or milliseconds
    /// ([`Kind::Sleep`]).
    pub arg: u32,
    /// File path.
    pub path: String,
}

/// The answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Extracted text (UTF-8, at most the requested bytes).
    Text(String),
    /// The filter produced no text.
    NoText,
    /// The filter failed (no filter, unreadable file, `FILTER_E_*`, …).
    Error(String),
}

fn frame(payload: &[u8], w: &mut impl Write) -> io::Result<()> {
    let n = u32::try_from(payload.len()).map_err(|_| io::Error::other("frame too large"))?;
    w.write_all(&n.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Reads one frame; `Ok(None)` on a clean end of stream before the length.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn u32_at(b: &[u8], at: usize) -> io::Result<u32> {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short frame"))
}

/// Writes a request frame.
pub fn write_request(w: &mut impl Write, r: &Request) -> io::Result<()> {
    if r.path.len() > MAX_PATH_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "path too long"));
    }
    let mut p = Vec::with_capacity(9 + r.path.len());
    p.extend(r.id.to_le_bytes());
    p.push(r.kind as u8);
    p.extend(r.arg.to_le_bytes());
    p.extend(r.path.as_bytes());
    frame(&p, w)
}

/// Reads a request frame (`Ok(None)` at the end of the stream).
pub fn read_request(r: &mut impl Read) -> io::Result<Option<Request>> {
    let Some(b) = read_frame(r)? else { return Ok(None) };
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let id = u32_at(&b, 0)?;
    let kind = b.get(4).copied().and_then(Kind::from_u8).ok_or_else(|| bad("unknown request kind"))?;
    let arg = u32_at(&b, 5)?;
    let path = b.get(9..).filter(|p| p.len() <= MAX_PATH_BYTES).ok_or_else(|| bad("bad path"))?;
    let path = String::from_utf8(path.to_vec()).map_err(|_| bad("path is not UTF-8"))?;
    Ok(Some(Request { id, kind, arg, path }))
}

/// Writes a reply frame.
pub fn write_reply(w: &mut impl Write, id: u32, reply: &Reply) -> io::Result<()> {
    let (status, body): (u8, &[u8]) = match reply {
        Reply::Text(t) => (0, t.as_bytes()),
        Reply::NoText => (1, &[]),
        Reply::Error(e) => (2, &e.as_bytes()[..floor_char_boundary(e, MAX_ERROR)]),
    };
    let mut p = Vec::with_capacity(5 + body.len());
    p.extend(id.to_le_bytes());
    p.push(status);
    p.extend(body);
    frame(&p, w)
}

/// Reads a reply frame (`Ok(None)` at the end of the stream).
pub fn read_reply(r: &mut impl Read) -> io::Result<Option<(u32, Reply)>> {
    let Some(b) = read_frame(r)? else { return Ok(None) };
    let id = u32_at(&b, 0)?;
    let body = b.get(5..).unwrap_or_default();
    let text = || String::from_utf8_lossy(body).into_owned();
    let reply = match b.get(4) {
        Some(0) => Reply::Text(text()),
        Some(1) => Reply::NoText,
        Some(2) => Reply::Error(text()),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown reply status")),
    };
    Ok(Some((id, reply)))
}

/// Skips stdout bytes up to and including [`MAGIC`].
pub fn read_magic(r: &mut impl Read) -> io::Result<()> {
    let mut window = [0u8; 8];
    let mut byte = [0u8; 1];
    for n in 1..=MAX_PREAMBLE {
        r.read_exact(&mut byte)?;
        window.rotate_left(1);
        window[7] = byte[0];
        if n >= MAGIC.len() && &window == MAGIC {
            return Ok(());
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "not an IFilter host (no handshake)"))
}

/// Largest char boundary of `s` at or below `max`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Appends `text` to `out` without exceeding `max` bytes; `false` once full.
fn push_limited(out: &mut String, text: &str, max: usize) -> bool {
    let room = max.saturating_sub(out.len());
    let cut = floor_char_boundary(text, room);
    out.push_str(&text[..cut]);
    cut == text.len() && out.len() < max
}

// ---------------------------------------------------------------------------------
// Host side.

/// Options of the host loop.
#[derive(Copy, Clone, Debug, Default)]
pub struct HostOptions {
    /// Accept the test-only requests ([`Kind::Sleep`], [`Kind::Crash`], [`Kind::Echo`]).
    /// `fennec.exe` never sets this.
    pub test_requests: bool,
}

/// The host process: lowers its priority, then answers requests from stdin on stdout
/// until stdin closes. Returns the process exit code.
pub fn host_main(opts: HostOptions) -> i32 {
    imp::prepare_host_process();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut r = BufReader::new(stdin.lock());
    let mut w = BufWriter::new(stdout.lock());
    if w.write_all(MAGIC).and_then(|()| w.flush()).is_err() {
        return 2;
    }
    match serve(&mut r, &mut w, opts) {
        Ok(()) => 0,
        Err(_) => 2,
    }
}

/// The request loop of [`host_main`] over any streams (tested in-process).
pub fn serve(r: &mut impl Read, w: &mut impl Write, opts: HostOptions) -> io::Result<()> {
    while let Some(req) = read_request(r)? {
        let reply = answer(&req, opts);
        write_reply(w, req.id, &reply)?;
    }
    Ok(())
}

fn answer(req: &Request, opts: HostOptions) -> Reply {
    let max = req.arg as usize;
    match req.kind {
        Kind::Extract => match filter_text(Path::new(&req.path), max) {
            Ok(Some(t)) => Reply::Text(t),
            Ok(None) => Reply::NoText,
            Err(e) => Reply::Error(e),
        },
        _ if !opts.test_requests => Reply::Error("test requests are disabled".into()),
        Kind::Sleep => {
            std::thread::sleep(Duration::from_millis(u64::from(req.arg)));
            Reply::NoText
        }
        Kind::Crash => std::process::abort(),
        Kind::Echo => match std::fs::read(&req.path) {
            Ok(b) => {
                let s = String::from_utf8_lossy(&b);
                let mut t = String::new();
                push_limited(&mut t, &s, max);
                Reply::Text(t)
            }
            Err(e) => Reply::Error(e.to_string()),
        },
    }
}

/// Text of the file at `path` from its IFilter, at most `max_bytes` (`Ok(None)` if the
/// filter emits no text). Loads third-party code into the calling process: call it only
/// inside the host process.
pub fn filter_text(path: &Path, max_bytes: usize) -> Result<Option<String>, String> {
    imp::filter_text(path, max_bytes)
}

// ---------------------------------------------------------------------------------
// Client side.

/// How to start a host process.
#[derive(Clone, Debug)]
pub struct HostCommand {
    /// Executable (`fennec.exe`).
    pub program: PathBuf,
    /// Arguments ([`HOST_ARG`]).
    pub args: Vec<OsString>,
    /// Extra environment variables.
    pub envs: Vec<(OsString, OsString)>,
}

impl HostCommand {
    /// `fennec.exe -ifilter-host` for the running executable.
    pub fn current_exe() -> io::Result<Self> {
        Ok(Self { program: std::env::current_exe()?, args: vec![HOST_ARG.into()], envs: Vec::new() })
    }
}

/// Client limits.
#[derive(Copy, Clone, Debug)]
pub struct ClientOptions {
    /// Deadline of one request (start-up of a new host included); the host is killed
    /// when it passes.
    pub timeout: Duration,
    /// Host processes (parallel requests); at least 1.
    pub hosts: usize,
    /// A host is restarted after this many requests (filters may leak).
    pub recycle_after: u32,
    /// After a failed start, no host is started for this long.
    pub spawn_backoff: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(10), hosts: 2, recycle_after: 500, spawn_backoff: Duration::from_secs(30) }
    }
}

/// Why a request failed.
#[derive(Debug)]
pub enum ClientError {
    /// The host did not answer in time and was killed.
    Timeout,
    /// The host exited (crashed) while handling the request.
    Crashed,
    /// The host could not be started (or starting is backed off).
    Spawn(String),
    /// The file timed out or crashed a host before in this session.
    Poisoned,
    /// Broken stream.
    Io(io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Timeout => write!(f, "IFilter host timed out"),
            ClientError::Crashed => write!(f, "IFilter host crashed"),
            ClientError::Spawn(e) => write!(f, "cannot start the IFilter host: {e}"),
            ClientError::Poisoned => write!(f, "file skipped (timed out or crashed before)"),
            ClientError::Io(e) => write!(f, "IFilter host I/O: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Counters for diagnostics and tests.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientStats {
    /// Hosts started.
    pub spawned: u64,
    /// Hosts killed after a timeout.
    pub timeouts: u64,
    /// Hosts that exited unexpectedly.
    pub crashes: u64,
    /// Hosts restarted after `recycle_after` requests.
    pub recycled: u64,
}

struct Host {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    replies: Receiver<io::Result<(u32, Reply)>>,
    served: u32,
}

impl Host {
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Slot {
    host: Option<Host>,
}

/// Talks to IFilter host processes (thread-safe; requests on different hosts run in
/// parallel). Dropping the client stops its hosts.
pub struct IFilterClient {
    cmd: HostCommand,
    opts: ClientOptions,
    slots: Vec<Mutex<Slot>>,
    next_id: AtomicU32,
    poisoned: Mutex<HashSet<PathBuf>>,
    filters: Mutex<HashMap<String, bool>>,
    backoff_until: Mutex<Option<Instant>>,
    spawned: AtomicU64,
    timeouts: AtomicU64,
    crashes: AtomicU64,
    recycled: AtomicU64,
    job: imp::Job,
}

impl std::fmt::Debug for IFilterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IFilterClient").field("cmd", &self.cmd).field("opts", &self.opts).finish_non_exhaustive()
    }
}

/// Poison-tolerant lock (a panicking request leaves the slot usable).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Most poisoned paths remembered (oldest are not evicted; the set just stops growing).
const MAX_POISONED: usize = 4096;

impl IFilterClient {
    /// Client starting hosts with `cmd`.
    pub fn new(cmd: HostCommand, opts: ClientOptions) -> Self {
        let n = opts.hosts.max(1);
        Self {
            cmd,
            opts,
            slots: (0..n).map(|_| Mutex::new(Slot::default())).collect(),
            next_id: AtomicU32::new(1),
            poisoned: Mutex::new(HashSet::new()),
            filters: Mutex::new(HashMap::new()),
            backoff_until: Mutex::new(None),
            spawned: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            crashes: AtomicU64::new(0),
            recycled: AtomicU64::new(0),
            job: imp::Job::new(),
        }
    }

    /// `true` if an IFilter (other than the null filter) is registered for the extension
    /// (without dot, any case). Cached; always `false` off Windows.
    pub fn has_filter(&self, ext: &str) -> bool {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            return false;
        }
        if let Some(&v) = lock(&self.filters).get(&ext) {
            return v;
        }
        let v = imp::has_filter(&ext);
        lock(&self.filters).insert(ext, v);
        v
    }

    /// Text of `path` from its IFilter in a host process, at most `max_bytes`.
    pub fn extract(&self, path: &Path, max_bytes: usize) -> Result<Option<String>, ClientError> {
        let max = u32::try_from(max_bytes).unwrap_or(u32::MAX);
        match self.request(Kind::Extract, max, path)? {
            Reply::Text(t) => Ok(Some(t)),
            Reply::NoText => Ok(None),
            Reply::Error(e) => {
                tracing::debug!(path = %path.display(), error = %e, "IFilter failed");
                Ok(None)
            }
        }
    }

    /// Sends one request (any kind; the test-only kinds need a host started with
    /// [`HostOptions::test_requests`]).
    pub fn request(&self, kind: Kind, arg: u32, path: &Path) -> Result<Reply, ClientError> {
        if lock(&self.poisoned).contains(path) {
            return Err(ClientError::Poisoned);
        }
        let path_text = path.to_str().ok_or_else(|| ClientError::Io(io::Error::other("path is not Unicode")))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut slot = self.pick_slot(id);
        let deadline = Instant::now() + self.opts.timeout;
        if slot.host.as_ref().is_some_and(|h| h.served >= self.opts.recycle_after) {
            if let Some(h) = slot.host.take() {
                h.stop();
            }
            self.recycled.fetch_add(1, Ordering::Relaxed);
        }
        if slot.host.is_none() {
            slot.host = Some(self.spawn()?);
        }
        let Some(host) = slot.host.as_mut() else { return Err(ClientError::Spawn("no host".into())) };
        let req = Request { id, kind, arg, path: path_text.to_string() };
        if let Err(e) = write_request(&mut host.stdin, &req) {
            // The host is gone (crashed between requests): restart once.
            tracing::debug!(error = %e, "IFilter host pipe closed; restarting");
            if let Some(h) = slot.host.take() {
                h.stop();
            }
            self.crashes.fetch_add(1, Ordering::Relaxed);
            let mut fresh = self.spawn()?;
            write_request(&mut fresh.stdin, &req).map_err(ClientError::Io)?;
            slot.host = Some(fresh);
        }
        let Some(host) = slot.host.as_mut() else { return Err(ClientError::Spawn("no host".into())) };
        host.served += 1;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match host.replies.recv_timeout(left) {
                Ok(Ok((rid, reply))) if rid == id => return Ok(reply),
                // A late answer to an earlier (timed-out) request of this host.
                Ok(Ok(_)) => continue,
                Ok(Err(e)) => {
                    self.fail(&mut slot, path, false);
                    return Err(ClientError::Io(e));
                }
                Err(RecvTimeoutError::Timeout) => {
                    tracing::warn!(path = %path.display(), timeout = ?self.opts.timeout, "IFilter host timed out; killed");
                    self.fail(&mut slot, path, true);
                    return Err(ClientError::Timeout);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    tracing::warn!(path = %path.display(), "IFilter host crashed");
                    self.fail(&mut slot, path, false);
                    return Err(ClientError::Crashed);
                }
            }
        }
    }

    /// Counters.
    pub fn stats(&self) -> ClientStats {
        ClientStats {
            spawned: self.spawned.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            crashes: self.crashes.load(Ordering::Relaxed),
            recycled: self.recycled.load(Ordering::Relaxed),
        }
    }

    /// Process ids of the running hosts (tests, diagnostics).
    pub fn host_pids(&self) -> Vec<u32> {
        self.slots.iter().filter_map(|s| lock(s).host.as_ref().map(|h| h.child.id())).collect()
    }

    /// A free slot if any, else the slot of `id` (waits for it).
    fn pick_slot(&self, id: u32) -> MutexGuard<'_, Slot> {
        for s in &self.slots {
            if let Ok(g) = s.try_lock() {
                return g;
            }
        }
        lock(&self.slots[id as usize % self.slots.len()])
    }

    fn fail(&self, slot: &mut Slot, path: &Path, timeout: bool) {
        if let Some(h) = slot.host.take() {
            h.stop();
        }
        if timeout {
            self.timeouts.fetch_add(1, Ordering::Relaxed);
        } else {
            self.crashes.fetch_add(1, Ordering::Relaxed);
        }
        let mut p = lock(&self.poisoned);
        if p.len() < MAX_POISONED {
            p.insert(path.to_path_buf());
        }
    }

    fn spawn(&self) -> Result<Host, ClientError> {
        if let Some(t) = *lock(&self.backoff_until)
            && Instant::now() < t
        {
            return Err(ClientError::Spawn("backing off after a failed start".into()));
        }
        let mut cmd = Command::new(&self.cmd.program);
        cmd.args(&self.cmd.args)
            .envs(self.cmd.envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        imp::configure_command(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                *lock(&self.backoff_until) = Some(Instant::now() + self.opts.spawn_backoff);
                tracing::warn!(program = %self.cmd.program.display(), error = %e, "cannot start the IFilter host");
                return Err(ClientError::Spawn(e.to_string()));
            }
        };
        self.job.assign(&child);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ClientError::Spawn("no pipes".into()));
        };
        let (tx, rx) = crossbeam_channel::bounded(4);
        let reader = std::thread::Builder::new().name("fennec-ifilter-pipe".into()).spawn(move || {
            let mut r = BufReader::new(stdout);
            if let Err(e) = read_magic(&mut r) {
                let _ = tx.send(Err(e));
                return;
            }
            loop {
                match read_reply(&mut r) {
                    Ok(Some(m)) => {
                        if tx.send(Ok(m)).is_err() {
                            break;
                        }
                    }
                    // End of stream: the host exited; dropping `tx` disconnects.
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        if let Err(e) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ClientError::Spawn(e.to_string()));
        }
        self.spawned.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(pid = child.id(), "IFilter host started");
        Ok(Host { child, stdin: BufWriter::new(stdin), replies: rx, served: 0 })
    }
}

impl Drop for IFilterClient {
    fn drop(&mut self) {
        for s in &self.slots {
            if let Some(h) = lock(s).host.take() {
                h.stop();
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command};

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::IndexServer::{
        CHUNK_EOC, CHUNK_EOP, CHUNK_EOS, CHUNK_EOW, CHUNK_TEXT, FILTER_E_EMBEDDING_UNAVAILABLE, FILTER_E_END_OF_CHUNKS,
        FILTER_E_LINK_UNAVAILABLE, FILTER_E_NO_MORE_TEXT, FILTER_S_LAST_TEXT, IFilter, LoadIFilter, STAT_CHUNK,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, RRF_RT_REG_SZ, RegGetValueW};
    use windows::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN,
        SetPriorityClass,
    };
    use windows::core::{IUnknown, Interface, PCWSTR, PWSTR};

    use super::push_limited;

    /// `IID_IFilter` persistent-handler add-in key.
    const IFILTER_ADDIN: &str = "{89BCB740-6119-101A-BCB7-00DD010655AF}";
    /// The null persistent handler (registered for binaries: no text).
    const NULL_HANDLER: &str = "{098F2470-BAE0-11CD-B579-08002B30BFEB}";
    /// The null filter class.
    const NULL_FILTER: &str = "{C3278E90-BEA7-11CD-B579-08002B30BFEB}";
    /// Memory limit of one host process.
    const HOST_MEMORY: usize = 1 << 30;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Default (or `value`) string of `HKEY_CLASSES_ROOT\subkey`.
    fn reg_string(subkey: &str) -> Option<String> {
        let key = wide(std::ffi::OsStr::new(subkey));
        let mut buf = vec![0u16; 512];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: `key` is NUL-terminated; `buf` is writable for `bytes` bytes and the
        // function writes at most that many (RRF_RT_REG_SZ: NUL-terminated string).
        let rc = unsafe {
            RegGetValueW(
                HKEY_CLASSES_ROOT,
                PCWSTR(key.as_ptr()),
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if rc.is_err() {
            return None;
        }
        let n = (bytes as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        let s = s.trim_end_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// Persistent handler of `.ext`: directly, or through the ProgID's CLSID.
    fn persistent_handler(ext: &str) -> Option<String> {
        if let Some(h) = reg_string(&format!(".{ext}\\PersistentHandler")) {
            return Some(h);
        }
        let progid = reg_string(&format!(".{ext}"))?;
        let clsid = reg_string(&format!("{progid}\\CLSID"))?;
        reg_string(&format!("CLSID\\{clsid}\\PersistentHandler"))
    }

    pub fn has_filter(ext: &str) -> bool {
        let Some(handler) = persistent_handler(ext) else { return false };
        if handler.eq_ignore_ascii_case(NULL_HANDLER) {
            return false;
        }
        match reg_string(&format!("CLSID\\{handler}\\PersistentAddinsRegistered\\{IFILTER_ADDIN}")) {
            Some(filter) => !filter.eq_ignore_ascii_case(NULL_FILTER),
            None => false,
        }
    }

    pub fn configure_command(cmd: &mut Command) {
        cmd.creation_flags((CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS).0);
    }

    pub fn prepare_host_process() {
        // SAFETY: process-wide flags of this (host) process only: no critical-error or
        // fault dialogs, background CPU/I/O/memory priority. Failures are harmless.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            let _ = SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN);
        }
    }

    /// Job object holding the hosts: killed with Fennec, memory-limited, no WER dialog.
    pub struct Job(Option<HANDLE>);

    // SAFETY: a job handle is a kernel handle usable from any thread.
    unsafe impl Send for Job {}
    // SAFETY: only `AssignProcessToJobObject` (thread-safe) is called through `&self`.
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Self {
            // SAFETY: anonymous job, default security.
            let Ok(h) = (unsafe { CreateJobObjectW(None, PCWSTR::null()) }) else { return Self(None) };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            info.ProcessMemoryLimit = HOST_MEMORY;
            // SAFETY: `h` is the job just created; `info` is a properly sized and aligned
            // JOBOBJECT_EXTENDED_LIMIT_INFORMATION that outlives the call.
            let r = unsafe {
                SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if let Err(e) = r {
                tracing::warn!(error = %e, "IFilter job limits not set");
            }
            Self(Some(h))
        }

        pub fn assign(&self, child: &Child) {
            let Some(job) = self.0 else { return };
            // SAFETY: `job` is our open job handle; the child's process handle is owned by
            // `child` and valid while it is borrowed.
            if let Err(e) = unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) } {
                tracing::warn!(error = %e, "IFilter host not assigned to the job object");
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                // SAFETY: closes the job handle created in `new` exactly once (kills the
                // remaining hosts: KILL_ON_JOB_CLOSE).
                let _ = unsafe { CloseHandle(h) };
            }
        }
    }

    thread_local! {
        // COM stays initialised (STA) for the life of the calling thread.
        static COM: () = {
            // SAFETY: initialises COM for this thread; a thread already initialised in
            // another apartment model just gets an error, which is harmless here.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        };
    }

    pub fn filter_text(path: &Path, max_bytes: usize) -> Result<Option<String>, String> {
        COM.with(|()| {});
        let w = wide(path.as_os_str());
        let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated and outlives the call; on success `raw` receives
        // an AddRef'd interface pointer that is owned below.
        unsafe { LoadIFilter(PCWSTR(w.as_ptr()), None, &mut raw) }.map_err(|e| format!("LoadIFilter: {e}"))?;
        if raw.is_null() {
            return Err("LoadIFilter returned no filter".into());
        }
        // SAFETY: `raw` is a valid, owned COM pointer (released when dropped).
        let unknown = unsafe { IUnknown::from_raw(raw) };
        let filter: IFilter = unknown.cast().map_err(|e| format!("no IFilter: {e}"))?;
        // No canonicalisation flags: the system's plain-text and RTF filters emit no
        // chunks at all with IFILTER_INIT_CANON_PARAGRAPHS | HARD_LINE_BREAKS (verified on
        // Windows Server 2022); line breaks are normalised below instead.
        let flags = 0u32;
        let mut out_flags = 0u32;
        // SAFETY: valid filter; no attribute list; `out_flags` is a valid out-pointer.
        let hr = unsafe { filter.Init(flags, &[], &mut out_flags) };
        if hr < 0 {
            return Err(format!("IFilter::Init failed: {hr:#010x}"));
        }
        let mut out = String::new();
        let mut buf = vec![0u16; 8192];
        'chunks: loop {
            let mut stat = STAT_CHUNK::default();
            // SAFETY: valid filter; `stat` is an owned out-structure.
            let hr = unsafe { filter.GetChunk(&mut stat) };
            if hr == FILTER_E_END_OF_CHUNKS.0 {
                break;
            }
            if hr == FILTER_E_EMBEDDING_UNAVAILABLE.0 || hr == FILTER_E_LINK_UNAVAILABLE.0 {
                continue;
            }
            if hr < 0 {
                if out.is_empty() {
                    return Err(format!("IFilter::GetChunk failed: {hr:#010x}"));
                }
                break;
            }
            // Only text chunks; value chunks (properties) are skipped.
            if stat.flags.0 & CHUNK_TEXT.0 == 0 {
                continue;
            }
            let sep = match stat.breakType {
                b if b == CHUNK_EOW => " ",
                b if b == CHUNK_EOS || b == CHUNK_EOP || b == CHUNK_EOC => "\n",
                _ => "",
            };
            if !out.is_empty()
                && !sep.is_empty()
                && !out.ends_with(['\n', ' '])
                && !push_limited(&mut out, sep, max_bytes)
            {
                break;
            }
            loop {
                let mut n = buf.len() as u32;
                // SAFETY: `buf` is writable for `n` UTF-16 units; the filter writes at
                // most `n` and returns the count in `n`.
                let hr = unsafe { filter.GetText(&mut n, PWSTR(buf.as_mut_ptr())) };
                if hr == FILTER_E_NO_MORE_TEXT.0 || hr < 0 {
                    break;
                }
                let n = (n as usize).min(buf.len());
                let text =
                    String::from_utf16_lossy(&buf[..n]).replace("\r\n", "\n").replace('\r', "\n").replace('\0', "");
                if !push_limited(&mut out, &text, max_bytes) {
                    break 'chunks;
                }
                if hr == FILTER_S_LAST_TEXT.0 {
                    break;
                }
            }
        }
        let t = out.trim();
        Ok((!t.is_empty()).then(|| t.to_string()))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    use std::process::{Child, Command};

    pub fn has_filter(_ext: &str) -> bool {
        false
    }

    pub fn configure_command(_cmd: &mut Command) {}

    pub fn prepare_host_process() {}

    pub struct Job;

    impl Job {
        pub fn new() -> Self {
            Job
        }

        pub fn assign(&self, _child: &Child) {}
    }

    pub fn filter_text(_path: &Path, _max_bytes: usize) -> Result<Option<String>, String> {
        Err("IFilters exist only on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment variable that turns this test binary into a host (see `host_entry`).
    const HOST_ENV: &str = "FENNEC_IFILTER_TEST_HOST";

    /// Not a test: the host process of the client tests. The tests start this binary
    /// again with `--exact ifilter::tests::host_entry --ignored` and `HOST_ENV` set.
    #[test]
    #[ignore = "host process of the IFilter client tests"]
    fn host_entry() {
        if std::env::var_os(HOST_ENV).is_some() {
            std::process::exit(host_main(HostOptions { test_requests: true }));
        }
    }

    fn test_client(opts: ClientOptions) -> IFilterClient {
        let cmd = HostCommand {
            program: std::env::current_exe().unwrap(),
            args: ["--exact", "ifilter::tests::host_entry", "--ignored", "--quiet", "--test-threads=1"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            envs: vec![(HOST_ENV.into(), "1".into())],
        };
        IFilterClient::new(cmd, opts)
    }

    #[test]
    fn frames_round_trip() {
        let req = Request { id: 7, kind: Kind::Extract, arg: 4096, path: "C:\\docs\\季度.doc".into() };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        assert_eq!(read_request(&mut buf.as_slice()).unwrap(), Some(req));
        for reply in [Reply::Text("plan 季度".into()), Reply::NoText, Reply::Error("FILTER_E_ACCESS".into())] {
            let mut buf = Vec::new();
            write_reply(&mut buf, 9, &reply).unwrap();
            assert_eq!(read_reply(&mut buf.as_slice()).unwrap(), Some((9, reply)));
        }
        assert_eq!(read_request(&mut [].as_slice()).unwrap(), None);
        // Truncated, oversized and unknown frames are errors, not panics.
        assert!(read_request(&mut [5u8, 0, 0, 0, 1].as_slice()).is_err());
        assert!(read_request(&mut [0xff, 0xff, 0xff, 0x7f].as_slice()).is_err());
        let mut bad = Vec::new();
        frame(&[1, 0, 0, 0, 99, 0, 0, 0, 0], &mut bad).unwrap();
        assert!(read_request(&mut bad.as_slice()).is_err());
        let long = Request { id: 1, kind: Kind::Extract, arg: 1, path: "x".repeat(MAX_PATH_BYTES + 1) };
        assert!(write_request(&mut Vec::new(), &long).is_err());
        // The handshake skips start-up noise (libtest's "running 1 test" in the tests).
        let mut s = b"running 1 test\n".to_vec();
        s.extend(MAGIC);
        s.extend([1, 2]);
        let mut r = s.as_slice();
        read_magic(&mut r).unwrap();
        assert_eq!(r, [1, 2]);
        assert!(read_magic(&mut b"no magic here".as_slice()).is_err());
    }

    #[test]
    fn serve_answers_in_process_and_limits_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "季度 quarterly plan").unwrap();
        let mut input = Vec::new();
        let path = p.to_str().unwrap().to_string();
        write_request(&mut input, &Request { id: 1, kind: Kind::Echo, arg: 1000, path: path.clone() }).unwrap();
        // 4 bytes: "季" is 3 bytes, the next char would not fit.
        write_request(&mut input, &Request { id: 2, kind: Kind::Echo, arg: 4, path: path.clone() }).unwrap();
        write_request(&mut input, &Request { id: 3, kind: Kind::Sleep, arg: 1, path: path.clone() }).unwrap();
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output, HostOptions { test_requests: true }).unwrap();
        let mut r = output.as_slice();
        assert_eq!(read_reply(&mut r).unwrap(), Some((1, Reply::Text("季度 quarterly plan".into()))));
        assert_eq!(read_reply(&mut r).unwrap(), Some((2, Reply::Text("季".into()))));
        assert_eq!(read_reply(&mut r).unwrap(), Some((3, Reply::NoText)));
        // Production hosts refuse the test requests.
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output, HostOptions::default()).unwrap();
        assert!(matches!(read_reply(&mut output.as_slice()).unwrap(), Some((1, Reply::Error(_)))));
        let mut s = String::new();
        assert!(!push_limited(&mut s, "abcdef", 3));
        assert_eq!(s, "abc");
    }

    #[test]
    fn client_kills_a_hung_host_and_respawns() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "hello from the host").unwrap();
        let c = test_client(ClientOptions { timeout: Duration::from_secs(5), hosts: 1, ..ClientOptions::default() });
        assert_eq!(c.request(Kind::Echo, 100, &p).unwrap(), Reply::Text("hello from the host".into()));
        let first = c.host_pids();
        assert_eq!(first.len(), 1);
        // A hung filter: the request times out, the host is killed.
        let c2 =
            test_client(ClientOptions { timeout: Duration::from_millis(800), hosts: 1, ..ClientOptions::default() });
        assert_eq!(c2.request(Kind::Echo, 100, &p).unwrap(), Reply::Text("hello from the host".into()));
        let hung = dir.path().join("hung.doc");
        std::fs::write(&hung, "x").unwrap();
        let t = Instant::now();
        assert!(matches!(c2.request(Kind::Sleep, 60_000, &hung), Err(ClientError::Timeout)));
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(700) && waited < Duration::from_secs(5), "{waited:?}");
        assert!(c2.host_pids().is_empty(), "the hung host is gone");
        // The poisoned file is skipped; other files get a fresh host.
        assert!(matches!(c2.request(Kind::Sleep, 1, &hung), Err(ClientError::Poisoned)));
        assert_eq!(c2.request(Kind::Echo, 5, &p).unwrap(), Reply::Text("hello".into()));
        let s = c2.stats();
        assert_eq!((s.spawned, s.timeouts, s.crashes), (2, 1, 0), "{s:?}");
    }

    #[test]
    fn client_survives_a_crashing_host_and_recycles() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "abc").unwrap();
        let bad = dir.path().join("crash.doc");
        std::fs::write(&bad, "x").unwrap();
        let c = test_client(ClientOptions {
            timeout: Duration::from_secs(10),
            hosts: 1,
            recycle_after: 2,
            ..ClientOptions::default()
        });
        assert!(matches!(c.request(Kind::Crash, 0, &bad), Err(ClientError::Crashed)));
        for _ in 0..3 {
            assert_eq!(c.request(Kind::Echo, 10, &p).unwrap(), Reply::Text("abc".into()));
        }
        let s = c.stats();
        assert_eq!((s.crashes, s.recycled), (1, 1), "{s:?}");
        assert_eq!(s.spawned, 3, "crash → respawn, recycle after 2 requests → respawn");
        // A client whose host cannot start reports it and backs off.
        let none = IFilterClient::new(
            HostCommand { program: dir.path().join("missing.exe"), args: Vec::new(), envs: Vec::new() },
            ClientOptions::default(),
        );
        assert!(matches!(none.request(Kind::Echo, 1, &p), Err(ClientError::Spawn(_))));
        assert!(matches!(none.request(Kind::Echo, 1, &p), Err(ClientError::Spawn(m)) if m.contains("backing off")));
    }

    #[test]
    fn registrations() {
        let c = test_client(ClientOptions::default());
        assert!(!c.has_filter(""));
        if cfg!(windows) {
            // Windows ships filters for plain text and HTML; executables have the null filter.
            assert!(c.has_filter("txt") && c.has_filter(".HTM"));
            assert!(!c.has_filter("exe"));
            assert!(!c.has_filter("no-such-extension-x"));
        } else {
            assert!(!c.has_filter("txt"));
            assert!(filter_text(Path::new("/etc/hostname"), 10).is_err());
        }
    }

    /// RTF through the system's RTF IFilter: text only the filter can produce (the
    /// built-in parsers read the raw markup).
    #[cfg(windows)]
    #[test]
    fn rtf_text_through_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("memo.rtf");
        std::fs::write(
            &p,
            br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0\pard Quarterly {\b zebra} plan\par Second line\par}",
        )
        .unwrap();
        if !test_client(ClientOptions::default()).has_filter("rtf") {
            eprintln!("no RTF IFilter registered; skipped");
            return;
        }
        let direct = filter_text(&p, 1 << 20).unwrap().unwrap_or_default();
        assert!(direct.contains("Quarterly zebra plan\nSecond line"), "{direct:?}");
        assert!(!direct.contains("rtf1"), "{direct:?}");
        let c = test_client(ClientOptions::default());
        let reply = c.request(Kind::Extract, 1 << 20, &p).unwrap();
        assert!(matches!(&reply, Reply::Text(t) if t.contains("zebra")), "{reply:?}");
        let t = c.extract(&p, 1 << 20).unwrap().unwrap();
        assert!(t.contains("Quarterly zebra plan") && t.contains("Second line"), "{t:?}");
        assert_eq!(c.extract(&p, 9).unwrap().as_deref(), Some("Quarterly"));
    }
}

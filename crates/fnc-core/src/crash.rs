//! Crash reports (REL-02): a process-wide panic hook that writes
//! `logs/crash-<YYYYMMDD-HHMMSS>[-<binary>].log`, and the "did not exit normally"
//! detection of the next start.
//!
//! * [`install`] — called once by every binary (`fennec`, `fes`, `fennec-service`, the
//!   IFilter host) with its name and logs folder. The hook writes the report, then chains
//!   to the previous hook (the default one prints to stderr as before).
//! * Report content: product version, binary, OS ([`install`]'s `os` from
//!   `fnc_platform::os::version`), local time, pid, thread name, panic message and
//!   location, a forced backtrace, the last [`MAX_QUERIES`] searches ([`record_query`];
//!   only length + hash with [`set_redact_queries`]) and an index-state summary from the
//!   provider the app registers ([`set_state_provider`]).
//! * Panic safety: the hook takes no blocking lock (every shared value is behind a
//!   `Mutex` read with `try_lock`; a lock held by the panicking thread shows as
//!   "unavailable"), never unwraps, and writes at most [`MAX_REPORTS`] files per process.
//! * Caught panics: the hook runs before unwinding, so it cannot know whether a
//!   `catch_unwind` (search engine, property parsers, IPC handlers) will contain the
//!   panic. Every panic gets a report; a process that later ends normally calls
//!   [`mark_survived`], which renames its reports to `….caught.log`. Only reports left
//!   with the plain name (the process died: panic out of `main`, killed, crashed) count
//!   as [`pending`] and trigger the next-start prompt, until [`acknowledge`]d.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

/// Recent searches kept for the report.
pub const MAX_QUERIES: usize = 20;
/// Longest search text kept (characters); longer ones are cut.
pub const MAX_QUERY_CHARS: usize = 1000;
/// Most reports one process writes (a panic repeated by a worker loop must not flood
/// the logs folder).
pub const MAX_REPORTS: usize = 10;
/// Environment variable through which `fennec` hands its logs folder to the IFilter
/// host processes it starts.
pub const ENV_CRASH_DIR: &str = "FENNEC_CRASH_DIR";
/// File in the logs folder naming the newest acknowledged report.
pub const ACK_FILE: &str = "crash.ack";
/// Suffix of reports of processes that ended normally after the panic.
pub const CAUGHT_SUFFIX: &str = ".caught.log";

/// Index-state summary callback (must not block or panic: use `try_*` locks).
pub type StateProvider = Arc<dyn Fn() -> String + Send + Sync>;

struct Target {
    binary: &'static str,
    dir: PathBuf,
    os: String,
}

static TARGET: Mutex<Option<Target>> = Mutex::new(None);
static QUERIES: Mutex<QueryRing> = Mutex::new(QueryRing::new());
static PROVIDER: Mutex<Option<StateProvider>> = Mutex::new(None);
static WRITTEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static REDACT: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static REPORTS: AtomicUsize = AtomicUsize::new(0);

/// Locks for normal (non-hook) code: a poisoned lock is still usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Non-blocking lock for the hook (`None` if another thread — or the panicking one —
/// holds it).
fn try_lock<T>(m: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// Installs the panic hook of this process (`binary`: `fennec`, `fes`,
/// `fennec-service`, `ifilter-host`; reports go to `dir`, created on demand, or to the
/// temporary folder if `dir` is not writable; `os` is the OS description). Calling it again only updates binary, folder and OS text.
pub fn install(binary: &'static str, dir: PathBuf, os: String) {
    *lock(&TARGET) = Some(Target { binary, dir, os });
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic(info);
        previous(info);
    }));
}

/// Changes the folder reports are written to (e.g. once `fes` knows its `-instance`).
pub fn set_dir(dir: PathBuf) {
    if let Some(t) = lock(&TARGET).as_mut() {
        t.dir = dir;
    }
}

/// The folder reports are written to (`None` before [`install`]).
pub fn dir() -> Option<PathBuf> {
    lock(&TARGET).as_ref().map(|t| t.dir.clone())
}

/// Setting `crash_redact_queries`: reports list only the length and a hash of searches.
pub fn set_redact_queries(on: bool) {
    REDACT.store(on, Ordering::Relaxed);
}

/// Remembers a search for the report (the last [`MAX_QUERIES`]; repeats of the newest
/// one are skipped).
pub fn record_query(text: &str) {
    lock(&QUERIES).push(text);
}

/// Registers the index-state summary of the report.
pub fn set_state_provider(f: StateProvider) {
    *lock(&PROVIDER) = Some(f);
}

/// The process is ending normally: renames the reports it wrote to
/// `….caught.log` (the panics were contained), so they do not count as [`pending`].
pub fn mark_survived() {
    let files = std::mem::take(&mut *lock(&WRITTEN));
    for f in files {
        let caught = caught_name(&f);
        if std::fs::rename(&f, &caught).is_err() {
            // Keep it pending rather than lose it.
            continue;
        }
    }
}

fn caught_name(f: &Path) -> PathBuf {
    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = name.strip_suffix(".log").unwrap_or(&name);
    f.with_file_name(format!("{stem}{CAUGHT_SUFFIX}"))
}

/// Ring buffer of the recent searches.
#[derive(Debug, Default)]
pub struct QueryRing {
    items: VecDeque<String>,
}

impl QueryRing {
    /// An empty ring.
    pub const fn new() -> Self {
        Self { items: VecDeque::new() }
    }

    /// Adds `text` (cut at [`MAX_QUERY_CHARS`]; empty texts are skipped); drops the
    /// oldest beyond [`MAX_QUERIES`]. Search-as-you-type sends every keystroke, so a text
    /// that extends or shortens the newest entry replaces it instead of adding one.
    pub fn push(&mut self, text: &str) {
        let text = match text.char_indices().nth(MAX_QUERY_CHARS) {
            Some((cut, _)) => &text[..cut],
            None => text,
        };
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.items.back_mut()
            && (text.starts_with(last.as_str()) || last.starts_with(text))
        {
            if last != text {
                text.clone_into(last);
            }
            return;
        }
        if self.items.len() == MAX_QUERIES {
            self.items.pop_front();
        }
        self.items.push_back(text.to_string());
    }

    /// Oldest first.
    pub fn items(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }
}

/// What a redacted report shows of a search: its length and the first 8 bytes of its
/// SHA-256 (equal searches stay recognisable).
pub fn redact(text: &str) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in hash.iter().take(8) {
        let _ = write!(hex, "{b:02x}");
    }
    format!("<redacted: {} chars, sha256 {hex}>", text.chars().count())
}

/// Everything a report shows.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// `panic` or `exception 0x…` (Windows SEH).
    pub kind: String,
    /// Binary name.
    pub binary: String,
    /// OS description.
    pub os: String,
    /// Local time, human readable.
    pub time: String,
    /// Process id.
    pub pid: u32,
    /// Thread name (or id).
    pub thread: String,
    /// Panic message.
    pub message: String,
    /// `file:line:column`.
    pub location: String,
    /// Recent searches, oldest first (`None`: unavailable).
    pub queries: Option<Vec<String>>,
    /// Show searches as [`redact`]ed.
    pub redact: bool,
    /// Index-state summary (`None`: no provider or unavailable).
    pub index_state: Option<String>,
    /// Backtrace text.
    pub backtrace: String,
}

impl Report {
    /// The report file text.
    pub fn format(&self) -> String {
        let mut s = String::with_capacity(4096 + self.backtrace.len());
        let _ = writeln!(s, "Fennec crash report");
        let _ = writeln!(s, "product:  Fennec {}", env!("CARGO_PKG_VERSION"));
        let _ = writeln!(s, "binary:   {}", self.binary);
        let _ = writeln!(s, "os:       {}", self.os);
        let _ = writeln!(s, "time:     {}", self.time);
        let _ = writeln!(s, "pid:      {}", self.pid);
        let _ = writeln!(s, "thread:   {}", self.thread);
        let _ = writeln!(s, "kind:     {}", self.kind);
        let _ = writeln!(s, "message:  {}", self.message.replace('\n', "\n          "));
        let _ = writeln!(s, "location: {}", self.location);
        let _ = writeln!(
            s,
            "note:     written when the panic happened; renamed to *{CAUGHT_SUFFIX} if the process went on and \
             ended normally"
        );
        let _ = writeln!(s, "\n[recent searches] (oldest first)");
        match &self.queries {
            None => {
                let _ = writeln!(s, "  (unavailable)");
            }
            Some(q) if q.is_empty() => {
                let _ = writeln!(s, "  (none)");
            }
            Some(q) => {
                for (i, text) in q.iter().enumerate() {
                    let shown = if self.redact { redact(text) } else { text.replace(['\r', '\n'], " ") };
                    let _ = writeln!(s, "  {:2}. {shown}", i + 1);
                }
            }
        }
        let _ = writeln!(s, "\n[index state]");
        let _ = writeln!(s, "  {}", self.index_state.as_deref().unwrap_or("(unavailable)").replace('\n', "\n  "));
        let _ = writeln!(s, "\n[backtrace]\n{}", self.backtrace);
        s
    }
}

/// `crash-<YYYYMMDD-HHMMSS>[-<binary>][-<n>].log` (no binary suffix for `fennec`;
/// `n` > 1 for further reports in the same second).
pub fn file_name(stamp: &str, binary: &str, n: u32) -> String {
    let mut name = format!("crash-{stamp}");
    if !binary.is_empty() && binary != "fennec" {
        name.push('-');
        name.push_str(binary);
    }
    if n > 1 {
        let _ = write!(name, "-{n}");
    }
    name.push_str(".log");
    name
}

/// `true` for report file names (`crash-YYYYMMDD-HHMMSS….log`).
pub fn is_report_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("crash-") else { return false };
    let b = rest.as_bytes();
    b.len() >= 19
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && rest.ends_with(".log")
}

/// Writes `text` as a new report in `dir` (created if missing); returns its path.
pub fn write_report(dir: &Path, binary: &str, stamp: &str, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 1..100 {
        let path = dir.join(file_name(stamp, binary, n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                f.flush()?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many crash reports in one second"))
}

/// Reports in `dir` newer than the acknowledged one ([`ACK_FILE`]) and not marked
/// caught, oldest first.
pub fn pending(dir: &Path) -> Vec<PathBuf> {
    let acked = std::fs::read_to_string(dir.join(ACK_FILE)).map(|s| s.trim().to_string()).unwrap_or_default();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_report_name(n) && !n.ends_with(CAUGHT_SUFFIX) && n.as_str() > acked.as_str())
        .collect();
    names.sort();
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// Marks every report up to the newest of `files` as seen: [`pending`] no longer lists
/// them.
pub fn acknowledge(dir: &Path, files: &[PathBuf]) -> std::io::Result<()> {
    let Some(newest) = files.iter().filter_map(|f| f.file_name()?.to_str()).max() else { return Ok(()) };
    std::fs::write(dir.join(ACK_FILE), newest)
}

fn thread_label() -> String {
    let t = std::thread::current();
    match t.name() {
        Some(n) => n.to_string(),
        None => format!("<unnamed> {:?}", t.id()),
    }
}

fn now() -> (String, String) {
    let t = chrono::Local::now();
    (t.format("%Y%m%d-%H%M%S").to_string(), t.format("%Y-%m-%d %H:%M:%S%.3f %:z").to_string())
}

/// Collects the shared state (non-blocking) into a report.
fn gather(kind: String, message: String, location: String, backtrace: String) -> Option<(Report, PathBuf)> {
    let (binary, dir, os) = {
        let t = try_lock(&TARGET)?;
        let t = t.as_ref()?;
        (t.binary.to_string(), t.dir.clone(), t.os.clone())
    };
    let queries = try_lock(&QUERIES).map(|q| q.items().map(str::to_string).collect());
    let provider = try_lock(&PROVIDER).and_then(|p| p.clone());
    let index_state = provider.map(|p| p());
    let report = Report {
        kind,
        binary,
        os,
        time: String::new(),
        pid: std::process::id(),
        thread: thread_label(),
        message,
        location,
        queries,
        redact: REDACT.load(Ordering::Relaxed),
        index_state,
        backtrace,
    };
    Some((report, dir))
}

fn emit(mut report: Report, dir: &Path) -> Option<PathBuf> {
    if REPORTS.fetch_add(1, Ordering::SeqCst) >= MAX_REPORTS {
        return None;
    }
    let (stamp, time) = now();
    report.time = time;
    let text = report.format();
    // A read-only data folder (portable copy on a CD, locked-down profile): %TEMP%.
    let path = write_report(dir, &report.binary, &stamp, &text)
        .or_else(|_| write_report(&std::env::temp_dir(), &report.binary, &stamp, &text))
        .ok()?;
    if let Some(mut w) = try_lock(&WRITTEN) {
        w.push(path.clone());
    }
    Some(path)
}

fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(non-string panic payload)".into());
    let location = info.location().map_or_else(String::new, |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    if let Some((report, dir)) = gather("panic".into(), message, location, backtrace) {
        let _ = emit(report, &dir);
    }
}

/// Writes a minimal report for a fatal non-panic crash (Windows unhandled structured
/// exception `code` at `address`); used by `fnc_platform::os::install_exception_filter`.
pub fn write_exception_report(code: u32, address: usize) -> Option<PathBuf> {
    let (report, dir) = gather(
        format!("exception 0x{code:08X}"),
        format!("unhandled exception 0x{code:08X} at 0x{address:X}"),
        String::new(),
        "(not captured for structured exceptions)".into(),
    )?;
    emit(report, &dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_last_twenty_without_repeats() {
        let mut r = QueryRing::new();
        for i in 0..25 {
            r.push(&format!("{i}-q"));
            r.push(&format!("{i}-q"));
            r.push("");
        }
        let v: Vec<&str> = r.items().collect();
        assert_eq!(v.len(), MAX_QUERIES);
        assert_eq!(v[0], "5-q");
        assert_eq!(v[19], "24-q");
        // Typing and backspacing collapse into the newest entry.
        for t in ["b", "bu", "bud", "budget", "budg", "budge"] {
            r.push(t);
        }
        assert_eq!(r.items().last(), Some("budge"));
        assert_eq!(r.items().nth(18), Some("24-q"));
        let long = "é".repeat(MAX_QUERY_CHARS + 50);
        r.push(&long);
        assert_eq!(r.items().last().map(|s| s.chars().count()), Some(MAX_QUERY_CHARS));
    }

    #[test]
    fn redaction_shows_length_and_hash_only() {
        let a = redact("secret 季度");
        assert!(a.starts_with("<redacted: 9 chars, sha256 "), "{a}");
        assert!(!a.contains("secret"));
        assert_eq!(a, redact("secret 季度"));
        assert_ne!(a, redact("secret 季"));
    }

    fn sample() -> Report {
        Report {
            kind: "panic".into(),
            binary: "fes".into(),
            os: "Windows Server 2022 (build 20348.1)".into(),
            time: "2026-10-14 10:15:00.000 +08:00".into(),
            pid: 42,
            thread: "main".into(),
            message: "boom".into(),
            location: "src/main.rs:3:5".into(),
            queries: Some(vec!["ext:pdf budget".into(), "*.rs".into()]),
            redact: false,
            index_state: Some("entries: 10\nvolumes: 1".into()),
            backtrace: "   0: fes::main".into(),
        }
    }

    #[test]
    fn report_lists_every_section() {
        let text = sample().format();
        for want in [
            &format!("product:  Fennec {}", env!("CARGO_PKG_VERSION")) as &str,
            "binary:   fes",
            "os:       Windows Server 2022",
            "thread:   main",
            "message:  boom",
            "location: src/main.rs:3:5",
            "   1. ext:pdf budget",
            "   2. *.rs",
            "[index state]\n  entries: 10\n  volumes: 1",
            "[backtrace]\n   0: fes::main",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        let redacted = Report { redact: true, ..sample() }.format();
        assert!(!redacted.contains("budget") && redacted.contains("<redacted: 14 chars"), "{redacted}");
        let empty = Report { queries: None, index_state: None, ..sample() }.format();
        assert!(empty.contains("[recent searches] (oldest first)\n  (unavailable)"), "{empty}");
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name("20261014-101500", "fennec", 1), "crash-20261014-101500.log");
        assert_eq!(file_name("20261014-101500", "fes", 1), "crash-20261014-101500-fes.log");
        assert_eq!(file_name("20261014-101500", "ifilter-host", 3), "crash-20261014-101500-ifilter-host-3.log");
        assert!(is_report_name("crash-20261014-101500.log"));
        assert!(is_report_name("crash-20261014-101500-fes.caught.log"));
        assert!(!is_report_name("crash.ack"));
        assert!(!is_report_name("crash-2026-10-14.log"));
        assert!(!is_report_name("fennec.2026-10-14.log"));
        assert_eq!(
            caught_name(Path::new("/l/crash-20261014-101500-fes.log")),
            Path::new("/l/crash-20261014-101500-fes.caught.log")
        );
    }

    #[test]
    fn same_second_reports_get_numbered() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_report(&dir.path().join("logs"), "fennec", "20261014-101500", "a").unwrap();
        let b = write_report(&dir.path().join("logs"), "fennec", "20261014-101500", "b").unwrap();
        assert!(a.ends_with("crash-20261014-101500.log"));
        assert!(b.ends_with("crash-20261014-101500-2.log"));
        assert_eq!(std::fs::read_to_string(b).unwrap(), "b");
    }

    #[test]
    fn pending_until_acknowledged_and_caught_reports_never_count() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert!(pending(d).is_empty());
        for n in ["crash-20261014-101500.log", "crash-20261014-101400-fes.caught.log", "fennec.2026-10-14.log"] {
            std::fs::write(d.join(n), "x").unwrap();
        }
        let p = pending(d);
        assert_eq!(p, [d.join("crash-20261014-101500.log")]);
        acknowledge(d, &p).unwrap();
        assert!(pending(d).is_empty(), "no prompt after acknowledging");
        // Older files stay acknowledged; a newer report of any binary is pending again.
        std::fs::write(d.join("crash-20261013-090000-fes.log"), "x").unwrap();
        assert!(pending(d).is_empty());
        std::fs::write(d.join("crash-20261015-090000-ifilter-host.log"), "x").unwrap();
        std::fs::write(d.join("crash-20261015-080000-fennec-service.log"), "x").unwrap();
        assert_eq!(
            pending(d),
            [d.join("crash-20261015-080000-fennec-service.log"), d.join("crash-20261015-090000-ifilter-host.log")]
        );
        acknowledge(d, &[]).unwrap();
        assert_eq!(pending(d).len(), 2);
    }

    /// The installed hook writes a report (with the recorded searches and the state
    /// summary) for a caught panic, chains to the previous hook, and `mark_survived`
    /// renames it so it is not pending.
    #[test]
    fn hook_writes_report_and_survivors_are_marked_caught() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        install("fennec", logs.clone(), "TestOS 1.0".into());
        assert_eq!(super::dir(), Some(logs.clone()));
        record_query("first search");
        record_query("ext:txt second");
        set_state_provider(Arc::new(|| "entries: 7".into()));
        let r = std::thread::Builder::new()
            .name("crash-test".into())
            .spawn(|| std::panic::catch_unwind(|| panic!("deliberate test panic")).is_err())
            .unwrap()
            .join()
            .unwrap();
        assert!(r);
        let files = pending(&logs);
        assert_eq!(files.len(), 1, "{files:?}");
        let text = std::fs::read_to_string(&files[0]).unwrap();
        for want in [
            "binary:   fennec",
            "os:       TestOS 1.0",
            "thread:   crash-test",
            "message:  deliberate test panic",
            "crash.rs:",
            "ext:txt second",
            "entries: 7",
            "[backtrace]",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        mark_survived();
        assert!(pending(&logs).is_empty());
        let names: Vec<String> =
            std::fs::read_dir(&logs).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| n.ends_with(CAUGHT_SUFFIX)), "{names:?}");
    }
}

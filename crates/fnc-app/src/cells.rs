//! Result cells that read files: property columns of properties the property index does
//! not cover, and the link target, file signature and recycle bin original location
//! columns (SPEC ch.3 §4.3).
//!
//! The UI thread never touches the disk: [`LazyCells::get`] returns the cached value or
//! queues the entry for a background worker and returns `None` (the cell stays empty);
//! when values arrive [`LazyCells::generation`] changes and the window repaints. Only rows
//! that are drawn are requested, newest requests first; a stale backlog is dropped when
//! the user scrolls on.
//!
//! The three file columns also keep their values in the persistent
//! [`FileValueCache`] (`files.fnx`), which the search engine sorts by: before such a sort
//! the app counts the results without a cached value ([`LazyCells::missing`]), asks, and
//! reads them on a background thread ([`LazyCells::read_files`], REL-06).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fnc_core::EntryId;
use fnc_index::{IndexStore, SharedIndex};
use fnc_props::filecache::{FileField, FileValueCache, file_key};
use fnc_props::ondemand::OnDemand;
use fnc_props::{PropertyId, Value};
use parking_lot::{Condvar, Mutex};

/// A cell that reads the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LazyCol {
    /// A property value.
    Prop(PropertyId),
    /// Target of a shortcut, internet shortcut, symbolic link or junction.
    LinkTarget,
    /// File signature (`fsi:` name).
    Signature,
    /// Original location of a recycle bin item.
    RecycleOrigin,
}

impl LazyCol {
    /// The persistent field of a file column (`None` for properties).
    pub fn field(self) -> Option<FileField> {
        match self {
            LazyCol::Prop(_) => None,
            LazyCol::LinkTarget => Some(FileField::LinkTarget),
            LazyCol::Signature => Some(FileField::Signature),
            LazyCol::RecycleOrigin => Some(FileField::RecycleOrigin),
        }
    }

    /// The cell of a file field.
    pub fn of_field(f: FileField) -> LazyCol {
        match f {
            FileField::LinkTarget => LazyCol::LinkTarget,
            FileField::Signature => LazyCol::Signature,
            FileField::RecycleOrigin => LazyCol::RecycleOrigin,
        }
    }
}

/// A loaded cell.
#[derive(Clone, Debug, PartialEq)]
pub enum LazyVal {
    /// Property value.
    Prop(Value),
    /// Text.
    Text(String),
}

/// Identity of one version of an entry: index instance, id, date modified and size (a
/// changed file is read again).
type Key = (u64, u32, i64, u64);

/// Cached entries before the cache starts over (the visible rows are requested again).
const MAX_CACHED: usize = 20_000;
/// Queued entries; older requests (rows scrolled past) are dropped.
const MAX_QUEUED: usize = 256;
/// Threads reading files for a sort ([`LazyCells::read_files`]): several reads in flight
/// hide disk and network latency without saturating the machine.
pub const READ_THREADS: usize = 4;
/// Entries prepared per index snapshot while reading for a sort.
const READ_CHUNK: usize = 1024;
/// Time to read one file for the estimate before anything was measured: a shortcut,
/// signature or `$I` record is a few hundred bytes, about 1 ms with the open/close on a
/// local disk.
pub const DEFAULT_READ_COST: Duration = Duration::from_millis(1);
/// Measured reads needed before the running average replaces [`DEFAULT_READ_COST`].
const MIN_SAMPLES: u64 = 8;

struct Job {
    key: Key,
    path: PathBuf,
    ext: String,
    cols: Vec<LazyCol>,
    /// `$R…` path and the path below it (recycle bin original location).
    recycle: Option<(PathBuf, String)>,
    /// Persistent cache key (file columns).
    file: Option<fnc_props::hash::HashKey>,
}

#[derive(Default)]
struct State {
    values: HashMap<(Key, LazyCol), Option<LazyVal>>,
    pending: HashSet<(Key, LazyCol)>,
    queue: VecDeque<Job>,
    stop: bool,
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    generation: AtomicU64,
    repaint: Mutex<Option<egui::Context>>,
    source: Box<dyn Fn() -> Arc<dyn OnDemand> + Send + Sync>,
    files: Arc<FileValueCache>,
    /// Measured file reads: total nanoseconds and count (estimates).
    read_ns: AtomicU64,
    read_n: AtomicU64,
}

impl Inner {
    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(ctx) = &*self.repaint.lock() {
            ctx.request_repaint();
        }
    }

    fn measure(&self, t: Instant) {
        self.read_ns.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.read_n.fetch_add(1, Ordering::Relaxed);
    }
}

/// Background loader and cache of file-reading cells (shared by all windows).
pub struct LazyCells {
    inner: Arc<Inner>,
}

impl LazyCells {
    /// Starts the worker; `source` gives the current on-demand extractors, `files` is the
    /// persistent cache of the file columns.
    pub fn new(source: impl Fn() -> Arc<dyn OnDemand> + Send + Sync + 'static, files: Arc<FileValueCache>) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            repaint: Mutex::new(None),
            source: Box::new(source),
            files,
            read_ns: AtomicU64::new(0),
            read_n: AtomicU64::new(0),
        });
        let w = inner.clone();
        if let Err(e) = std::thread::Builder::new().name("fennec-cells".into()).spawn(move || work(&w)) {
            tracing::warn!(error = %e, "cell loader not started");
        }
        Self { inner }
    }

    /// Repaints `ctx` when values arrive.
    pub fn set_repaint(&self, ctx: egui::Context) {
        *self.inner.repaint.lock() = Some(ctx);
    }

    /// Changes whenever values arrive (part of the row cache key).
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// The persistent cache of the file columns (search options: file column sorts).
    pub fn file_values(&self) -> Arc<FileValueCache> {
        self.inner.files.clone()
    }

    /// Whether `col` can have a value for `id` without reading the file (`false`: the cell
    /// is empty for good, e.g. a signature of a folder or a link target of a plain file).
    pub fn applies(s: &IndexStore, id: EntryId, col: LazyCol) -> bool {
        match col.field() {
            Some(f) => fnc_exec::files::reads_file(s, id, f),
            None => {
                // Reading a cloud placeholder would download it.
                let a = s.attributes(id);
                use fnc_core::flags::attr;
                a & (attr::OFFLINE | attr::RECALL_ON_OPEN | attr::RECALL_ON_DATA_ACCESS) == 0 && !s.is_dir(id)
            }
        }
    }

    /// The value of `col` for `id` if loaded; otherwise queues it and returns `None`.
    pub fn get(&self, s: &IndexStore, id: EntryId, col: LazyCol) -> Option<LazyVal> {
        if !Self::applies(s, id, col) {
            return None;
        }
        let file = col.field().and_then(|f| {
            let k = file_key(s, id, &mut Vec::new())?;
            // A busy lock (a sort or a bulk read): the worker looks again.
            match self.inner.files.try_get(&k, f, Duration::from_millis(1)) {
                Some(Some(v)) => Some(Err(v)),
                _ => Some(Ok(k)),
            }
        });
        let file = match file {
            Some(Err(v)) => return (!v.is_empty()).then(|| LazyVal::Text(v.to_string())),
            Some(Ok(k)) => Some(k),
            None => None,
        };
        let key = key_of(s, id);
        let mut st = self.inner.state.lock();
        if let Some(v) = st.values.get(&(key, col)) {
            return v.clone();
        }
        if !st.pending.insert((key, col)) {
            return None;
        }
        // Join the queued job of the same entry (one file open for all its columns).
        if let Some(j) = st.queue.iter_mut().find(|j| j.key == key) {
            j.cols.push(col);
            if col == LazyCol::RecycleOrigin && j.recycle.is_none() {
                j.recycle = recycle_of(s, id, &j.path);
            }
            j.file = j.file.or(file);
            return None;
        }
        let mut job = job_of(s, id, vec![col]);
        job.file = file;
        st.queue.push_front(job);
        while st.queue.len() > MAX_QUEUED {
            if let Some(old) = st.queue.pop_back() {
                for c in old.cols {
                    st.pending.remove(&(old.key, c));
                }
            }
        }
        drop(st);
        self.inner.wake.notify_one();
        None
    }

    /// Forgets every value (the extractors changed).
    pub fn clear(&self) {
        let mut st = self.inner.state.lock();
        st.values.clear();
        st.pending.clear();
        st.queue.clear();
        drop(st);
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Entries of `ids` that have no cached value for at least one of `fields` and can
    /// have one (cloud placeholders, folders without a signature, plain files without a
    /// link target are never read). Runs over the whole result: call it off the UI thread.
    pub fn missing(&self, s: &IndexStore, ids: &[EntryId], fields: &[FileField], cancel: &AtomicBool) -> Vec<EntryId> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        let mut keys = Vec::with_capacity(READ_CHUNK);
        for chunk in ids.chunks(READ_CHUNK) {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            keys.clear();
            for &id in chunk {
                let mask = fields
                    .iter()
                    .filter(|&&f| fnc_exec::files::reads_file(s, id, f))
                    .fold(0u8, |m, &f| m | (1 << field_bit(f)));
                if mask != 0
                    && let Some(k) = file_key(s, id, &mut buf)
                {
                    keys.push((id, k, mask));
                }
            }
            for &(id, k, mask) in &keys {
                // One entry at a time: the lock is never held for long.
                if self.inner.files.count_missing(&[(k, mask)]) > 0 {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Estimated wall time to read `n` files with [`LazyCells::read_files`]: the measured
    /// average read time (or [`DEFAULT_READ_COST`] before [`MIN_SAMPLES`] reads), spread
    /// over [`READ_THREADS`].
    pub fn estimate(&self, n: usize) -> Duration {
        let count = self.inner.read_n.load(Ordering::Relaxed);
        let per = if count >= MIN_SAMPLES {
            Duration::from_nanos(self.inner.read_ns.load(Ordering::Relaxed) / count)
        } else {
            DEFAULT_READ_COST
        };
        per.saturating_mul(n as u32) / READ_THREADS as u32
    }

    /// Reads `fields` of `ids` (entries of index instance `instance` in `shared`) into the
    /// persistent cache on [`READ_THREADS`] threads; `done` counts finished entries.
    /// Returns `false` if `cancel` was set or the index was renumbered meanwhile (the ids
    /// are stale). Call it off the UI thread.
    pub fn read_files(
        &self,
        shared: &SharedIndex,
        instance: u64,
        ids: &[EntryId],
        fields: &[FileField],
        cancel: &AtomicBool,
        done: &AtomicUsize,
    ) -> bool {
        let cols: Vec<LazyCol> = fields.iter().map(|&f| LazyCol::of_field(f)).collect();
        for chunk in ids.chunks(READ_CHUNK) {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            // Paths for one chunk under a short snapshot; reading runs without locks.
            let jobs: Vec<Job> = {
                let s = shared.snapshot();
                if s.instance() != instance {
                    return false;
                }
                let mut buf = Vec::new();
                chunk
                    .iter()
                    .filter(|&&id| s.is_live(id))
                    .filter_map(|&id| {
                        let cols: Vec<LazyCol> = cols.iter().copied().filter(|&c| Self::applies(&s, id, c)).collect();
                        let file = file_key(&s, id, &mut buf)?;
                        (!cols.is_empty()).then(|| Job { file: Some(file), ..job_of(&s, id, cols) })
                    })
                    .collect()
            };
            done.fetch_add(chunk.len() - jobs.len(), Ordering::Relaxed);
            let src = (self.inner.source)();
            let next = AtomicUsize::new(0);
            std::thread::scope(|sc| {
                for _ in 0..READ_THREADS.min(jobs.len()) {
                    sc.spawn(|| {
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                return;
                            }
                            let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else { return };
                            store(&self.inner, &*src, job);
                            done.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
            });
            self.inner.changed();
        }
        !cancel.load(Ordering::Relaxed)
    }

    /// Waits until the queue is empty (tests).
    #[cfg(test)]
    pub fn wait_idle(&self) {
        for _ in 0..500 {
            if self.inner.state.lock().pending.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

impl Drop for LazyCells {
    fn drop(&mut self) {
        // Not joined: an extraction in progress (an IFilter call) may take seconds.
        self.inner.state.lock().stop = true;
        self.inner.wake.notify_all();
    }
}

fn field_bit(f: FileField) -> u8 {
    match f {
        FileField::LinkTarget => 0,
        FileField::Signature => 1,
        FileField::RecycleOrigin => 2,
    }
}

fn key_of(s: &IndexStore, id: EntryId) -> Key {
    (s.instance(), id.0, s.date_modified(id).0, s.size(id).unwrap_or(u64::MAX))
}

/// The `$R…` item of `id` and the path of `id` below it (recycle bin original location).
fn recycle_of(s: &IndexStore, id: EntryId, path: &Path) -> Option<(PathBuf, String)> {
    let item = fnc_exec::files::recycle_item(s, id)?;
    let mut top = String::new();
    s.full_path(item, &mut top);
    let below = path.to_str().and_then(|p| p.get(top.len()..)).unwrap_or("").to_string();
    Some((PathBuf::from(top), below))
}

fn job_of(s: &IndexStore, id: EntryId, cols: Vec<LazyCol>) -> Job {
    let mut path = String::new();
    s.full_path(id, &mut path);
    let path = PathBuf::from(path);
    let recycle = cols.contains(&LazyCol::RecycleOrigin).then(|| recycle_of(s, id, &path)).flatten();
    let ext = String::from_utf8_lossy(fnc_index::ext::ext_of(s.fold(id))).into_owned();
    Job { key: key_of(s, id), path, ext, cols, recycle, file: None }
}

/// Reads the job's file columns that are not cached yet into the persistent cache.
fn store(inner: &Inner, src: &dyn OnDemand, job: &Job) {
    let Some(k) = job.file else { return };
    let todo: Vec<LazyCol> =
        job.cols.iter().copied().filter(|c| c.field().is_some_and(|f| !inner.files.contains(&k, f))).collect();
    if todo.is_empty() {
        return;
    }
    let t = Instant::now();
    let values = load(src, job, &todo);
    inner.measure(t);
    for (c, v) in values {
        if let Some(f) = c.field() {
            let text = match &v {
                Some(LazyVal::Text(t)) => t.as_str(),
                _ => "",
            };
            inner.files.insert(k, f, text);
        }
    }
}

fn work(inner: &Inner) {
    loop {
        let job = {
            let mut st = inner.state.lock();
            loop {
                if st.stop {
                    return;
                }
                if let Some(j) = st.queue.pop_front() {
                    break j;
                }
                inner.wake.wait(&mut st);
            }
        };
        let src = (inner.source)();
        // File columns cached meanwhile (or before the UI saw them) are not read again.
        let mut values = Vec::new();
        let mut read = Vec::new();
        for &c in &job.cols {
            match (c.field(), job.file) {
                (Some(f), Some(k)) => match inner.files.get(&k, f) {
                    Some(v) => values.push((c, (!v.is_empty()).then(|| LazyVal::Text(v.to_string())))),
                    None => read.push(c),
                },
                _ => read.push(c),
            }
        }
        if !read.is_empty() {
            let t = Instant::now();
            let loaded = load(&*src, &job, &read);
            if read.iter().any(|c| c.field().is_some()) {
                inner.measure(t);
            }
            for (c, v) in &loaded {
                if let (Some(f), Some(k)) = (c.field(), job.file) {
                    let text = match v {
                        Some(LazyVal::Text(t)) => t.as_str(),
                        _ => "",
                    };
                    inner.files.insert(k, f, text);
                }
            }
            values.extend(loaded);
        }
        finish(inner, job.key, values);
    }
}

fn finish(inner: &Inner, key: Key, values: Vec<(LazyCol, Option<LazyVal>)>) {
    let mut st = inner.state.lock();
    if st.values.len() + values.len() > MAX_CACHED {
        st.values.clear();
    }
    for (c, v) in values {
        // Dropped by `clear` meanwhile: the value may come from old extractors.
        if st.pending.remove(&(key, c)) {
            st.values.insert((key, c), v);
        }
    }
    drop(st);
    inner.changed();
}

/// Reads `cols` of the job's file (one property extraction for all its properties).
fn load(src: &dyn OnDemand, job: &Job, cols: &[LazyCol]) -> Vec<(LazyCol, Option<LazyVal>)> {
    let props: Vec<PropertyId> = cols
        .iter()
        .filter_map(|c| match c {
            LazyCol::Prop(p) => Some(*p),
            _ => None,
        })
        .collect();
    let mut found = if props.is_empty() { Vec::new() } else { src.properties(&job.path, &job.ext, &props) };
    cols.iter()
        .map(|&c| {
            let v = match c {
                LazyCol::Prop(p) => {
                    found.iter().position(|(q, _)| *q == p).map(|i| LazyVal::Prop(found.swap_remove(i).1))
                }
                LazyCol::LinkTarget => link_target(src, &job.path).map(LazyVal::Text),
                LazyCol::Signature => {
                    fnc_exec::files::file_signature(src, &job.path).filter(|s| !s.is_empty()).map(LazyVal::Text)
                }
                LazyCol::RecycleOrigin => job.recycle.as_ref().and_then(|(item, below)| {
                    let origin = fnc_exec::files::recycle_origin(src, item)?;
                    Some(LazyVal::Text(format!("{}{below}", origin.trim_end_matches(['\\', '/']))))
                }),
            };
            (c, v)
        })
        .collect()
}

fn link_target(src: &dyn OnDemand, path: &Path) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let kind = fnc_props::link::LinkKind::of_ext(&ext).unwrap_or(fnc_props::link::LinkKind::Reparse);
    src.link_target(path, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells() -> LazyCells {
        LazyCells::new(
            || Arc::new(fnc_props::ondemand::DefaultOnDemand::default()) as Arc<dyn OnDemand>,
            Arc::new(FileValueCache::new()),
        )
    }

    #[test]
    fn loads_in_the_background_and_reloads_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.dat");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
        std::fs::write(dir.path().join("x.txt"), b"plain").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let s = crate::stores::tests::index_of(dir.path());
        let find = |n: &str| crate::stores::tests::id_of(&s, n);
        let cells = cells();
        let (pic, txt, sub) = (find("pic.dat"), find("x.txt"), find("sub"));
        assert!(!LazyCells::applies(&s, sub, LazyCol::Signature));
        assert!(!LazyCells::applies(&s, txt, LazyCol::LinkTarget));
        assert!(!LazyCells::applies(&s, txt, LazyCol::RecycleOrigin));
        let g = cells.generation();
        assert_eq!(cells.get(&s, pic, LazyCol::Signature), None);
        assert_eq!(cells.get(&s, txt, LazyCol::Signature), None);
        cells.wait_idle();
        assert!(cells.generation() > g);
        assert_eq!(cells.get(&s, pic, LazyCol::Signature), Some(LazyVal::Text("png".into())));
        assert_eq!(cells.get(&s, txt, LazyCol::Signature), None);
        assert_eq!(cells.get(&s, sub, LazyCol::Signature), None);
        // Visible cells fill the persistent cache too ("" = read, nothing to show).
        let mut buf = Vec::new();
        let files = cells.file_values();
        assert_eq!(files.get(&file_key(&s, pic, &mut buf).unwrap(), FileField::Signature).as_deref(), Some("png"));
        assert_eq!(files.get(&file_key(&s, txt, &mut buf).unwrap(), FileField::Signature).as_deref(), Some(""));
        cells.clear();
        assert_eq!(cells.get(&s, pic, LazyCol::Signature), Some(LazyVal::Text("png".into())), "persistent");
    }

    /// Counts its reads; slow when asked to (cancellation).
    struct Counting {
        reads: AtomicUsize,
        delay: Duration,
    }

    impl OnDemand for Counting {
        fn properties(&self, _: &Path, _: &str, _: &[PropertyId]) -> Vec<(PropertyId, Value)> {
            Vec::new()
        }
        fn content(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
        fn text_as(&self, _: &Path, _: fnc_props::content::TextEncoding) -> Option<String> {
            None
        }
        fn bytes(&self, _: &Path) -> Option<Vec<u8>> {
            None
        }
        fn head(&self, path: &Path, max: usize) -> Option<Vec<u8>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(self.delay);
            let mut b = std::fs::read(path).ok()?;
            b.truncate(max);
            Some(b)
        }
    }

    #[test]
    fn bulk_reads_fill_the_cache_count_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            let data: &[u8] = if i % 2 == 0 { b"%PDF-1.4 x" } else { b"GIF89a....." };
            std::fs::write(dir.path().join(format!("f{i:02}.bin")), data).unwrap();
        }
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        let shared = SharedIndex::new(crate::stores::tests::index_of(dir.path()));
        let s = shared.snapshot();
        let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.parent(i).is_some()).collect();
        let src = Arc::new(Counting { reads: AtomicUsize::new(0), delay: Duration::ZERO });
        let s2 = src.clone();
        let cells = LazyCells::new(move || s2.clone() as Arc<dyn OnDemand>, Arc::new(FileValueCache::new()));
        let no = AtomicBool::new(false);
        let sig = [FileField::Signature];
        // The folder has no signature and is never read; link targets apply to nothing.
        assert_eq!(cells.missing(&s, &ids, &sig, &no).len(), 40);
        assert!(cells.missing(&s, &ids, &[FileField::LinkTarget], &no).is_empty());
        assert_eq!(cells.estimate(40), DEFAULT_READ_COST * 40 / READ_THREADS as u32, "nothing measured yet");
        let missing = cells.missing(&s, &ids, &sig, &no);
        let done = AtomicUsize::new(0);
        assert!(cells.read_files(&shared, s.instance(), &missing, &sig, &no, &done));
        assert_eq!(done.load(Ordering::Relaxed), 40);
        assert_eq!(src.reads.load(Ordering::Relaxed), 40);
        assert!(cells.missing(&s, &ids, &sig, &no).is_empty(), "all cached");
        assert!(cells.estimate(40) < Duration::from_secs(1), "measured");
        // Cached: nothing is read again.
        assert!(cells.read_files(&shared, s.instance(), &missing, &sig, &no, &done));
        assert_eq!(src.reads.load(Ordering::Relaxed), 40);
        // Stale ids (another instance) are not read.
        assert!(!cells.read_files(&shared, s.instance() + 1, &missing, &sig, &no, &done));
        let mut buf = Vec::new();
        let first = crate::stores::tests::id_of(&s, "f00.bin");
        let k = file_key(&s, first, &mut buf).unwrap();
        assert_eq!(cells.file_values().get(&k, FileField::Signature).as_deref(), Some("pdf"));

        // Cancelling stops a slow read early.
        let slow = Arc::new(Counting { reads: AtomicUsize::new(0), delay: Duration::from_millis(20) });
        let s3 = slow.clone();
        let cells = Arc::new(LazyCells::new(move || s3.clone() as Arc<dyn OnDemand>, Arc::new(FileValueCache::new())));
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let (c2, d2, cl2) = (cancel.clone(), done.clone(), cells.clone());
        let inst = s.instance();
        let t = std::thread::spawn(move || cl2.read_files(&shared, inst, &missing, &sig, &c2, &d2));
        while done.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(!t.join().unwrap(), "cancelled");
        assert!(slow.reads.load(Ordering::Relaxed) < 40, "stopped early");
    }
}

//! Result cells that read files: property columns of properties the property index does
//! not cover, and the link target, file signature and recycle bin original location
//! columns (SPEC ch.3 §4.3).
//!
//! The UI thread never touches the disk: [`LazyCells::get`] returns the cached value or
//! queues the entry for a background worker and returns `None` (the cell stays empty);
//! when values arrive [`LazyCells::generation`] changes and the window repaints. Only rows
//! that are drawn are requested, newest requests first; a stale backlog is dropped when
//! the user scrolls on.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fnc_core::EntryId;
use fnc_core::flags::attr;
use fnc_index::IndexStore;
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

struct Job {
    key: Key,
    path: PathBuf,
    ext: String,
    cols: Vec<LazyCol>,
    /// `$R…` path and the path below it (recycle bin original location).
    recycle: Option<(PathBuf, String)>,
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
}

/// Background loader and cache of file-reading cells (shared by all windows).
pub struct LazyCells {
    inner: Arc<Inner>,
}

impl LazyCells {
    /// Starts the worker; `source` gives the current on-demand extractors.
    pub fn new(source: impl Fn() -> Arc<dyn OnDemand> + Send + Sync + 'static) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            repaint: Mutex::new(None),
            source: Box::new(source),
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

    /// Whether `col` can have a value for `id` without reading the file (`false`: the cell
    /// is empty for good, e.g. a signature of a folder or a link target of a plain file).
    pub fn applies(s: &IndexStore, id: EntryId, col: LazyCol) -> bool {
        let a = s.attributes(id);
        // Reading a cloud placeholder would download it.
        if a & (attr::OFFLINE | attr::RECALL_ON_OPEN | attr::RECALL_ON_DATA_ACCESS) != 0
            && col != LazyCol::RecycleOrigin
        {
            return false;
        }
        match col {
            LazyCol::Prop(_) | LazyCol::Signature => !s.is_dir(id),
            LazyCol::LinkTarget => fnc_exec::files::link_kind(s, id).is_some(),
            LazyCol::RecycleOrigin => fnc_exec::files::recycle_item(s, id).is_some(),
        }
    }

    /// The value of `col` for `id` if loaded; otherwise queues it and returns `None`.
    pub fn get(&self, s: &IndexStore, id: EntryId, col: LazyCol) -> Option<LazyVal> {
        if !Self::applies(s, id, col) {
            return None;
        }
        let key = key_of(s, id);
        let mut st = self.inner.state.lock();
        if let Some(v) = st.values.get(&(key, col)) {
            return v.clone();
        }
        if !st.pending.insert((key, col)) {
            return None;
        }
        let mut path = String::new();
        s.full_path(id, &mut path);
        let recycle =
            (col == LazyCol::RecycleOrigin).then(|| fnc_exec::files::recycle_item(s, id)).flatten().map(|item| {
                let mut top = String::new();
                s.full_path(item, &mut top);
                let below = path.get(top.len()..).unwrap_or("").to_string();
                (PathBuf::from(top), below)
            });
        // Join the queued job of the same entry (one file open for all its columns).
        if let Some(j) = st.queue.iter_mut().find(|j| j.key == key) {
            j.cols.push(col);
            j.recycle = j.recycle.take().or(recycle);
            return None;
        }
        let ext = String::from_utf8_lossy(fnc_index::ext::ext_of(s.fold(id))).into_owned();
        st.queue.push_front(Job { key, path: PathBuf::from(path), ext, cols: vec![col], recycle });
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

fn key_of(s: &IndexStore, id: EntryId) -> Key {
    (s.instance(), id.0, s.date_modified(id).0, s.size(id).unwrap_or(u64::MAX))
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
        let values = load(&*src, &job);
        let mut st = inner.state.lock();
        if st.values.len() + values.len() > MAX_CACHED {
            st.values.clear();
        }
        for (c, v) in values {
            // Dropped by `clear` meanwhile: the value may come from old extractors.
            if st.pending.remove(&(job.key, c)) {
                st.values.insert((job.key, c), v);
            }
        }
        drop(st);
        inner.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(ctx) = &*inner.repaint.lock() {
            ctx.request_repaint();
        }
    }
}

/// Reads the job's columns (one property extraction for all its properties).
fn load(src: &dyn OnDemand, job: &Job) -> Vec<(LazyCol, Option<LazyVal>)> {
    let props: Vec<PropertyId> = job
        .cols
        .iter()
        .filter_map(|c| match c {
            LazyCol::Prop(p) => Some(*p),
            _ => None,
        })
        .collect();
    let mut found = if props.is_empty() { Vec::new() } else { src.properties(&job.path, &job.ext, &props) };
    job.cols
        .iter()
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

    #[test]
    fn loads_in_the_background_and_reloads_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("pic.dat");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
        std::fs::write(dir.path().join("x.txt"), b"plain").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let s = crate::stores::tests::index_of(dir.path());
        let find = |n: &str| crate::stores::tests::id_of(&s, n);
        let cells = LazyCells::new(|| Arc::new(fnc_props::ondemand::DefaultOnDemand::default()) as Arc<dyn OnDemand>);
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
        cells.clear();
        assert_eq!(cells.get(&s, pic, LazyCol::Signature), None);
        cells.wait_idle();
        assert_eq!(cells.get(&s, pic, LazyCol::Signature), Some(LazyVal::Text("png".into())));
    }
}

//! Property, content and hash stores of the application (SPEC ch.1 §8, §9, §11), wired
//! as described by the integration contract of `fnc_props`:
//!
//! * [`Stores`] owns the shared handles ([`PropertyIndex`], [`ContentIndex`],
//!   [`HashCache`], the on-demand source with the Windows Property System fallback) —
//!   created once, so every search reuses the same `Arc`s ([`Stores::apply_options`]).
//! * It implements [`IndexHooks`]: `props.fnx` / `content.fnx` are attached to the final
//!   index right before it is published (same id space, so they are searchable at once),
//!   follow compaction, and are saved with the index; `hashes.fnx` is loaded at start.
//! * A low-priority worker thread (background mode on Windows) extracts what is missing:
//!   a full scan once sizes and dates are complete and after configuration changes, and
//!   live changes debounced by 2 s (modified files are re-extracted, renamed ones only
//!   re-checked against the scope, deleted ones dropped). Extraction runs without any
//!   lock; results are inserted in short batches (lock order index → props → content),
//!   then the visible search is refreshed (at most once a second).
//!
//! With both indexes off (the default) the worker sleeps and live changes are not even
//! collected.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use fnc_core::settings::Settings;
use fnc_core::{EntryFlags, EntryId};
use fnc_exec::{SearchOptions, Shared, SlowPolicy};
use fnc_index::{Change, ChangeBatch, EntryRef, IndexStore, PathChange, SharedIndex};
use fnc_props::content::{ContentExtractor, ContentIndex, ContentStore};
use fnc_props::extract::{PropertyExtractor, PropertyProvider};
use fnc_props::hash::HashCache;
use fnc_props::ondemand::{DefaultOnDemand, OnDemand};
use fnc_props::{ContentIndexConfig, ContentLimits, PropertyIndex, PropertyIndexConfig, PropertyStore, Scope};
use parking_lot::Mutex;

use crate::indexer::IndexHooks;

/// Debounce of live changes (SPEC ch.1 §8.5).
const DEBOUNCE: Duration = Duration::from_secs(2);
/// Changes are processed at the latest this long after the first one.
const DEBOUNCE_MAX: Duration = Duration::from_secs(10);
/// Files extracted between two store insertions.
const BATCH: usize = 64;
/// Entries examined per index snapshot during a full scan.
const SCAN_CHUNK: usize = 32 * 1024;
/// Minimum time between two search refreshes while extracting.
const REFRESH_EVERY: Duration = Duration::from_secs(1);

/// What to index, derived from the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreConfig {
    /// Property index (empty = off).
    pub props: PropertyIndexConfig,
    /// Content index (`None` = off).
    pub content: Option<ContentIndexConfig>,
    /// Property names in the settings that are not known.
    pub unknown_props: Vec<String>,
    /// Slow-path policy for searches.
    pub slow: SlowPolicy,
    /// IFilter fallback for content (`content_ifilter`), with or without a content index.
    pub ifilter: bool,
}

impl StoreConfig {
    /// Builds the configuration from the settings.
    pub fn from_settings(s: &Settings) -> Self {
        let scope = Scope::parse(&s.property_folders.join(";"), &s.property_extensions.join(";"));
        let (props, unknown_props) =
            PropertyIndexConfig::from_names(&s.property_names.join(";"), scope, s.property_fast_sort);
        let content = s.content_enabled.then(|| ContentIndexConfig {
            scope: Scope::parse(&s.content_folders.join(";"), &s.content_extensions.join(";")),
            limits: ContentLimits {
                max_file_bytes: u64::from(s.content_max_file_mb.max(1)) << 20,
                office: s.content_office,
                pdf: s.content_pdf,
                ifilter: s.content_ifilter,
                ..ContentLimits::default()
            },
            max_total_bytes: u64::from(s.content_max_total_mb) << 20,
        });
        let slow = SlowPolicy {
            props: s.property_read_unindexed,
            content: s.content_read_unindexed,
            hashes: true,
            max_files: s.slow_search_max_files as usize,
            // Indexed properties and the content index are authoritative (IDX-08/09:
            // files outside their scope are not read); `notindexed:` still reads.
            read_uncovered: false,
        };
        Self { props, content, unknown_props, slow, ifilter: s.content_ifilter }
    }

    /// `true` if either index is on.
    pub fn active(&self) -> bool {
        !self.props.is_empty() || self.content.is_some()
    }

    fn content_limits(&self) -> ContentLimits {
        let mut l = self.content.as_ref().map(|c| c.limits.clone()).unwrap_or_default();
        l.ifilter = self.ifilter;
        l
    }
}

/// Extraction progress (status bar).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Files extracted in the current run.
    pub done: usize,
    /// Files queued in the current run.
    pub total: usize,
}

enum Msg {
    /// Stores attached; data keys of the attached rows.
    Attached(HashMap<u32, u64>),
    Compacted {
        old: u64,
        remap: Arc<[u32]>,
        new: u64,
    },
    Ready,
    Changed(Vec<EntryId>, Vec<EntryId>),
    Config,
}

/// The application's stores and their extraction worker.
pub struct Stores {
    /// Property index (`width:`, property columns).
    pub props: PropertyIndex,
    /// Content index (`content:`, preview text).
    pub content: ContentIndex,
    /// Hash cache (`sha256:`, `dupe:size;sha256`).
    pub hashes: Arc<HashCache>,
    on_demand: Mutex<Arc<dyn OnDemand>>,
    provider: Option<Arc<dyn PropertyProvider>>,
    ansi_code_page: u16,
    /// Folder of `props.fnx` / `content.fnx` / `hashes.fnx` (`None` = not persisted).
    dir: Option<PathBuf>,
    cfg: Mutex<Arc<StoreConfig>>,
    cfg_epoch: AtomicU64,
    active: AtomicBool,
    stopped: AtomicBool,
    done: AtomicUsize,
    total: AtomicUsize,
    /// `props.generation() ^ content.generation()` after the last batch (UI cache key).
    generation: AtomicU64,
    /// Generations written last (props, content, hashes).
    saved: Mutex<(u64, u64, u64)>,
    tx: Sender<Msg>,
}

impl std::fmt::Debug for Stores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stores").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Stores {
    /// Creates the stores for `settings`, loads `hashes.fnx` from `dir` and starts the
    /// worker. `refresh` re-runs the visible search.
    pub fn start(
        shared: SharedIndex,
        settings: &Settings,
        dir: Option<PathBuf>,
        refresh: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        let cfg = StoreConfig::from_settings(settings);
        for n in &cfg.unknown_props {
            tracing::warn!(property = %n, "unknown property name in property_names");
        }
        let provider = fnc_platform::props::provider();
        let ansi_code_page = fnc_platform::locale::ansi_code_page();
        let hashes = dir
            .as_ref()
            .map(|d| d.join("hashes.fnx"))
            .filter(|p| p.exists())
            .and_then(|p| match HashCache::load(&p) {
                Ok(h) => Some(h),
                Err(e) => {
                    tracing::warn!(error = %e, "hashes.fnx unusable; starting empty");
                    None
                }
            })
            .unwrap_or_default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let me = Arc::new(Self {
            props: PropertyIndex::new(PropertyStore::new(0)),
            content: ContentIndex::new(ContentStore::new(0)),
            hashes: Arc::new(hashes),
            on_demand: Mutex::new(on_demand(provider.clone(), &cfg, ansi_code_page)),
            provider,
            ansi_code_page,
            dir,
            active: AtomicBool::new(cfg.active()),
            cfg: Mutex::new(Arc::new(cfg)),
            cfg_epoch: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            saved: Mutex::new((0, 0, 0)),
            tx,
        });
        me.saved.lock().2 = me.hashes.generation();
        let worker = Worker::new(me.clone(), shared, refresh, rx);
        let r = std::thread::Builder::new().name("fennec-extract".into()).spawn(move || worker.run());
        if let Err(e) = r {
            tracing::error!(error = %e, "cannot start the extraction thread");
        }
        me
    }

    /// Stores without persistence for tests.
    #[cfg(test)]
    pub fn in_memory(shared: SharedIndex, settings: &Settings) -> Arc<Self> {
        Self::start(shared, settings, None, Arc::new(|| {}))
    }

    /// Installs the shared handles and the slow-search policy into search options.
    pub fn apply_options(&self, o: &mut SearchOptions) {
        let cfg = self.cfg.lock().clone();
        o.props = Some(self.props.clone());
        // Without a content index, `content:` reads files (bounded by the budget).
        o.content = cfg.content.is_some().then(|| self.content.clone());
        o.hashes = Some(Shared::new(self.hashes.clone()));
        o.on_demand = Some(Shared::new(self.on_demand.lock().clone()));
        o.slow = cfg.slow;
    }

    /// The on-demand file source (preview text of unindexed documents).
    pub fn on_demand(&self) -> Arc<dyn OnDemand> {
        self.on_demand.lock().clone()
    }

    /// Applies changed settings: the worker re-scans when the indexes' scope changed.
    pub fn configure(&self, settings: &Settings) {
        let cfg = StoreConfig::from_settings(settings);
        let mut cur = self.cfg.lock();
        if **cur == cfg {
            return;
        }
        let limits_changed = cur.content_limits() != cfg.content_limits();
        let index_changed = cur.props != cfg.props || cur.content != cfg.content;
        if limits_changed {
            *self.on_demand.lock() = on_demand(self.provider.clone(), &cfg, self.ansi_code_page);
        }
        self.active.store(cfg.active(), Ordering::Release);
        *cur = Arc::new(cfg);
        drop(cur);
        if index_changed {
            self.cfg_epoch.fetch_add(1, Ordering::AcqRel);
            let _ = self.tx.send(Msg::Config);
        }
    }

    /// Current configuration.
    pub fn config(&self) -> Arc<StoreConfig> {
        self.cfg.lock().clone()
    }

    /// Extraction progress while files are queued.
    pub fn progress(&self) -> Option<Progress> {
        let total = self.total.load(Ordering::Acquire);
        let done = self.done.load(Ordering::Acquire).min(total);
        (total > 0 && done < total).then_some(Progress { done, total })
    }

    /// Changes whenever extracted data changed (cache key for property cells).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(name))
    }

    /// Loads the stores for the final index `s` (or creates empty ones).
    fn load(&self, s: &IndexStore) -> (PropertyStore, ContentStore) {
        let cfg = self.config();
        let load_props = || {
            let path = self.path("props.fnx").filter(|p| p.exists() && !cfg.props.is_empty());
            let mut st = path
                .and_then(|p| match PropertyStore::load(&p, s) {
                    Ok(st) => Some(st),
                    Err(e) => {
                        tracing::warn!(error = %e, "props.fnx unusable; re-extracting");
                        None
                    }
                })
                .unwrap_or_else(|| PropertyStore::new(s.instance()));
            if st.set_indexed(&cfg.props.stored()) {
                tracing::info!("indexed properties changed; re-extracting");
            }
            st
        };
        let load_content = || {
            let path = self.path("content.fnx").filter(|p| p.exists() && cfg.content.is_some());
            path.and_then(|p| match ContentStore::load(&p, s) {
                Ok(st) => Some(st),
                Err(e) => {
                    tracing::warn!(error = %e, "content.fnx unusable; re-extracting");
                    None
                }
            })
            .unwrap_or_else(|| ContentStore::new(s.instance()))
        };
        std::thread::scope(|sc| {
            let c = sc.spawn(load_content);
            let p = load_props();
            let c = c.join().unwrap_or_else(|_| ContentStore::new(s.instance()));
            (p, c)
        })
    }

    fn bump_generation(&self) {
        let g = self.props.snapshot().generation() ^ self.content.snapshot().generation().rotate_left(32);
        self.generation.store(g, Ordering::Release);
    }
}

fn on_demand(provider: Option<Arc<dyn PropertyProvider>>, cfg: &StoreConfig, ansi: u16) -> Arc<dyn OnDemand> {
    let mut properties = PropertyExtractor::new();
    properties.fallback = provider;
    let limits = cfg.content_limits();
    let fallback = if limits.ifilter { crate::ifilter::provider() } else { None };
    let content = ContentExtractor { limits, ansi_code_page: ansi, fallback };
    Arc::new(DefaultOnDemand { properties, content })
}

/// Identity of a file's data: size and date modified (renames keep it).
fn data_key(s: &IndexStore, id: EntryId) -> u64 {
    let size = s.size(id).unwrap_or(u64::MAX);
    xxhash_rust::xxh3::xxh3_64(&[size.to_le_bytes(), s.date_modified(id).0.to_le_bytes()].concat())
}

/// Extension of `id` (folded, without the dot).
fn ext_bytes(s: &IndexStore, id: EntryId) -> &[u8] {
    let e = s.ext_dict().name(s.ext_id(id));
    if e.is_empty() { fnc_index::ext::ext_of(s.fold(id)) } else { e }
}

fn ext_of(s: &IndexStore, id: EntryId) -> String {
    String::from_utf8_lossy(ext_bytes(s, id)).into_owned()
}

/// `true` if `ext` is listed (an empty list allows every extension).
fn ext_listed(list: &[String], ext: &[u8]) -> bool {
    list.is_empty() || list.iter().any(|e| e.as_bytes() == ext)
}

impl IndexHooks for Stores {
    fn attach(&self, s: &IndexStore) {
        let (p, c) = self.load(s);
        let mut keys = HashMap::new();
        for &id in p.rows().ids().iter().chain(c.rows().ids()) {
            let e = EntryId(id);
            if id != fnc_props::rows::NONE && e.idx() < s.len() {
                keys.insert(id, data_key(s, e));
            }
        }
        tracing::info!(properties = p.len(), documents = c.len(), "stores attached");
        let gens = (p.generation(), c.generation());
        *self.props.write() = p;
        *self.content.write() = c;
        {
            let mut saved = self.saved.lock();
            saved.0 = gens.0;
            saved.1 = gens.1;
        }
        self.bump_generation();
        let _ = self.tx.send(Msg::Attached(keys));
    }

    fn compacted(&self, old_instance: u64, remap: &[u32], new_instance: u64) {
        {
            let mut p = self.props.write();
            if p.instance() == old_instance {
                p.remap(remap, new_instance);
            }
        }
        {
            let mut c = self.content.write();
            if c.instance() == old_instance {
                c.remap(remap, new_instance);
            }
        }
        let _ = self.tx.send(Msg::Compacted { old: old_instance, remap: remap.into(), new: new_instance });
    }

    fn ready(&self) {
        let _ = self.tx.send(Msg::Ready);
    }

    fn wants_changes(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn changed(&self, ids: Vec<EntryId>, moved: Vec<EntryId>) {
        let _ = self.tx.send(Msg::Changed(ids, moved));
    }

    fn save(&self, s: &IndexStore) {
        let mut saved = self.saved.lock();
        let t = Instant::now();
        // Serialise under the read locks, write without them.
        let props = {
            let p = self.props.snapshot();
            (p.generation() != saved.0 && p.instance() == s.instance()).then(|| (p.generation(), p.to_bytes(s)))
        };
        let content = {
            let c = self.content.snapshot();
            (c.generation() != saved.1 && c.instance() == s.instance()).then(|| (c.generation(), c.to_bytes(s)))
        };
        let write = |name: &str, bytes: &[u8]| match self.path(name) {
            Some(path) => match fnc_props::fnx::write_atomic(&path, bytes) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(file = name, error = %e, "cannot save");
                    false
                }
            },
            None => false,
        };
        if let Some((g, Ok(bytes))) = props
            && write("props.fnx", &bytes)
        {
            saved.0 = g;
            tracing::info!(kib = bytes.len() >> 10, "props.fnx saved");
        }
        if let Some((g, Ok(bytes))) = content
            && write("content.fnx", &bytes)
        {
            saved.1 = g;
            tracing::info!(kib = bytes.len() >> 10, "content.fnx saved");
        }
        let hg = self.hashes.generation();
        if hg != saved.2
            && let Some(path) = self.path("hashes.fnx")
        {
            match self.hashes.save(&path) {
                Ok(()) => saved.2 = hg,
                Err(e) => tracing::warn!(error = %e, "cannot save hashes.fnx"),
            }
        }
        tracing::debug!(elapsed = ?t.elapsed(), "stores saved");
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Config);
    }
}

/// Entries touched by a batch of live changes (collected by the indexer's watch sink
/// around `IndexStore::apply`, only while an index is on).
#[derive(Default, Debug)]
pub struct Touched {
    /// Changed entries (files and folders).
    pub ids: Vec<EntryId>,
    /// Folders renamed or moved (their subtree may have left or entered the scope).
    pub moved: Vec<EntryId>,
}

/// References to resolve once the batch is applied.
pub struct AfterApply {
    refs: Vec<(fnc_core::VolumeId, Resolve, bool)>,
}

enum Resolve {
    Ref(EntryRef),
    Path(fnc_index::RelPath),
}

impl Touched {
    /// `true` if nothing was touched.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.moved.is_empty()
    }

    /// Records entries that exist before `batch` is applied (deleted, renamed and
    /// modified ones) and returns what can only be resolved afterwards.
    pub fn before(&mut self, s: &IndexStore, batch: &ChangeBatch) -> AfterApply {
        let mut refs = Vec::new();
        let vol0 = fnc_core::VolumeId(0);
        for c in batch {
            match c {
                Change::Delete(r) | Change::Meta { target: r, .. } => {
                    if let Some(id) = s.resolve(*r) {
                        self.ids.push(id);
                    }
                }
                Change::Rename { target, .. } => {
                    if let Some(id) = s.resolve(*target) {
                        self.ids.push(id);
                    }
                    refs.push((vol0, Resolve::Ref(*target), true));
                }
                Change::Create { volume, frn, .. } | Change::Touch { volume, frn, .. } => {
                    refs.push((*volume, Resolve::Ref(EntryRef::Frn(*volume, *frn)), false));
                }
                Change::Path { volume, change } => match change {
                    PathChange::Upsert { path, .. } => refs.push((*volume, Resolve::Path(path.clone()), false)),
                    PathChange::Remove { path } => {
                        if let Some(id) = s.resolve_path(*volume, path) {
                            self.ids.push(id);
                        }
                    }
                    PathChange::Rename { to, .. } => refs.push((*volume, Resolve::Path(to.clone()), true)),
                    PathChange::SyncDir { dir, children } => {
                        // Deleted children are dropped by the dead-row sweep.
                        for (name, _) in children {
                            let mut p = dir.clone();
                            p.push(name.clone());
                            refs.push((*volume, Resolve::Path(p), false));
                        }
                    }
                },
            }
        }
        AfterApply { refs }
    }

    /// Resolves the references recorded by [`Touched::before`] against the updated index.
    pub fn after(&mut self, s: &IndexStore, a: AfterApply) {
        for (vol, r, renamed) in a.refs {
            let id = match r {
                Resolve::Ref(r) => s.resolve(r),
                Resolve::Path(p) => s.resolve_path(vol, &p),
            };
            if let Some(id) = id {
                if renamed && s.is_dir(id) {
                    self.moved.push(id);
                } else {
                    self.ids.push(id);
                }
            }
        }
    }
}

/// Folder part of a scope, resolved against one index snapshot.
enum FolderScope {
    /// No folders configured: everything.
    All,
    /// Literal folders as entry ids (tested by ancestry, no path strings).
    Roots(HashSet<u32>),
    /// Wildcard folders: match the full path.
    Paths,
}

impl FolderScope {
    fn of(s: &IndexStore, folders: &[String]) -> Self {
        if folders.is_empty() {
            FolderScope::All
        } else if folders.iter().any(|f| f.contains(['*', '?'])) {
            FolderScope::Paths
        } else {
            FolderScope::Roots(folders.iter().filter_map(|f| s.find_path(f)).map(|e| e.0).collect())
        }
    }
}

/// Property and content folder scopes of one snapshot.
struct Scopes {
    props: FolderScope,
    content: FolderScope,
}

/// `true` if an ancestor of `id` is one of `roots`.
fn under(s: &IndexStore, id: EntryId, roots: &HashSet<u32>) -> bool {
    let mut p = s.parent(id);
    for _ in 0..1024 {
        if !p.is_some() || p.idx() >= s.len() {
            return false;
        }
        if roots.contains(&p.0) {
            return true;
        }
        let next = s.parent(p);
        if next == p {
            return false;
        }
        p = next;
    }
    false
}

/// One file to extract.
struct Job {
    id: EntryId,
    path: PathBuf,
    ext: String,
    key: u64,
    props: bool,
    content: bool,
}

struct Worker {
    stores: Arc<Stores>,
    shared: SharedIndex,
    refresh: Arc<dyn Fn() + Send + Sync>,
    rx: Receiver<Msg>,
    /// Data key of every extracted entry (renamed vs modified).
    keys: HashMap<u32, u64>,
    queue: VecDeque<EntryId>,
    queued: HashSet<u32>,
    pending: HashSet<u32>,
    pending_moved: HashSet<u32>,
    first_change: Option<Instant>,
    last_change: Option<Instant>,
    attached: bool,
    ready: bool,
    full_scan: bool,
    cfg: Arc<StoreConfig>,
    last_refresh: Instant,
    dirty: bool,
    content_full: bool,
}

impl Worker {
    fn new(stores: Arc<Stores>, shared: SharedIndex, refresh: Arc<dyn Fn() + Send + Sync>, rx: Receiver<Msg>) -> Self {
        let cfg = stores.config();
        Self {
            stores,
            shared,
            refresh,
            rx,
            keys: HashMap::new(),
            queue: VecDeque::new(),
            queued: HashSet::new(),
            pending: HashSet::new(),
            pending_moved: HashSet::new(),
            first_change: None,
            last_change: None,
            attached: false,
            ready: false,
            full_scan: false,
            cfg,
            last_refresh: Instant::now(),
            dirty: false,
            content_full: false,
        }
    }

    fn stopped(&self) -> bool {
        self.stores.stopped.load(Ordering::Acquire)
    }

    fn run(mut self) {
        if !fnc_platform::privilege::enter_background_mode() {
            tracing::debug!("background thread mode unavailable");
        }
        while !self.stopped() {
            let timeout = if !self.queue.is_empty() || self.full_scan {
                Duration::ZERO
            } else if let (Some(first), Some(last)) = (self.first_change, self.last_change) {
                (last + DEBOUNCE).min(first + DEBOUNCE_MAX).saturating_duration_since(Instant::now())
            } else if self.dirty {
                REFRESH_EVERY.saturating_sub(self.last_refresh.elapsed())
            } else {
                Duration::from_secs(3600)
            };
            match self.rx.recv_timeout(timeout) {
                Ok(m) => self.handle(m),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(m) = self.rx.try_recv() {
                self.handle(m);
            }
            if self.stopped() {
                break;
            }
            if !(self.attached && self.ready) {
                continue;
            }
            if self.full_scan {
                self.full_scan = false;
                self.scan_all();
            }
            let due = match (self.first_change, self.last_change) {
                (Some(first), Some(last)) => last.elapsed() >= DEBOUNCE || first.elapsed() >= DEBOUNCE_MAX,
                _ => false,
            };
            if due {
                self.process_changes();
            }
            if !self.queue.is_empty() {
                self.extract_batch();
            }
            if self.queue.is_empty() {
                self.stores.total.store(0, Ordering::Release);
                self.stores.done.store(0, Ordering::Release);
            }
            if self.dirty && (self.queue.is_empty() || self.last_refresh.elapsed() >= REFRESH_EVERY) {
                self.dirty = false;
                self.last_refresh = Instant::now();
                self.stores.bump_generation();
                (self.refresh)();
            }
        }
        tracing::debug!("extraction thread stopped");
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Attached(keys) => {
                self.keys = keys;
                self.attached = true;
            }
            Msg::Compacted { old, remap, new } => {
                let _ = (old, new);
                self.keys = self
                    .keys
                    .drain()
                    .filter_map(|(id, k)| remap.get(id as usize).copied().filter(|&n| n != u32::MAX).map(|n| (n, k)))
                    .collect();
                let fix = |set: &mut HashSet<u32>| {
                    *set =
                        set.iter().filter_map(|&i| remap.get(i as usize).copied()).filter(|&n| n != u32::MAX).collect()
                };
                fix(&mut self.queued);
                fix(&mut self.pending);
                fix(&mut self.pending_moved);
                self.queue = self.queued.iter().map(|&i| EntryId(i)).collect();
            }
            Msg::Ready => {
                self.ready = true;
                self.full_scan = true;
            }
            Msg::Changed(ids, moved) => {
                self.pending.extend(ids.iter().map(|i| i.0));
                self.pending_moved.extend(moved.iter().map(|i| i.0));
                let now = Instant::now();
                self.first_change.get_or_insert(now);
                self.last_change = Some(now);
            }
            Msg::Config => {
                let cfg = self.stores.config();
                if *cfg == *self.cfg {
                    return;
                }
                self.cfg = cfg;
                self.apply_config();
                self.full_scan = true;
            }
        }
    }

    /// Follows a configuration change: covered properties, content on/off.
    fn apply_config(&mut self) {
        let instance = self.shared.snapshot().instance();
        {
            let mut p = self.stores.props.write();
            if self.cfg.props.is_empty() {
                if !p.is_empty() || !p.indexed().is_empty() {
                    *p = PropertyStore::new(instance);
                }
            } else if p.set_indexed(&self.cfg.props.stored()) {
                tracing::info!("indexed properties changed; re-extracting");
            }
        }
        if self.cfg.content.is_none() {
            let mut c = self.stores.content.write();
            if !c.is_empty() {
                *c = ContentStore::new(instance);
            }
        }
        self.content_full = false;
        self.queue.clear();
        self.queued.clear();
        self.stores.total.store(0, Ordering::Release);
        self.stores.done.store(0, Ordering::Release);
        self.dirty = true;
    }

    /// What `id` needs: `(props, content)` extraction, and whether it is in either scope.
    fn wants(&self, s: &IndexStore, id: EntryId, sc: &Scopes, path: &mut String) -> (bool, bool) {
        let f = s.flags(id);
        if !s.is_live(id) || s.is_dir(id) || f.contains(EntryFlags::OFFLINE) || f.contains(EntryFlags::ROOT) {
            return (false, false);
        }
        let e = ext_bytes(s, id);
        let p_ext = !self.cfg.props.is_empty() && ext_listed(&self.cfg.props.extensions, e);
        let c_ext = self.cfg.content.as_ref().is_some_and(|c| {
            ext_listed(&c.scope.extensions, e) && s.size(id).is_some_and(|z| z <= c.limits.max_file_bytes)
        });
        if !p_ext && !c_ext {
            return (false, false);
        }
        // Folder scope: by ancestry when the folders are literal (no path string needed).
        let mut built = false;
        let mut in_scope = |scope: &FolderScope, folders: &[String]| match scope {
            FolderScope::All => true,
            FolderScope::Roots(roots) => under(s, id, roots),
            FolderScope::Paths => {
                if !built {
                    s.full_path(id, path);
                    built = true;
                }
                folders.iter().any(|f| fnc_props::config::folder_matches(f, path))
            }
        };
        let p = p_ext && in_scope(&sc.props, &self.cfg.props.folders);
        let c = c_ext && self.cfg.content.as_ref().is_some_and(|c| in_scope(&sc.content, &c.scope.folders));
        (p, c)
    }

    fn scopes(&self, s: &IndexStore) -> Scopes {
        Scopes {
            props: FolderScope::of(s, &self.cfg.props.folders),
            content: self.cfg.content.as_ref().map_or(FolderScope::All, |c| FolderScope::of(s, &c.scope.folders)),
        }
    }

    /// Queues every in-scope file that is missing from a store and drops rows that left
    /// the scope or whose entry is gone.
    fn scan_all(&mut self) {
        let t = Instant::now();
        let epoch = self.stores.cfg_epoch.load(Ordering::Acquire);
        let n = self.shared.snapshot().len();
        let mut path = String::new();
        let (mut drop_p, mut drop_c) = (Vec::new(), Vec::new());
        let mut queued = 0usize;
        for start in (0..n).step_by(SCAN_CHUNK) {
            if self.stopped() || self.stores.cfg_epoch.load(Ordering::Acquire) != epoch {
                return;
            }
            let s = self.shared.snapshot();
            let p = self.stores.props.snapshot();
            let c = self.stores.content.snapshot();
            let (p_ok, c_ok) = (p.instance() == s.instance(), c.instance() == s.instance());
            let sc = self.scopes(&s);
            for i in start..(start + SCAN_CHUNK).min(s.len()) {
                let id = EntryId(i as u32);
                let (wp, wc) = self.wants(&s, id, &sc, &mut path);
                let (hp, hc) = (p_ok && p.has(id), c_ok && c.has(id));
                if hp && !wp {
                    drop_p.push(id);
                }
                if hc && !wc {
                    drop_c.push(id);
                }
                if ((wp && !hp) || (wc && !hc && !self.content_full)) && self.queued.insert(id.0) {
                    self.queue.push_back(id);
                    queued += 1;
                }
            }
        }
        self.remove(&drop_p, &drop_c);
        self.stores.total.fetch_add(queued, Ordering::AcqRel);
        tracing::info!(queued, dropped = drop_p.len() + drop_c.len(), elapsed = ?t.elapsed(), "store scan");
    }

    fn remove(&mut self, props: &[EntryId], content: &[EntryId]) {
        if !props.is_empty() {
            let mut p = self.stores.props.write();
            for &id in props {
                p.remove(id);
            }
            self.dirty = true;
        }
        if !content.is_empty() {
            let mut c = self.stores.content.write();
            for &id in content {
                c.remove(id);
            }
            self.dirty = true;
        }
        let (p, c) = (self.stores.props.snapshot(), self.stores.content.snapshot());
        for id in props.iter().chain(content) {
            if !p.has(*id) && !c.has(*id) {
                self.keys.remove(&id.0);
            }
        }
    }

    /// Live changes after the debounce: drop deleted and modified entries (re-queueing
    /// the modified ones), re-check renamed ones and the subtrees of moved folders.
    fn process_changes(&mut self) {
        self.first_change = None;
        self.last_change = None;
        let ids: Vec<u32> = self.pending.drain().collect();
        let moved: Vec<u32> = self.pending_moved.drain().collect();
        let mut path = String::new();
        let (mut drop_p, mut drop_c, mut requeue) = (Vec::new(), Vec::new(), Vec::new());
        {
            let s = self.shared.snapshot();
            let p = self.stores.props.snapshot();
            let c = self.stores.content.snapshot();
            let sc = self.scopes(&s);
            let mut check = |id: EntryId, path: &mut String| {
                if id.idx() >= s.len() {
                    return;
                }
                let (wp, wc) = self.wants(&s, id, &sc, path);
                let (hp, hc) = (p.has(id), c.has(id));
                let modified = s.is_live(id) && self.keys.get(&id.0).is_some_and(|&k| k != data_key(&s, id));
                if hp && (!wp || modified) {
                    drop_p.push(id);
                }
                if hc && (!wc || modified) {
                    drop_c.push(id);
                }
                if (wp && (!hp || modified)) || (wc && (!hc || modified)) {
                    requeue.push(id);
                }
            };
            for &i in &ids {
                check(EntryId(i), &mut path);
            }
            // Moved folders: their whole subtree.
            let mut stack: Vec<EntryId> = moved.iter().map(|&i| EntryId(i)).filter(|i| i.idx() < s.len()).collect();
            while let Some(d) = stack.pop() {
                for ch in s.children(d) {
                    if s.is_dir(ch) {
                        stack.push(ch);
                    } else {
                        check(ch, &mut path);
                    }
                }
            }
            // Rows of entries that are gone (deleted files, subtrees of deleted folders).
            for &r in p.rows().ids() {
                let id = EntryId(r);
                if r != fnc_props::rows::NONE && (id.idx() >= s.len() || !s.is_live(id)) {
                    drop_p.push(id);
                }
            }
            for &r in c.rows().ids() {
                let id = EntryId(r);
                if r != fnc_props::rows::NONE && (id.idx() >= s.len() || !s.is_live(id)) {
                    drop_c.push(id);
                }
            }
        }
        drop_p.sort_unstable();
        drop_p.dedup();
        drop_c.sort_unstable();
        drop_c.dedup();
        self.remove(&drop_p, &drop_c);
        let mut n = 0;
        for id in requeue {
            if self.queued.insert(id.0) {
                self.queue.push_back(id);
                n += 1;
            }
        }
        self.stores.total.fetch_add(n, Ordering::AcqRel);
        tracing::debug!(changed = ids.len(), moved = moved.len(), requeued = n, "live changes processed");
    }

    /// Extracts the next batch without holding any lock, then inserts it.
    fn extract_batch(&mut self) {
        let epoch = self.stores.cfg_epoch.load(Ordering::Acquire);
        let mut jobs = Vec::with_capacity(BATCH);
        let instance;
        {
            let s = self.shared.snapshot();
            instance = s.instance();
            let p = self.stores.props.snapshot();
            let c = self.stores.content.snapshot();
            let sc = self.scopes(&s);
            let mut path = String::new();
            while jobs.len() < BATCH {
                let Some(id) = self.queue.pop_front() else { break };
                self.queued.remove(&id.0);
                if id.idx() >= s.len() {
                    continue;
                }
                let (wp, wc) = self.wants(&s, id, &sc, &mut path);
                let props = wp && !p.has(id);
                let content = wc && !c.has(id) && !self.content_full;
                if !props && !content {
                    self.stores.done.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                s.full_path(id, &mut path);
                jobs.push(Job {
                    id,
                    path: PathBuf::from(&path),
                    ext: ext_of(&s, id),
                    key: data_key(&s, id),
                    props,
                    content,
                });
            }
        }
        if jobs.is_empty() {
            return;
        }
        let od = self.stores.on_demand();
        let wanted = self.cfg.props.stored();
        let mut results = Vec::with_capacity(jobs.len());
        for j in &jobs {
            if self.stopped() || self.stores.cfg_epoch.load(Ordering::Acquire) != epoch {
                break;
            }
            let values = j.props.then(|| od.properties(&j.path, &j.ext, &wanted));
            // Unreadable or binary files are stored empty ("no text"), so they are not
            // retried until they change.
            let text = j.content.then(|| od.content(&j.path, &j.ext).unwrap_or_default());
            results.push((j.id, j.key, values, text));
            self.stores.done.fetch_add(1, Ordering::AcqRel);
        }
        // Unprocessed jobs (cancelled) go back to the queue unless the scope changed.
        if self.stores.cfg_epoch.load(Ordering::Acquire) == epoch {
            for j in jobs.iter().skip(results.len()) {
                if self.queued.insert(j.id.0) {
                    self.queue.push_front(j.id);
                }
            }
        } else {
            return;
        }
        let limit = self.cfg.content.as_ref().map_or(0, |c| c.max_total_bytes);
        {
            let mut p = self.stores.props.write();
            if p.instance() == instance {
                for (id, _, values, _) in &mut results {
                    if let Some(v) = values.take() {
                        p.insert(*id, v);
                    }
                }
            }
        }
        {
            let mut c = self.stores.content.write();
            if c.instance() == instance {
                for (id, _, _, text) in &results {
                    if let Some(t) = text {
                        if limit > 0 && c.compressed_bytes() >= limit {
                            if !self.content_full {
                                tracing::warn!(limit_mib = limit >> 20, "content index size limit reached");
                            }
                            self.content_full = true;
                            break;
                        }
                        c.insert(*id, t);
                    }
                }
            }
        }
        for (id, key, ..) in &results {
            self.keys.insert(id.0, *key);
        }
        self.dirty = true;
    }
}

/// Default content extensions of the settings and of `fnc_props` agree.
#[cfg(test)]
pub(crate) fn default_extensions_match() -> bool {
    Settings::default().content_extensions.join(";") == fnc_props::config::DEFAULT_CONTENT_EXTENSIONS
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use fnc_props::{PropertyId, Value};

    fn index_of(dir: &Path) -> IndexStore {
        let mut store = IndexStore::default();
        let mut src = fnc_platform::walk::FolderSource::new(dir);
        let vol = store.add_volume(fnc_platform::VolumeSource::info(&src), Some(1)).unwrap();
        struct Sink<'a>(fnc_index::BulkLoader<'a>);
        impl fnc_platform::EntrySink for Sink<'_> {
            fn entry(&mut self, e: &fnc_platform::RawEntry<'_>) -> Result<(), fnc_platform::SourceError> {
                self.0
                    .push(e.frn, e.parent_frn, e.name, &e.meta)
                    .map(|_| ())
                    .map_err(|e| fnc_platform::SourceError::io("push", std::io::Error::other(e)))
            }
        }
        let mut sink = Sink(store.bulk_load(vol));
        fnc_platform::VolumeSource::enumerate(&mut src, &mut sink, &fnc_platform::StopFlag::new()).unwrap();
        sink.0.finish().unwrap();
        store
    }

    fn id_of(s: &IndexStore, name: &str) -> EntryId {
        (0..s.len() as u32).map(EntryId).find(|&i| s.is_live(i) && s.name_string(i) == name).unwrap()
    }

    fn wait(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut out);
        let px = vec![0u8; (w * h) as usize];
        image::ImageEncoder::write_image(enc, &px, w, h, image::ExtendedColorType::L8).unwrap();
        out
    }

    #[test]
    fn settings_defaults_match_the_library() {
        assert!(default_extensions_match());
        let cfg = StoreConfig::from_settings(&Settings::default());
        assert!(!cfg.active(), "both indexes are off by default");
        assert_eq!(cfg.slow, SlowPolicy { read_uncovered: false, ..SlowPolicy::default() });
        let s = Settings {
            property_names: vec!["width".into(), "Height".into(), "bogus".into()],
            content_enabled: true,
            content_max_file_mb: 2,
            ..Settings::default()
        };
        let cfg = StoreConfig::from_settings(&s);
        assert_eq!(cfg.props.properties, [PropertyId::WIDTH, PropertyId::HEIGHT]);
        assert_eq!(cfg.unknown_props, ["bogus"]);
        assert_eq!(cfg.content.as_ref().unwrap().limits.max_file_bytes, 2 << 20);
    }

    #[test]
    fn folder_scopes_by_ancestry_and_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/photos/2026")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a/photos/2026/x.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("b/y.jpg"), b"y").unwrap();
        let s = index_of(dir.path());
        let (x, y) = (id_of(&s, "x.jpg"), id_of(&s, "y.jpg"));
        let root = dir.path().join("a").join("photos").display().to_string();
        let FolderScope::Roots(roots) = FolderScope::of(&s, std::slice::from_ref(&root)) else { panic!("literal") };
        assert!(under(&s, x, &roots) && !under(&s, y, &roots));
        assert!(matches!(FolderScope::of(&s, &[]), FolderScope::All));
        assert!(matches!(FolderScope::of(&s, &["C:\\*\\photos".to_string()]), FolderScope::Paths));
        let FolderScope::Roots(none) = FolderScope::of(&s, &["/no/such/folder".to_string()]) else { panic!() };
        assert!(none.is_empty() && !under(&s, x, &none));
    }

    /// End to end on a real folder: attach → ready → full scan extracts properties and
    /// content; a modified file is re-extracted, a deleted one dropped; save + reload
    /// re-attaches the rows; compaction remaps them.
    #[test]
    fn extracts_follows_changes_saves_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("photos")).unwrap();
        std::fs::write(dir.path().join("photos/big.png"), png(4100, 3)).unwrap();
        std::fs::write(dir.path().join("photos/small.png"), png(10, 10)).unwrap();
        std::fs::write(dir.path().join("note.txt"), "the quarterly plan is ready").unwrap();
        std::fs::write(dir.path().join("other.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(dir.path().join("outside.png"), png(5000, 2)).unwrap();
        let shared = SharedIndex::new(index_of(dir.path()));
        let settings = Settings {
            property_names: vec!["width".into(), "height".into()],
            property_extensions: vec!["png".into()],
            property_folders: vec![dir.path().join("photos").display().to_string()],
            content_enabled: true,
            ..Settings::default()
        };
        let refreshed = Arc::new(AtomicUsize::new(0));
        let r = refreshed.clone();
        let stores = Stores::start(
            shared.clone(),
            &settings,
            Some(data.path().to_path_buf()),
            Arc::new(move || {
                r.fetch_add(1, Ordering::Relaxed);
            }),
        );
        stores.attach(&shared.snapshot());
        stores.ready();
        let (big, note) = {
            let s = shared.snapshot();
            (id_of(&s, "big.png"), id_of(&s, "note.txt"))
        };
        wait("extraction", || stores.props.snapshot().has(big) && stores.content.snapshot().has(note));
        wait("idle", || stores.progress().is_none() && refreshed.load(Ordering::Relaxed) > 0);
        assert_eq!(stores.props.snapshot().get(big, PropertyId::WIDTH), Some(Value::Int(4100)));
        assert_eq!(stores.content.snapshot().text(note).as_deref(), Some("the quarterly plan is ready"));
        assert_eq!(stores.content.snapshot().len(), 1, "other.bin is not a content extension");
        assert!(!stores.props.snapshot().has(note), "txt is not a property extension");
        let outside = id_of(&shared.snapshot(), "outside.png");
        assert!(!stores.props.snapshot().has(outside), "outside the property folders");

        // Search options carry the same handles.
        let mut o = SearchOptions::default();
        stores.apply_options(&mut o);
        assert!(o.props.as_ref().unwrap().ptr_eq(&stores.props));
        let mut o2 = SearchOptions::default();
        stores.apply_options(&mut o2);
        assert_eq!(o, o2, "handles are reused");

        // Modify note.txt (size + date change), delete small.png.
        std::fs::write(dir.path().join("note.txt"), "now the budget review").unwrap();
        let small = id_of(&shared.snapshot(), "small.png");
        {
            let mut w = shared.write();
            let batch = vec![
                Change::Meta {
                    target: EntryRef::Id(note),
                    meta: fnc_index::Meta {
                        size: Some(21),
                        date_modified: Some(fnc_core::FileTime(1)),
                        ..fnc_index::Meta::default()
                    },
                },
                Change::Delete(EntryRef::Id(small)),
            ];
            let mut t = Touched::default();
            let after = t.before(&w, &batch);
            w.apply(batch);
            t.after(&w, after);
            assert_eq!(t.ids.len(), 2);
            drop(w);
            stores.changed(t.ids, t.moved);
        }
        wait("re-extraction", || {
            stores.content.snapshot().text(note).as_deref() == Some("now the budget review")
                && !stores.props.snapshot().has(small)
        });

        // Save, reload into a fresh set of stores against the same index.
        stores.save(&shared.snapshot());
        assert!(data.path().join("props.fnx").exists() && data.path().join("content.fnx").exists());
        stores.stop();
        let again = Stores::start(shared.clone(), &settings, Some(data.path().to_path_buf()), Arc::new(|| {}));
        again.attach(&shared.snapshot());
        assert_eq!(again.props.snapshot().get(big, PropertyId::WIDTH), Some(Value::Int(4100)));
        assert!(again.content.snapshot().has(note));
        {
            // A restart walks the folder again into a new index: path-derived file ids
            // let the saved rows re-attach (no re-extraction).
            let rewalked = SharedIndex::new(index_of(dir.path()));
            let fresh = Stores::start(rewalked.clone(), &settings, Some(data.path().to_path_buf()), Arc::new(|| {}));
            fresh.attach(&rewalked.snapshot());
            let big_new = id_of(&rewalked.snapshot(), "big.png");
            assert_eq!(fresh.props.snapshot().get(big_new, PropertyId::WIDTH), Some(Value::Int(4100)), "re-attached");
            fresh.stop();
        }

        // Compaction (small.png's tombstone goes away): rows follow the remap.
        let old = shared.snapshot().instance();
        let remap = shared.write().compact();
        let new = shared.snapshot().instance();
        again.compacted(old, &remap, new);
        let big2 = id_of(&shared.snapshot(), "big.png");
        assert_eq!(again.props.snapshot().instance(), new);
        assert_eq!(again.props.snapshot().get(big2, PropertyId::WIDTH), Some(Value::Int(4100)));

        // Turning the content index off clears it.
        let mut off = settings.clone();
        off.content_enabled = false;
        again.ready();
        again.configure(&off);
        wait("content cleared", || again.content.snapshot().is_empty());
        again.stop();
    }

    /// A JPEG of `w` × `h` with a noisy gradient (real photo-sized data).
    #[cfg(windows)]
    fn jpeg(w: u32, h: u32, seed: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        for y in 0..h {
            for c in 0..w {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                px.extend([(c * 255 / w) as u8, (y * 255 / h) as u8, (x & 0x3f) as u8]);
            }
        }
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        image::ImageEncoder::write_image(enc, &px, w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    /// Inserts an XMP packet (APP1) after the SOI marker of a JPEG.
    #[cfg(windows)]
    fn with_xmp(jpeg: &[u8], xmp: &str) -> Vec<u8> {
        let ns = b"http://ns.adobe.com/xap/1.0/\0";
        let len = (2 + ns.len() + xmp.len()) as u16;
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(len.to_be_bytes());
        out.extend(ns);
        out.extend(xmp.as_bytes());
        out.extend(&jpeg[2..]);
        out
    }

    /// The Windows Property System fallback supplies values the built-in extractors
    /// cannot read: XMP title, keywords and rating of a JPEG without EXIF.
    #[cfg(windows)]
    #[test]
    fn windows_provider_reads_what_builtin_extractors_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:MicrosoftPhoto="http://ns.microsoft.com/photo/1.0/"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Fennec XMP title</rdf:li></rdf:Alt></dc:title><dc:subject><rdf:Bag><rdf:li>fox</rdf:li><rdf:li>desert</rdf:li></rdf:Bag></dc:subject><xmp:Rating>4</xmp:Rating><MicrosoftPhoto:Rating>75</MicrosoftPhoto:Rating></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let path = dir.path().join("xmp.jpg");
        std::fs::write(&path, with_xmp(&jpeg(64, 48, 1), xmp)).unwrap();
        let wanted = [PropertyId::WIDTH, PropertyId::TITLE, PropertyId::TAGS, PropertyId::RATING];
        let builtin = PropertyExtractor::new().extract(&path, "jpg", &wanted);
        assert!(builtin.iter().any(|(p, _)| *p == PropertyId::WIDTH), "{builtin:?}");
        assert!(!builtin.iter().any(|(p, _)| *p == PropertyId::TITLE), "built-in has no XMP: {builtin:?}");
        let provider = fnc_platform::props::provider().unwrap();
        let with = PropertyExtractor::new().with_fallback(provider).extract(&path, "jpg", &wanted);
        eprintln!("built-in: {builtin:?}\nwith the Windows Property System: {with:?}");
        assert!(with.contains(&(PropertyId::TITLE, Value::Text("Fennec XMP title".into()))), "{with:?}");
        assert!(with.iter().any(|(p, v)| *p == PropertyId::TAGS && v.as_str() == Some("fox")), "{with:?}");
        assert!(with.contains(&(PropertyId::RATING, Value::Int(4))), "{with:?}");
        // MSI summary information (no built-in parser), if the system has packages.
        let msi = std::fs::read_dir(r"C:\Windows\Installer")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("msi")));
        if let Some(msi) = msi {
            let p = fnc_platform::props::provider().unwrap();
            let v = p.get(&msi, &[PropertyId::TITLE, PropertyId::SUBJECT, PropertyId::AUTHOR, PropertyId::COMMENT]);
            eprintln!("{}: {v:?}", msi.display());
            assert!(PropertyExtractor::new().extract(&msi, "msi", &[PropertyId::TITLE]).is_empty());
        }
    }

    /// IDX-08 / IDX-09 on a real NTFS volume (Windows, elevated): the full app pipeline
    /// (NTFS index + stores + background extraction) on `FENNEC_IT_DIR` (default
    /// `D:\fennec-m3-it`) with photos of more than 4000 px and 10,000 notes; then
    /// `width:>4000` and `content:"quarterly plan"` through the search engine with the
    /// app's options, a live change, and a restart that re-attaches the saved stores.
    ///
    /// `cargo test -p fnc-app --release -- --ignored --nocapture ntfs_property_and_content_index`
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Windows, administrator rights and a writable NTFS volume"]
    fn ntfs_property_and_content_index() {
        use crate::indexer::{IndexEvent, IndexStatus, Indexer, IndexerConfig};
        use fnc_exec::{EngineEvent, SearchEngine, SearchRequest, SortSpec};
        crate::i18n::init("en");
        assert!(fnc_platform::privilege::is_elevated(), "run elevated");
        // Indexer and store timings (attach, scan, saves) on stderr.
        let _log = fnc_core::logging::init(&fnc_core::logging::LogConfig {
            dir: None,
            prefix: "it",
            level: "fennec=info",
            max_files: 1,
            console: true,
        });
        let base = PathBuf::from(std::env::var("FENNEC_IT_DIR").unwrap_or_else(|_| r"D:\fennec-m3-it".into()));
        let (photos, notes) = (base.join("Photos"), base.join("Notes"));
        let n_notes: usize = std::env::var("FENNEC_IT_NOTES").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
        let t = Instant::now();
        if !base.join("ready.txt").exists() {
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&photos).unwrap();
            std::fs::create_dir_all(&notes).unwrap();
            for i in 0..12u32 {
                let (w, h) = if i % 2 == 0 { (4500, 3000) } else { (3000, 4800) };
                std::fs::write(photos.join(format!("big-{i:02}.jpg")), jpeg(w, h, i)).unwrap();
            }
            for i in 0..60u32 {
                std::fs::write(photos.join(format!("small-{i:02}.jpg")), jpeg(1600, 1200, 100 + i)).unwrap();
            }
            let words = ["alpha", "budget", "review", "meeting", "notes", "roadmap", "draft", "summary", "q3", "team"];
            for i in 0..n_notes {
                let mut text = String::new();
                for k in 0..40 {
                    text.push_str(words[(i * 7 + k * 3) % words.len()]);
                    text.push(if k % 10 == 9 { '\n' } else { ' ' });
                }
                if i % 200 == 0 {
                    text.push_str("\nThe quarterly plan is attached.\n");
                }
                let ext = if i % 3 == 0 { "md" } else { "txt" };
                std::fs::write(notes.join(format!("note-{i:05}.{ext}")), text).unwrap();
            }
            std::fs::write(base.join("ready.txt"), b"ok").unwrap();
        }
        let expected_notes = n_notes.div_ceil(200);
        eprintln!("setup: 72 photos and {n_notes} notes in {:?}", t.elapsed());

        let data = base.join("data");
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        let settings = Settings {
            property_names: vec!["width".into(), "height".into(), "date-taken".into(), "camera-model".into()],
            property_folders: vec![photos.display().to_string()],
            content_enabled: true,
            content_folders: vec![notes.display().to_string()],
            ..Settings::default()
        };
        struct Run {
            engine: SearchEngine,
            rx: crossbeam_channel::Receiver<EngineEvent>,
            irx: crossbeam_channel::Receiver<IndexEvent>,
            indexer: Indexer,
            stores: Arc<Stores>,
            shared: SharedIndex,
            started: Instant,
            next: u64,
        }
        let start = |reindex: bool| -> Run {
            let shared = SharedIndex::default();
            let (tx, rx) = crossbeam_channel::unbounded();
            let engine = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
            let handle = engine.handle();
            let stores =
                Stores::start(shared.clone(), &settings, Some(data.clone()), Arc::new(move || handle.refresh()));
            let (itx, irx) = crossbeam_channel::unbounded();
            let handle = engine.handle();
            let cfg = IndexerConfig {
                ntfs: true,
                usn_create: true,
                usn_max_size: 32 << 20,
                usn_delta: 4 << 20,
                usn_poll: Duration::from_millis(1000),
                db_path: Some(data.join("index.fnx")),
                reindex,
                hooks: Some(stores.clone()),
                ..IndexerConfig::default()
            };
            let started = Instant::now();
            let indexer = Indexer::spawn(cfg, shared.clone(), Arc::new(move || handle.refresh()), itx, Arc::new(|| {}));
            Run { engine, rx, irx, indexer, stores, shared, started, next: 0 }
        };
        impl Run {
            fn ready(&self) -> Duration {
                loop {
                    match self.irx.recv_timeout(Duration::from_secs(600)) {
                        Ok(IndexEvent::Status(IndexStatus::Ready { .. })) => return self.started.elapsed(),
                        Ok(_) => {}
                        Err(e) => panic!("indexer never became ready: {e}"),
                    }
                }
            }
            /// Runs `q`; returns (engine time, wall time, result count) of the first
            /// outcome satisfying `ok`.
            fn search(&mut self, q: &str, ok: &dyn Fn(usize) -> bool) -> (Duration, Duration, usize) {
                let mut o = SearchOptions::default();
                self.stores.apply_options(&mut o);
                // Measure full executions (no narrowing from the previous search).
                o.reuse = false;
                self.next += 1;
                let id = self.next;
                let t = Instant::now();
                self.engine.search(SearchRequest {
                    id,
                    text: q.into(),
                    filter: None,
                    sort: SortSpec::default(),
                    options: Arc::new(o),
                });
                loop {
                    assert!(t.elapsed() < Duration::from_secs(120), "timed out waiting for `{q}`");
                    if let Ok(EngineEvent::Done(out)) = self.rx.recv_timeout(Duration::from_millis(100))
                        && out.id == id
                        && ok(out.results.len())
                    {
                        return (out.elapsed, t.elapsed(), out.results.len());
                    }
                }
            }
            fn stop(self) {
                self.indexer.stop();
                self.indexer.save();
                std::thread::sleep(Duration::from_millis(1500));
            }
        }

        // 1. Build, extract in the background.
        let mut run = start(true);
        let ready = run.ready();
        let t = Instant::now();
        while !(run.stores.progress().is_none()
            && run.stores.props.snapshot().len() >= 72
            && run.stores.content.snapshot().len() >= n_notes)
        {
            assert!(t.elapsed() < Duration::from_secs(900), "extraction never finished: {:?}", run.stores.progress());
            std::thread::sleep(Duration::from_millis(50));
        }
        let extract = t.elapsed();
        let (props_n, docs, comp, raw) = {
            let c = run.stores.content.snapshot();
            (run.stores.props.snapshot().len(), c.len(), c.compressed_bytes(), c.text_bytes())
        };
        eprintln!(
            "index ready after {ready:?}; extraction of {props_n} photos + {docs} notes took {extract:?} \
             (content {} KiB text, {} KiB stored)",
            raw >> 10,
            comp >> 10
        );

        // IDX-08: `width:>4000` is instant (indexed values only, no file reads).
        let mut times = Vec::new();
        for _ in 0..20 {
            // A different search in between: every timed run executes in full.
            run.search("ext:zzz", &|_| true);
            let (e, w, n) = run.search("width:>4000", &|n| n > 0);
            assert_eq!(n, 6, "the six landscape photos are wider than 4000 px");
            times.push((e, w));
        }
        times.sort();
        eprintln!(
            "IDX-08: width:>4000 -> 6 results of {} entries; engine p50 {:?} max {:?}; wall p50 {:?}",
            run.shared.snapshot().live_count(),
            times[10].0,
            times[19].0,
            times[10].1
        );
        let (_, _, tall) = run.search("height:>4000 ext:jpg", &|n| n > 0);
        assert_eq!(tall, 6);
        assert!(times[19].0 < Duration::from_millis(50), "instant: {:?}", times[19].0);

        // IDX-09: `content:"quarterly plan"` < 200 ms.
        let mut times = Vec::new();
        for _ in 0..20 {
            run.search("ext:zzz", &|_| true);
            let (e, w, n) = run.search("content:\"quarterly plan\"", &|n| n > 0);
            assert_eq!(n, expected_notes);
            times.push((e, w));
        }
        times.sort();
        eprintln!(
            "IDX-09: content:\"quarterly plan\" -> {expected_notes} results; engine p50 {:?} max {:?}; wall p50 {:?}",
            times[10].0, times[19].0, times[10].1
        );
        assert!(times[19].0 < Duration::from_millis(200), "IDX-09 budget: {:?}", times[19].0);

        // Live change: a note gains the phrase → re-extracted after the debounce.
        let changed = notes.join("note-00001.txt");
        let t = Instant::now();
        std::fs::write(&changed, "Edited: the quarterly plan moved to Friday.").unwrap();
        let (_, _, n) = run.search("content:\"quarterly plan\"", &|n| n == expected_notes + 1);
        eprintln!("live change: modified note found after {:?}", t.elapsed());
        assert_eq!(n, expected_notes + 1);
        std::fs::write(&changed, "back to normal").unwrap();
        let (_, _, n) = run.search("content:\"quarterly plan\"", &|n| n == expected_notes);
        assert_eq!(n, expected_notes);
        run.stop();
        for f in ["index.fnx", "props.fnx", "content.fnx"] {
            let len = std::fs::metadata(data.join(f)).map(|m| m.len()).unwrap_or(0);
            eprintln!("{f}: {} KiB", len >> 10);
            assert!(len > 0, "{f} saved");
        }

        // Restart: the saved stores re-attach; searchable without re-extraction.
        let mut run = start(false);
        let (_, w, n) = run.search("width:>4000", &|n| n == 6);
        eprintln!("restart: width:>4000 -> {n} after {:?} (search wall {w:?})", run.started.elapsed());
        let (_, _, n) = run.search("content:\"quarterly plan\"", &|n| n > 0);
        assert_eq!(n, expected_notes);
        eprintln!("restart: content index re-attached, {} documents", run.stores.content.snapshot().len());
        run.stop();
    }
}

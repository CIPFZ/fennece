//! Background indexing: builds the index from all sources, publishes it, back-fills
//! sizes/dates and keeps it current from the USN journal (SPEC ch.0 §4.1–4.2).
//!
//! The initial build happens in a private `IndexStore` (no lock contention), which is
//! then swapped into the shared index in one step. Later updates are applied in small
//! batches under the write lock, and the current search is refreshed (debounced).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use fnc_core::fold::fold_utf16_into;
use fnc_core::{EntryId, VolumeId};
use fnc_index::{BulkLoader, Change, ChangeBatch, EntryRef, IndexOptions, IndexStore, Meta, SharedIndex, VolumeKind};
use fnc_platform::walk::FolderSource;
use fnc_platform::{ChangeSink, EntrySink, MetaSink, RawEntry, SourceError, StopFlag, VolumeSource};
use xxhash_rust::xxh3::xxh3_64;

/// Indexing progress shown in the status bar.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexStatus {
    /// Nothing done yet.
    Starting,
    /// Enumerating a volume.
    Building {
        /// Volume display name.
        volume: String,
        /// Entries so far.
        done: u64,
    },
    /// Index searchable; back-filling sizes and dates.
    Metadata {
        /// Percent done.
        percent: u8,
    },
    /// Fully built.
    Ready {
        /// Live entries.
        entries: usize,
        /// Build time.
        seconds: f64,
    },
}

/// Messages from the indexer.
#[derive(Clone, Debug)]
pub enum IndexEvent {
    /// Progress.
    Status(IndexStatus),
    /// Problem worth showing (already localised).
    Message(String),
}

/// What to index.
#[derive(Clone, Debug, Default)]
// The NTFS fields are only read on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct IndexerConfig {
    /// EFU file lists.
    pub file_lists: Vec<PathBuf>,
    /// Folder index roots.
    pub folders: Vec<PathBuf>,
    /// Index local NTFS/ReFS volumes (Windows).
    pub ntfs: bool,
    /// Excluded volumes (drive letters such as `D:`).
    pub ntfs_exclude: Vec<String>,
    /// Create missing USN journals.
    pub usn_create: bool,
    /// Journal size when creating (bytes).
    pub usn_max_size: u64,
    /// Journal allocation delta (bytes).
    pub usn_delta: u64,
    /// Idle poll interval.
    pub usn_poll: Duration,
    /// Keep date accessed.
    pub date_accessed: bool,
    /// Database file (`None` = `-nodb`).
    pub db_path: Option<PathBuf>,
    /// Ignore the database (`-reindex`).
    pub reindex: bool,
    /// Periodic save interval (zero = only on exit).
    pub save_interval: Duration,
    /// Instance name (service pipe).
    pub instance: String,
}

type Callback = Arc<dyn Fn() + Send + Sync>;

/// Handle to the indexing threads.
pub struct Indexer {
    stop: StopFlag,
    saver: Arc<Saver>,
}

impl Indexer {
    /// Starts indexing. `refresh` re-runs the current search; `notify` wakes the UI.
    pub fn spawn(
        cfg: IndexerConfig,
        shared: SharedIndex,
        refresh: Callback,
        events: Sender<IndexEvent>,
        notify: Callback,
    ) -> Self {
        let stop = StopFlag::new();
        let saver = Arc::new(Saver {
            path: cfg.db_path.clone(),
            shared: shared.clone(),
            interval: cfg.save_interval,
            published: false.into(),
            last: parking_lot::Mutex::new(u64::MAX),
        });
        let ctx = RunCtx { shared, refresh, events, notify, stop: stop.clone(), saver: saver.clone() };
        let spawn = std::thread::Builder::new().name("fennec-indexer".into()).spawn(move || run(cfg, ctx));
        if let Err(e) = spawn {
            tracing::error!(error = %e, "cannot start indexer");
        }
        Self { stop, saver }
    }

    /// Stops all indexing threads (they exit at their next check).
    pub fn stop(&self) {
        self.stop.stop();
    }

    /// Saves the database now (exit, `-save-db`); no-op before the first build.
    pub fn save(&self) {
        self.saver.save();
    }

    /// An indexer that does nothing (the index is provided by the caller).
    #[cfg(test)]
    pub fn idle() -> Self {
        let saver = Arc::new(Saver {
            path: None,
            shared: SharedIndex::default(),
            interval: Duration::ZERO,
            published: false.into(),
            last: parking_lot::Mutex::new(0),
        });
        Self { stop: StopFlag::new(), saver }
    }
}

struct Emitter {
    tx: Sender<IndexEvent>,
    notify: Callback,
}

impl Emitter {
    fn status(&self, s: IndexStatus) {
        let _ = self.tx.send(IndexEvent::Status(s));
        (self.notify)();
    }

    fn message(&self, m: String) {
        tracing::warn!("{m}");
        let _ = self.tx.send(IndexEvent::Message(m));
        (self.notify)();
    }
}

struct LoadSink<'a, 'e> {
    bulk: BulkLoader<'a>,
    volume: String,
    emit: &'e Emitter,
    last: Instant,
}

impl EntrySink for LoadSink<'_, '_> {
    fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
        self.bulk
            .push(e.frn, e.parent_frn, e.name, &e.meta)
            .map(|_| ())
            .map_err(|err| SourceError::io("index insert", std::io::Error::other(err)))
    }

    fn progress(&mut self, done: u64, _total: u64) {
        if self.last.elapsed() > Duration::from_millis(100) {
            self.last = Instant::now();
            self.emit.status(IndexStatus::Building { volume: self.volume.clone(), done });
        }
    }
}

fn collect_sources(cfg: &IndexerConfig, emit: &Emitter) -> Vec<Box<dyn VolumeSource>> {
    let mut sources: Vec<Box<dyn VolumeSource>> = Vec::new();
    #[cfg(windows)]
    if cfg.ntfs {
        let opts = fnc_ntfs::NtfsOptions {
            create_journal: cfg.usn_create,
            max_size: cfg.usn_max_size,
            allocation_delta: cfg.usn_delta,
            poll_interval: cfg.usn_poll,
        };
        for d in fnc_ntfs::list_volumes() {
            let letter = format!("{}:", d.letter);
            if cfg.ntfs_exclude.iter().any(|x| x.trim_end_matches('\\').eq_ignore_ascii_case(&letter)) {
                continue;
            }
            let root = d.root.clone();
            match fnc_ntfs::NtfsSource::open(d, opts, &cfg.instance) {
                Ok(s) => sources.push(Box::new(s)),
                Err(SourceError::AccessDenied(_)) => {
                    emit.message(crate::i18n::t_args("index-folder-fallback", &[("volume", letter.into())]));
                    sources.push(Box::new(FolderSource::new(root)));
                }
                Err(e) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
            }
        }
    }
    for f in &cfg.folders {
        if f.is_dir() {
            sources.push(Box::new(FolderSource::new(f.clone())));
        } else {
            emit.message(crate::i18n::t_args(
                "index-error",
                &[("message", format!("{}: not a folder", f.display()).into())],
            ));
        }
    }
    sources
}

/// Loads the database, if configured and readable. A damaged file is kept as
/// `index.fnx.bad` and the index is rebuilt.
fn load_db(cfg: &IndexerConfig, emit: &Emitter) -> Option<IndexStore> {
    let path = cfg.db_path.as_ref().filter(|_| !cfg.reindex)?;
    if !path.exists() {
        return None;
    }
    let t = Instant::now();
    match fnc_index::persist::load(path) {
        Ok(s) => {
            tracing::info!(entries = s.live_count(), elapsed = ?t.elapsed(), "database loaded");
            Some(s)
        }
        Err(e) => {
            tracing::warn!(error = %e, "database unusable; rebuilding");
            let _ = std::fs::rename(path, path.with_extension("fnx.bad"));
            emit.message(crate::i18n::t_args("index-db-rebuild", &[("message", e.to_string().into())]));
            None
        }
    }
}

/// Enumerates `src` into volume `vol` of `store`; `false` if stopped.
fn enumerate_into(
    store: &mut IndexStore,
    vol: VolumeId,
    src: &mut dyn VolumeSource,
    emit: &Emitter,
    stop: &StopFlag,
) -> bool {
    let t = Instant::now();
    let name = src.info().display_name;
    let mut sink = LoadSink { bulk: store.bulk_load(vol), volume: name.clone(), emit, last: Instant::now() };
    let result = src.enumerate(&mut sink, stop);
    let stats = sink.bulk.finish();
    match (result, stats) {
        (Ok(()), Ok(st)) => {
            tracing::info!(volume = %name, entries = st.entries, orphans = st.orphans, elapsed = ?t.elapsed(), "volume indexed")
        }
        (Err(SourceError::Stopped), _) => return false,
        (Err(e), _) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
        (_, Err(e)) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
    }
    if let Some((jid, usn)) = src.journal_position() {
        store.set_journal_position(vol, jid, usn);
    }
    if src.enumerates_metadata()
        && let Some(v) = store.volume_mut(vol)
    {
        v.metadata_complete = true;
    }
    true
}

fn run(cfg: IndexerConfig, ctx: RunCtx) {
    let RunCtx { shared, refresh, events, notify, stop, saver } = ctx;
    let emit = Emitter { tx: events, notify };
    let t0 = Instant::now();
    emit.status(IndexStatus::Starting);
    let mut store =
        load_db(&cfg, &emit).unwrap_or_else(|| IndexStore::new(IndexOptions { date_accessed: cfg.date_accessed }));
    let loaded = !store.is_empty();

    // File lists and folder indexes are re-read on every start (their content can
    // change while Fennec is not running); their old records are reused.
    let stale: Vec<VolumeId> = store
        .volumes()
        .iter()
        .filter(|v| !v.is_retired() && matches!(v.info.kind, VolumeKind::FileList | VolumeKind::Folder))
        .map(|v| v.id)
        .collect();
    for v in stale {
        store.drop_volume(v);
    }
    for efu in &cfg.file_lists {
        emit.status(IndexStatus::Building { volume: efu.display().to_string(), done: 0 });
        match fnc_index::efu::load_efu_file(&mut store, efu) {
            Ok((vol, _)) => {
                if let Some(v) = store.volume_mut(vol) {
                    v.metadata_complete = true;
                }
            }
            Err(e) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
        }
    }

    let mut sources = collect_sources(&cfg, &emit);
    let mut volumes: Vec<(VolumeId, Box<dyn VolumeSource>)> = Vec::new();
    let mut kept: std::collections::HashSet<VolumeId> = std::collections::HashSet::new();
    for mut src in sources.drain(..) {
        if stop.is_stopped() {
            return;
        }
        let info = src.info();
        let journaled = matches!(info.kind, VolumeKind::Ntfs | VolumeKind::Refs);
        let existing = store
            .volumes()
            .iter()
            .find(|v| journaled && !v.is_retired() && v.info.kind == info.kind && v.info.guid == info.guid)
            .map(|v| (v.id, v.usn_journal_id, v.next_usn));
        let vol = match existing {
            Some((vol, jid, usn)) => {
                kept.insert(vol);
                if let Some(v) = store.volume_mut(vol) {
                    v.info = info.clone();
                    v.online = true;
                }
                match src.resume(jid, usn) {
                    Ok(()) => {
                        tracing::info!(volume = %info.display_name, next_usn = usn, "resuming from the database");
                        volumes.push((vol, src));
                        continue;
                    }
                    Err(e) => {
                        // IDX-05: journal deleted/recreated or wrapped → rebuild this volume only.
                        tracing::info!(volume = %info.display_name, reason = %e, "rebuilding volume");
                        emit.message(crate::i18n::t_args(
                            "index-volume-rebuild",
                            &[("volume", info.display_name.clone().into())],
                        ));
                        store.clear_volume(vol);
                        if let Some(v) = store.volume_mut(vol) {
                            v.metadata_complete = false;
                        }
                        vol
                    }
                }
            }
            None => match store.add_volume(info, Some(src.root_frn())) {
                Ok(v) => {
                    kept.insert(v);
                    v
                }
                Err(e) => {
                    emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())]));
                    continue;
                }
            },
        };
        if !enumerate_into(&mut store, vol, src.as_mut(), &emit, &stop) {
            return;
        }
        volumes.push((vol, src));
    }
    // Journaled volumes from the database that are gone or excluded now.
    let gone: Vec<VolumeId> = store
        .volumes()
        .iter()
        .filter(|v| {
            matches!(v.info.kind, VolumeKind::Ntfs | VolumeKind::Refs) && !v.is_retired() && !kept.contains(&v.id)
        })
        .map(|v| v.id)
        .collect();
    for v in gone {
        store.drop_volume(v);
    }
    if store.len() > 1024 && store.live_count() * 5 < store.len() * 4 {
        store.compact();
    }
    store.shrink_to_fit();
    let order = store.name_order();
    if order.covered() != store.len() || order.dirty_count() > 0 {
        store.rebuild_name_order();
    }
    let entries = store.live_count();
    shared.replace(store);
    refresh();
    let seconds = t0.elapsed().as_secs_f64();
    tracing::info!(entries, seconds, loaded, "index published");
    saver.published();
    saver.spawn_periodic(stop.clone());

    let refresher = Refresher::spawn(refresh);
    let needs_meta: Vec<VolumeId> = {
        let snap = shared.snapshot();
        volumes
            .iter()
            .filter(|(v, s)| !s.enumerates_metadata() && !snap.volume_info(*v).is_some_and(|i| i.metadata_complete))
            .map(|(v, _)| *v)
            .collect()
    };
    if needs_meta.is_empty() {
        emit.status(IndexStatus::Ready { entries, seconds });
    }
    let emit = Arc::new(emit);
    let total_meta = needs_meta.len();
    let done_meta = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Marks a volume's back-fill complete and reports "ready" after the last one.
    let meta_finished = {
        let (shared, emit, done_meta) = (shared.clone(), emit.clone(), done_meta.clone());
        Arc::new(move |vol: VolumeId, complete: bool| {
            if complete && let Some(v) = shared.write().volume_mut(vol) {
                v.metadata_complete = true;
            }
            let finished = done_meta.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if finished == total_meta {
                // Rows added by the back-fill (hard-link names) grew the columns
                // geometrically; give the slack back once (one-time, tens of ms).
                let (entries, heap) = {
                    let mut s = shared.write();
                    s.shrink_to_fit();
                    // Fold the alias rows out of the children overlay into the CSR.
                    s.rebuild_children();
                    (s.live_count(), s.heap_bytes())
                };
                tracing::info!(entries, heap_mib = heap / (1 << 20), "index ready");
                emit.status(IndexStatus::Ready { entries, seconds: t0.elapsed().as_secs_f64() });
            }
        })
    };
    let workers = Arc::new(Workers {
        shared: shared.clone(),
        stop: stop.clone(),
        emit,
        refresher,
        active: parking_lot::Mutex::new(std::collections::HashSet::new()),
    });
    for (vol, src) in volumes {
        let wants_meta = needs_meta.contains(&vol);
        workers.start(vol, src, wants_meta, meta_finished.clone());
    }
    #[cfg(windows)]
    if cfg.ntfs {
        let w = workers.clone();
        let _ = std::thread::Builder::new().name("fennec-volumes".into()).spawn(move || monitor_volumes(&cfg, &w));
    }
    #[cfg(not(windows))]
    let _ = (&cfg, &workers);
}

type MetaDone = Arc<dyn Fn(VolumeId, bool) + Send + Sync>;

/// Starts and tracks the per-volume threads (journal watch, metadata back-fill).
struct Workers {
    shared: SharedIndex,
    stop: StopFlag,
    emit: Arc<Emitter>,
    refresher: Refresher,
    /// GUIDs of volumes being watched.
    active: parking_lot::Mutex<std::collections::HashSet<String>>,
}

impl Workers {
    fn start(self: &Arc<Self>, vol: VolumeId, mut src: Box<dyn VolumeSource>, wants_meta: bool, done: MetaDone) {
        let guid = src.info().guid;
        self.active.lock().insert(guid.clone());
        // Back-fill concurrently with journal monitoring when the source allows it, so
        // the journal is read from the start (no risk of it wrapping during a long
        // back-fill).
        // SPEC-GAP: a folder listed by the back-fill after a USN-triggered refresh of one
        // of its files can overwrite that refresh with slightly older size/dates until
        // the file changes again.
        let fetcher = if wants_meta { src.metadata_fetcher() } else { None };
        let inline_meta = wants_meta && fetcher.is_none();
        if let Some(fetcher) = fetcher {
            let (me, done) = (self.clone(), done.clone());
            let r = std::thread::Builder::new().name(format!("fennec-meta-{}", vol.0)).spawn(move || {
                let complete = fill_metadata(
                    &|d, o, s| fetcher.fetch(d, o, s),
                    vol,
                    &me.shared,
                    &me.stop,
                    &me.emit,
                    &me.refresher,
                );
                done(vol, complete);
            });
            if let Err(e) = r {
                tracing::error!(error = %e, "cannot start metadata thread");
            }
        }
        let me = self.clone();
        let r = std::thread::Builder::new().name(format!("fennec-volume-{}", vol.0)).spawn(move || {
            if inline_meta {
                let complete = fill_metadata(
                    &|d, o, s| src.fetch_metadata(d, o, s),
                    vol,
                    &me.shared,
                    &me.stop,
                    &me.emit,
                    &me.refresher,
                );
                done(vol, complete);
            }
            let name = src.info().display_name;
            let mut sink = WatchSink { shared: me.shared.clone(), refresher: me.refresher.clone(), vol };
            match src.watch(vol, &mut sink, &me.stop) {
                Ok(()) | Err(SourceError::Stopped | SourceError::Unsupported(_)) => {}
                Err(SourceError::Offline(_)) => {
                    tracing::info!(volume = %name, "volume went offline");
                    me.shared.write().set_volume_online(vol, false);
                    sink.refresher.poke();
                }
                Err(SourceError::JournalReset(_)) => {
                    // The monitor sees the volume as inactive and rebuilds it.
                    tracing::info!(volume = %name, "journal reset while watching");
                    me.shared.write().set_volume_online(vol, false);
                    sink.refresher.poke();
                }
                Err(e) => me.emit.message(crate::i18n::t_args(
                    "index-watch-error",
                    &[("volume", name.into()), ("message", e.to_string().into())],
                )),
            }
            me.active.lock().remove(&guid);
        });
        if let Err(e) = r {
            tracing::error!(error = %e, "cannot start volume thread");
        }
    }
}

/// Volume arrival/removal (SPEC ch.1 §2.3): polls the fixed NTFS/ReFS drives every
/// 3 s. A new volume is enumerated privately and absorbed into the index; a volume
/// seen before resumes from its journal position (entries come back online).
#[cfg(windows)]
fn monitor_volumes(cfg: &IndexerConfig, w: &Arc<Workers>) {
    let opts = fnc_ntfs::NtfsOptions {
        create_journal: cfg.usn_create,
        max_size: cfg.usn_max_size,
        allocation_delta: cfg.usn_delta,
        poll_interval: cfg.usn_poll,
    };
    let mut failed: std::collections::HashMap<String, Instant> = std::collections::HashMap::new();
    while !w.stop.is_stopped() {
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(500));
            if w.stop.is_stopped() {
                return;
            }
        }
        for d in fnc_ntfs::list_volumes() {
            let letter = format!("{}:", d.letter);
            if cfg.ntfs_exclude.iter().any(|x| x.trim_end_matches('\\').eq_ignore_ascii_case(&letter)) {
                continue;
            }
            let guid = if d.guid.is_empty() { d.root.clone() } else { d.guid.clone() };
            if w.active.lock().contains(&guid)
                || failed.get(&guid).is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            {
                continue;
            }
            let mut src = match fnc_ntfs::NtfsSource::open(d, opts, &cfg.instance) {
                Ok(s) => s,
                Err(e) => {
                    tracing::info!(volume = %letter, error = %e, "cannot open arrived volume");
                    failed.insert(guid, Instant::now());
                    continue;
                }
            };
            let info = src.info();
            let existing = {
                let snap = w.shared.snapshot();
                snap.volumes()
                    .iter()
                    .find(|v| !v.is_retired() && v.info.kind == info.kind && v.info.guid == info.guid)
                    .map(|v| (v.id, v.usn_journal_id, v.next_usn, v.metadata_complete))
            };
            if let Some((vol, jid, usn, meta_ok)) = existing {
                if src.resume(jid, usn).is_ok() {
                    tracing::info!(volume = %info.display_name, "volume back online; resuming its journal");
                    w.shared.write().set_volume_online(vol, true);
                    w.refresher.poke();
                    w.start(vol, Box::new(src), !meta_ok, Arc::new(|_, _| {}));
                    continue;
                }
                w.shared.write().drop_volume(vol);
            }
            let t = Instant::now();
            let mut private = IndexStore::new(IndexOptions { date_accessed: cfg.date_accessed });
            let Ok(pv) = private.add_volume(info.clone(), Some(src.root_frn())) else { continue };
            if !enumerate_into(&mut private, pv, &mut src, &w.emit, &w.stop) {
                return;
            }
            let vol = {
                let mut s = w.shared.write();
                match s.absorb(private) {
                    Ok(ids) => {
                        let order = s.name_order();
                        if order.covered() < s.len() * 9 / 10 {
                            s.rebuild_name_order();
                        }
                        ids[0]
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot add volume");
                        continue;
                    }
                }
            };
            tracing::info!(volume = %info.display_name, elapsed = ?t.elapsed(), "volume arrived and indexed");
            w.emit.message(crate::i18n::t_args("index-volume-added", &[("volume", info.display_name.clone().into())]));
            w.refresher.poke();
            let shared = w.shared.clone();
            w.start(
                vol,
                Box::new(src),
                true,
                Arc::new(move |v, complete| {
                    if complete && let Some(vi) = shared.write().volume_mut(v) {
                        vi.metadata_complete = true;
                    }
                }),
            );
        }
    }
}

struct RunCtx {
    shared: SharedIndex,
    refresh: Callback,
    events: Sender<IndexEvent>,
    notify: Callback,
    stop: StopFlag,
    saver: Arc<Saver>,
}

/// Writes the database: periodically, on demand and at exit (SPEC ch.1 §5.3).
pub struct Saver {
    path: Option<PathBuf>,
    shared: SharedIndex,
    interval: Duration,
    published: std::sync::atomic::AtomicBool,
    /// Generation of the last save (skip unchanged saves); serialises saves.
    last: parking_lot::Mutex<u64>,
}

impl Saver {
    fn published(&self) {
        self.published.store(true, std::sync::atomic::Ordering::Release);
    }

    /// Saves now if the index was built and changed since the last save.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if !self.published.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut last = self.last.lock();
        let snap = self.shared.snapshot();
        if snap.generation() == *last {
            return;
        }
        let t = Instant::now();
        match fnc_index::persist::save(&snap, path) {
            Ok(st) => {
                *last = snap.generation();
                tracing::info!(rows = st.rows, mib = st.bytes >> 20, elapsed = ?t.elapsed(), "database saved");
            }
            Err(e) => tracing::warn!(error = %e, "cannot save the database"),
        }
    }

    fn spawn_periodic(self: &Arc<Self>, stop: StopFlag) {
        if self.path.is_none() || self.interval.is_zero() {
            return;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new().name("fennec-db-save".into()).spawn(move || {
            let mut next = Instant::now() + me.interval;
            while !stop.is_stopped() {
                std::thread::sleep(Duration::from_millis(500));
                if Instant::now() >= next {
                    me.save();
                    next = Instant::now() + me.interval;
                }
            }
        });
    }
}

/// Coalesces "index changed" signals into at most one search refresh per 50 ms.
#[derive(Clone)]
struct Refresher {
    tx: Sender<()>,
}

impl Refresher {
    fn spawn(refresh: Callback) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<()>();
        let _ = std::thread::Builder::new().name("fennec-refresh".into()).spawn(move || {
            while rx.recv().is_ok() {
                std::thread::sleep(Duration::from_millis(50));
                while rx.try_recv().is_ok() {}
                refresh();
            }
        });
        Self { tx }
    }

    fn poke(&self) {
        let _ = self.tx.send(());
    }
}

struct WatchSink {
    shared: SharedIndex,
    refresher: Refresher,
    vol: VolumeId,
}

/// Maximum changes applied under one write lock (keeps lock hold time < 5 ms).
const MAX_BATCH: usize = 4096;

impl ChangeSink for WatchSink {
    fn changes(&mut self, mut batch: ChangeBatch) -> bool {
        let mut changed = false;
        while !batch.is_empty() {
            let rest = if batch.len() > MAX_BATCH { batch.split_off(MAX_BATCH) } else { Vec::new() };
            let t = Instant::now();
            let stats = self.shared.write().apply(batch);
            tracing::trace!(?stats, held = ?t.elapsed(), "applied USN batch");
            changed |= stats.changed();
            batch = rest;
        }
        if changed {
            self.refresher.poke();
        }
        true
    }

    fn checkpoint(&mut self, journal_id: u64, next_usn: i64) {
        self.shared.write().set_journal_position(self.vol, journal_id, next_usn);
    }
}

/// Resolves `(folder, child name)` to entries of one batch of folders.
struct MetaCollector {
    vol: VolumeId,
    children: HashMap<(u32, u64), EntryId>,
    out: ChangeBatch,
    fold: Vec<u8>,
}

impl MetaSink for MetaCollector {
    fn meta(&mut self, dir: EntryId, name: &[u16], meta: Meta, file_id: Option<u64>) {
        self.fold.clear();
        fold_utf16_into(name, &mut self.fold);
        // SPEC-GAP: children are matched by a 64-bit hash of the fold key (collision
        // probability ~2^-64 per folder); a collision would assign metadata to a sibling.
        if let Some(&id) = self.children.get(&(dir.0, xxh3_64(&self.fold))) {
            self.out.push(Change::Meta { target: EntryRef::Id(id), meta });
        } else if let Some(frn) = file_id {
            // A name the MFT enumeration did not report: an additional hard link of a
            // known file (the common case, e.g. System32 → WinSxS) or a file created
            // meanwhile. `Touch` adds it as an alias or inserts it.
            self.out.push(Change::Touch { volume: self.vol, frn, parent: EntryRef::Id(dir), name: name.into(), meta });
        }
    }
}

/// Folder lister used by the back-fill (`VolumeSource::fetch_metadata` or a detached
/// `MetadataFetcher`).
type FetchFn<'a> = &'a dyn Fn(&[(EntryId, String)], &mut dyn MetaSink, &StopFlag) -> Result<(), SourceError>;

fn fill_metadata(
    fetch: FetchFn<'_>,
    vol: VolumeId,
    shared: &SharedIndex,
    stop: &StopFlag,
    emit: &Emitter,
    refresher: &Refresher,
) -> bool {
    let t = Instant::now();
    let dirs: Vec<(EntryId, String)> = {
        let snap = shared.snapshot();
        let mut buf = String::new();
        (0..snap.len() as u32)
            .map(EntryId)
            .filter(|&id| snap.is_live(id) && snap.is_dir(id) && snap.volume_of(id) == vol)
            .map(|id| {
                snap.full_path(id, &mut buf);
                if !buf.ends_with('\\') && snap.flags(id).contains(fnc_core::EntryFlags::ROOT) {
                    buf.push('\\');
                }
                (id, buf.clone())
            })
            .collect()
    };
    let total = dirs.len().max(1);
    let mut last_report = Instant::now();
    for (k, chunk) in dirs.chunks(1024).enumerate() {
        if stop.is_stopped() {
            return false;
        }
        let mut collector = MetaCollector { vol, children: HashMap::new(), out: Vec::new(), fold: Vec::new() };
        {
            let snap = shared.snapshot();
            for (d, _) in chunk {
                for c in snap.children(*d) {
                    collector.children.insert((d.0, xxh3_64(snap.fold(c))), c);
                }
            }
        }
        if fetch(chunk, &mut collector, stop).is_err() {
            return false;
        }
        let mut batch = std::mem::take(&mut collector.out);
        while !batch.is_empty() {
            let rest = if batch.len() > MAX_BATCH { batch.split_off(MAX_BATCH) } else { Vec::new() };
            shared.write().apply_initial(batch);
            batch = rest;
        }
        refresher.poke();
        if last_report.elapsed() > Duration::from_millis(250) {
            last_report = Instant::now();
            let percent = (((k + 1) * 1024).min(total) * 100 / total) as u8;
            emit.status(IndexStatus::Metadata { percent });
        }
    }
    tracing::info!(volume = vol.0, folders = dirs.len(), elapsed = ?t.elapsed(), "sizes and dates filled");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-volume end-to-end test (Windows, elevated): MFT enumeration of all fixed
    /// NTFS volumes (IDX-01), then live USN changes in `FENNEC_IT_DIR` (default
    /// `D:\fennec-it`): create -> visible (IDX-02), rename of a folder with 100 000
    /// children (IDX-03), size refresh on close, delete.
    ///
    /// `cargo test -p fnc-app --release -- --ignored --nocapture ntfs_end_to_end`
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Windows, administrator rights and a writable NTFS volume"]
    fn ntfs_end_to_end() {
        use fnc_exec::{EngineEvent, SearchEngine, SearchOptions, SearchRequest, SortSpec};
        crate::i18n::init("en");
        assert!(fnc_platform::privilege::is_elevated(), "run elevated");
        let base = std::path::PathBuf::from(std::env::var("FENNEC_IT_DIR").unwrap_or_else(|_| r"D:\fennec-it".into()));
        let _ = std::fs::remove_dir_all(&base);
        let big = base.join("big");
        std::fs::create_dir_all(&big).unwrap();
        let n_big: usize = std::env::var("FENNEC_IT_CHILDREN").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
        let t = Instant::now();
        for i in 0..n_big {
            std::fs::File::create(big.join(format!("child{i:06}.dat"))).unwrap();
        }
        eprintln!("setup: created {n_big} files in {:?}", t.elapsed());

        let shared = SharedIndex::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let engine = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
        let (itx, irx) = crossbeam_channel::unbounded();
        let handle = engine.handle();
        let cfg = IndexerConfig {
            ntfs: true,
            usn_create: true,
            usn_max_size: 32 << 20,
            usn_delta: 4 << 20,
            usn_poll: Duration::from_millis(1000),
            ..IndexerConfig::default()
        };
        let t0 = Instant::now();
        let _ix = Indexer::spawn(cfg, shared.clone(), Arc::new(move || handle.refresh()), itx, Arc::new(|| {}));
        let mut next_id = 0u64;
        // Runs `text` until `ok(count)` holds (re-running on every index refresh).
        let mut wait_for = |text: &str, ok: &dyn Fn(usize) -> bool, limit: Duration| -> (Duration, usize) {
            let start = Instant::now();
            next_id += 1;
            let my_id = next_id;
            engine.search(SearchRequest {
                id: my_id,
                text: text.into(),
                filter: None,
                sort: SortSpec::default(),
                options: Arc::new(SearchOptions::default()),
            });
            loop {
                assert!(start.elapsed() < limit, "timed out waiting for `{text}`");
                if let Ok(EngineEvent::Done(o)) = rx.recv_timeout(Duration::from_millis(100))
                    && o.id == my_id
                    && ok(o.results.len())
                {
                    return (start.elapsed(), o.results.len());
                }
            }
        };

        // IDX-01: searchable after enumeration (names), ready after the size/date back-fill.
        let (_, _) = wait_for("child000000.dat", &|n| n == 1, Duration::from_secs(600));
        let searchable = t0.elapsed();
        let entries = shared.snapshot().live_count();
        eprintln!("IDX-01: {entries} entries searchable after {searchable:?}");
        let ready = loop {
            match irx.recv_timeout(Duration::from_secs(600)) {
                Ok(IndexEvent::Status(IndexStatus::Ready { entries, seconds })) => break (entries, seconds),
                Ok(IndexEvent::Message(m)) => eprintln!("indexer: {m}"),
                Ok(_) => {}
                Err(e) => panic!("indexer never became ready: {e}"),
            }
        };
        eprintln!("IDX-01: ready (sizes/dates filled) with {} entries after {:.1} s", ready.0, ready.1);
        assert!(searchable < Duration::from_secs(60), "IDX-01 budget is 60 s");
        // Metafile folders get their real path (`$Extend` is synthesized), and nothing
        // else should be orphaned on a healthy volume.
        let (_, rm) = wait_for("wfn:$RmMetadata parent:C:\\$Extend", &|_| true, Duration::from_secs(10));
        let (_, orphans) =
            wait_for("ancestor:C:\\$Orphans | ancestor:D:\\$Orphans", &|_| true, Duration::from_secs(10));
        eprintln!("C:\\$Extend\\$RmMetadata found: {rm}; entries under $Orphans: {orphans}");
        assert_eq!(rm, 1);
        // Hard links: System32 names of files whose MFT name is in WinSxS.
        let (_, k32) = wait_for("wfn:kernel32.dll parent:C:\\Windows\\System32", &|n| n == 1, Duration::from_secs(60));
        let (_, aliases) = wait_for("wfn:kernel32.dll", &|_| true, Duration::from_secs(10));
        eprintln!("hard links: C:\\Windows\\System32\\kernel32.dll found: {k32}; kernel32.dll names: {aliases}");

        // IDX-02: a new file becomes visible within 1 s.
        let name = format!("fennec-e2e-{}.txt", std::process::id());
        let created_at = Instant::now();
        std::fs::create_dir_all(base.join("t")).unwrap();
        std::fs::write(base.join("t").join(&name), b"hello").unwrap();
        let (lat, _) = wait_for(&name, &|n| n == 1, Duration::from_secs(10));
        let total = created_at.elapsed();
        eprintln!("IDX-02: create -> visible {total:?} (search wait {lat:?})");
        assert!(total < Duration::from_secs(1), "IDX-02 budget is 1 s, took {total:?}");

        // Size refresh on close.
        std::fs::write(base.join("t").join(&name), vec![b'x'; 123_456]).unwrap();
        let (lat, _) = wait_for(&format!("{name} size:123456"), &|n| n == 1, Duration::from_secs(10));
        eprintln!("size refresh visible after {lat:?}");

        // IDX-03: rename the folder with all children; paths update within 2 s.
        let renamed_at = Instant::now();
        std::fs::rename(&big, base.join("big2")).unwrap();
        let q = format!("{}\\big2\\ child0{:05}", base.display(), n_big - 1);
        let (lat, _) = wait_for(&q, &|n| n == 1, Duration::from_secs(10));
        let total = renamed_at.elapsed();
        let (_, old) = wait_for(&format!("{}\\big\\ child", base.display()), &|_| true, Duration::from_secs(10));
        eprintln!(
            "IDX-03: rename of {n_big}-child folder -> paths updated {total:?} (search {lat:?}); old path hits {old}"
        );
        assert!(total < Duration::from_secs(2), "IDX-03 budget is 2 s");
        assert_eq!(old, 0);
        let (_, all) = wait_for(&format!("{}\\big2\\", base.display()), &|n| n == n_big, Duration::from_secs(10));
        assert_eq!(all, n_big);

        // Delete.
        let deleted_at = Instant::now();
        std::fs::remove_file(base.join("t").join(&name)).unwrap();
        let (_, _) = wait_for(&name, &|n| n == 0, Duration::from_secs(10));
        eprintln!("delete -> gone {:?}", deleted_at.elapsed());
        std::fs::remove_dir_all(&base).ok();
    }

    /// A live indexer + engine for the Windows real-volume tests.
    #[cfg(windows)]
    struct Live {
        engine: fnc_exec::SearchEngine,
        rx: crossbeam_channel::Receiver<fnc_exec::EngineEvent>,
        irx: crossbeam_channel::Receiver<IndexEvent>,
        indexer: Indexer,
        shared: SharedIndex,
        next_id: u64,
        started: Instant,
    }

    #[cfg(windows)]
    impl Live {
        fn start(db: &std::path::Path, reindex: bool) -> Self {
            let shared = SharedIndex::default();
            let (tx, rx) = crossbeam_channel::unbounded();
            let engine = fnc_exec::SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
            let (itx, irx) = crossbeam_channel::unbounded();
            let handle = engine.handle();
            let cfg = IndexerConfig {
                ntfs: true,
                usn_create: true,
                usn_max_size: 32 << 20,
                usn_delta: 4 << 20,
                usn_poll: Duration::from_millis(1000),
                db_path: Some(db.to_path_buf()),
                reindex,
                ..IndexerConfig::default()
            };
            let started = Instant::now();
            let indexer = Indexer::spawn(cfg, shared.clone(), Arc::new(move || handle.refresh()), itx, Arc::new(|| {}));
            Self { engine, rx, irx, indexer, shared, next_id: 0, started }
        }

        /// Waits for `IndexStatus::Ready`, collecting indexer messages.
        fn ready(&self) -> (Duration, Vec<String>) {
            let mut msgs = Vec::new();
            loop {
                match self.irx.recv_timeout(Duration::from_secs(600)) {
                    Ok(IndexEvent::Status(IndexStatus::Ready { .. })) => return (self.started.elapsed(), msgs),
                    Ok(IndexEvent::Message(m)) => msgs.push(m),
                    Ok(_) => {}
                    Err(e) => panic!("indexer never became ready: {e}"),
                }
            }
        }

        fn count(&mut self, text: &str, ok: &dyn Fn(usize) -> bool, limit: Duration) -> usize {
            use fnc_exec::{EngineEvent, SearchOptions, SearchRequest, SortSpec};
            let start = Instant::now();
            self.next_id += 1;
            let id = self.next_id;
            self.engine.search(SearchRequest {
                id,
                text: text.into(),
                filter: None,
                sort: SortSpec::default(),
                options: Arc::new(SearchOptions::default()),
            });
            let mut last = 0;
            while start.elapsed() < limit {
                if let Ok(EngineEvent::Done(o)) = self.rx.recv_timeout(Duration::from_millis(100))
                    && o.id == id
                {
                    last = o.results.len();
                    if ok(last) {
                        return last;
                    }
                }
            }
            panic!("timed out waiting for `{text}` (last count {last})");
        }

        fn stop(self) {
            self.indexer.stop();
            self.indexer.save();
            // Let the volume threads observe the stop flag.
            std::thread::sleep(Duration::from_millis(1500));
            drop(self.shared);
        }
    }

    /// IDX-04 / IDX-05 on real volumes (Windows, elevated): build and save; make 100
    /// changes while "not running", restart and expect the database to be searchable
    /// within 2 s with the changes replayed; delete D:'s USN journal, restart and
    /// expect D: to be rebuilt while C: is still resumed.
    ///
    /// `cargo test -p fnc-app --release -- --ignored --nocapture ntfs_persistence`
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Windows, administrator rights, NTFS C: and D:"]
    fn ntfs_persistence() {
        crate::i18n::init("en");
        assert!(fnc_platform::privilege::is_elevated(), "run elevated");
        let base = std::path::PathBuf::from(std::env::var("FENNEC_IT_DIR").unwrap_or_else(|_| r"D:\fennec-it".into()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let db = std::env::temp_dir().join(format!("fennec-it-{}.fnx", std::process::id()));
        let _ = std::fs::remove_file(&db);

        // 1. Initial build.
        let live = Live::start(&db, false);
        let (t, _) = live.ready();
        eprintln!("initial build ready after {t:?}");
        live.stop();
        let size = std::fs::metadata(&db).unwrap().len();
        eprintln!("database: {} MiB", size >> 20);

        // 2. 100 changes while Fennec is not running.
        for i in 0..60 {
            std::fs::write(base.join(format!("offline-new-{i:03}.txt")), b"x").unwrap();
        }
        for i in 0..20 {
            std::fs::rename(
                base.join(format!("offline-new-{i:03}.txt")),
                base.join(format!("offline-renamed-{i:03}.txt")),
            )
            .unwrap();
        }
        for i in 20..40 {
            std::fs::remove_file(base.join(format!("offline-new-{i:03}.txt"))).unwrap();
        }
        let mut live = Live::start(&db, false);
        let q = format!("{}\\ offline-", base.display());
        let n = live.count(&q, &|n| n == 40, Duration::from_secs(10));
        let t = live.started.elapsed();
        let resumed = live.count("kernel32.dll", &|n| n > 0, Duration::from_secs(5));
        eprintln!("IDX-04: restart -> {n} replayed entries searchable after {t:?} (kernel32.dll: {resumed})");
        assert!(t < Duration::from_secs(2), "IDX-04 budget is 2 s");
        assert_eq!(live.count(&format!("{q}renamed"), &|_| true, Duration::from_secs(5)), 20);
        let (t, msgs) = live.ready();
        eprintln!("ready after {t:?}; messages: {msgs:?}");
        assert!(msgs.is_empty());
        live.stop();

        // 3. IDX-05: delete D:'s journal; D: must be rebuilt, C: resumed.
        let out = std::process::Command::new("fsutil").args(["usn", "deletejournal", "/d", "D:"]).output().unwrap();
        assert!(out.status.success(), "fsutil: {}", String::from_utf8_lossy(&out.stderr));
        std::thread::sleep(Duration::from_secs(3));
        let mut live = Live::start(&db, false);
        let (t, msgs) = live.ready();
        eprintln!("IDX-05: ready after {t:?}; messages: {msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("D:")), "D: was rebuilt");
        assert!(!msgs.iter().any(|m| m.contains("C:")), "C: was not rebuilt");
        assert_eq!(live.count(&format!("{q}renamed"), &|n| n == 20, Duration::from_secs(10)), 20);
        assert!(live.count("wfn:kernel32.dll parent:C:\\Windows\\System32", &|n| n == 1, Duration::from_secs(10)) == 1);
        live.stop();
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&db);
    }

    /// IDX-10: a *non-elevated* process indexes NTFS through `fennec-service`
    /// (install it first: `fennec-service -install`). Run as a standard user:
    /// `fennec-<hash>.exe --ignored --nocapture ntfs_service_mode`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Windows, a standard (non-admin) user and the running fennec-service"]
    fn ntfs_service_mode() {
        crate::i18n::init("en");
        assert!(!fnc_platform::privilege::is_elevated(), "run as a standard user");
        let base = std::path::PathBuf::from(
            std::env::var("FENNEC_IT_DIR").unwrap_or_else(|_| r"C:\Users\Public\fennec-it".into()),
        );
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let db = base.join("index.fnx");
        let mut live = Live::start(&db, true);
        let (t, msgs) = live.ready();
        let entries = live.shared.snapshot().live_count();
        eprintln!("IDX-10: {entries} entries ready after {t:?}; messages: {msgs:?}");
        assert!(msgs.is_empty(), "no folder-index fallback: {msgs:?}");
        assert!(entries > 100_000);
        let n = live.count("wfn:kernel32.dll parent:C:\\Windows\\System32", &|n| n == 1, Duration::from_secs(10));
        assert_eq!(n, 1);
        let name = format!("svc-mode-{}.txt", std::process::id());
        let t = Instant::now();
        std::fs::write(base.join(&name), b"hello").unwrap();
        live.count(&name, &|n| n == 1, Duration::from_secs(5));
        eprintln!("IDX-10: USN change via the service visible after {:?}", t.elapsed());
        assert!(t.elapsed() < Duration::from_secs(1));
        live.stop();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Volume arrival/removal on Windows (elevated): attaches a VHD as `T:`, expects it
    /// to be indexed while running, detaches it (entries go offline), re-attaches it
    /// (journal resumed, entries online again).
    #[cfg(windows)]
    #[test]
    #[ignore = "needs Windows, administrator rights and diskpart"]
    fn ntfs_volume_hotplug() {
        crate::i18n::init("en");
        assert!(fnc_platform::privilege::is_elevated(), "run elevated");
        let vhd = std::env::temp_dir().join(format!("fennec-hotplug-{}.vhdx", std::process::id()));
        let diskpart = |script: &str| {
            let f = std::env::temp_dir().join("fennec-diskpart.txt");
            std::fs::write(&f, script).unwrap();
            let out = std::process::Command::new("diskpart").arg("/s").arg(&f).output().unwrap();
            assert!(out.status.success(), "diskpart failed: {}", String::from_utf8_lossy(&out.stdout));
        };
        let db = std::env::temp_dir().join(format!("fennec-hotplug-{}.fnx", std::process::id()));
        let mut live = Live::start(&db, true);
        live.ready();
        let t = Instant::now();
        diskpart(&format!(
            "create vdisk file=\"{}\" maximum=256 type=expandable\nselect vdisk file=\"{}\"\nattach vdisk\ncreate partition primary\nformat fs=ntfs quick label=FNTEST\nassign letter=T\n",
            vhd.display(),
            vhd.display()
        ));
        std::fs::write(r"T:\hotplug-marker.txt", b"x").unwrap();
        live.count("hotplug-marker.txt", &|n| n == 1, Duration::from_secs(30));
        eprintln!("arrival: T: attached and searchable after {:?}", t.elapsed());
        std::fs::write(r"T:\hotplug-live.txt", b"y").unwrap();
        live.count("hotplug-live.txt", &|n| n == 1, Duration::from_secs(5));
        let offline = |live: &Live| {
            let s = live.shared.snapshot();
            (0..s.len() as u32)
                .map(fnc_core::EntryId)
                .find(|&i| s.is_live(i) && s.name_string(i) == "hotplug-marker.txt")
                .map(|i| s.flags(i).contains(fnc_core::EntryFlags::OFFLINE))
        };
        diskpart(&format!("select vdisk file=\"{}\"\ndetach vdisk\n", vhd.display()));
        let t = Instant::now();
        while offline(&live) != Some(true) {
            assert!(t.elapsed() < Duration::from_secs(10), "entries never went offline");
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("removal: entries offline after {:?}", t.elapsed());
        diskpart(&format!("select vdisk file=\"{}\"\nattach vdisk\n", vhd.display()));
        let t = Instant::now();
        while offline(&live) != Some(false) {
            assert!(t.elapsed() < Duration::from_secs(30), "entries never came back online");
            std::thread::sleep(Duration::from_millis(100));
        }
        std::fs::write(r"T:\hotplug-after.txt", b"z").unwrap();
        live.count("hotplug-after.txt", &|n| n == 1, Duration::from_secs(5));
        eprintln!("re-arrival: online again after {:?}; live changes work", t.elapsed());
        diskpart(&format!("select vdisk file=\"{}\"\ndetach vdisk\n", vhd.display()));
        live.stop();
        let _ = std::fs::remove_file(&vhd);
        let _ = std::fs::remove_file(&db);
    }

    /// IDX-02 (pipeline part): a change delivered by a watcher becomes visible in the
    /// current search well within 1 s (apply + debounce + re-search).
    #[test]
    fn idx02_change_to_visible_result_latency() {
        let mut store = IndexStore::default();
        let vol = store.add_volume(fnc_index::VolumeInfo::new(fnc_index::VolumeKind::Ntfs, "C:\\"), Some(5)).unwrap();
        let mut b = store.bulk_load(vol);
        let name: Vec<u16> = "t".encode_utf16().collect();
        b.push(10, 5, &name, &Meta::attrs(fnc_core::flags::attr::DIRECTORY)).unwrap();
        b.finish().unwrap();
        let shared = SharedIndex::new(store);
        let (tx, rx) = crossbeam_channel::unbounded();
        let engine = fnc_exec::SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
        let req = |id| fnc_exec::SearchRequest {
            id,
            text: "a.txt".into(),
            filter: None,
            sort: fnc_exec::SortSpec::default(),
            options: Arc::new(fnc_exec::SearchOptions::default()),
        };
        engine.search(req(1));
        let fnc_exec::EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(o.results.len(), 0);

        let handle = engine.handle();
        let refresher = Refresher::spawn(Arc::new(move || handle.refresh()));
        let mut sink = WatchSink { shared: shared.clone(), refresher, vol };
        let t0 = Instant::now();
        sink.changes(vec![Change::Create {
            volume: vol,
            frn: 11,
            parent: EntryRef::Frn(vol, 10),
            name: "a.txt".encode_utf16().collect(),
            meta: Meta::attrs(fnc_core::flags::attr::ARCHIVE),
        }]);
        let fnc_exec::EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let latency = t0.elapsed();
        assert_eq!(o.results.len(), 1);
        assert!(latency < Duration::from_secs(1), "latency {latency:?}");
        eprintln!("IDX-02 change→visible latency: {latency:?}");
    }

    #[test]
    fn builds_from_folders_and_file_lists() {
        crate::i18n::init("en");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/notes.txt"), b"hi").unwrap();
        let efu = dir.path().join("list.efu");
        std::fs::write(&efu, "Filename,Size,Date Modified,Date Created,Attributes\n\"X:\\a\\b.iso\",5,,,32\n").unwrap();
        let shared = SharedIndex::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let refreshed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = refreshed.clone();
        let cfg =
            IndexerConfig { folders: vec![dir.path().join("docs")], file_lists: vec![efu], ..IndexerConfig::default() };
        let _ix = Indexer::spawn(
            cfg,
            shared.clone(),
            Arc::new(move || {
                r.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
            tx,
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(IndexEvent::Status(IndexStatus::Ready { entries, .. })) =
                rx.recv_timeout(Duration::from_millis(100))
            {
                assert_eq!(entries, 5, "X:, a, b.iso from the list; the docs root and notes.txt");
                break;
            }
            assert!(Instant::now() < deadline, "indexer did not finish");
        }
        assert!(refreshed.load(std::sync::atomic::Ordering::Relaxed) >= 1);
        let snap = shared.snapshot();
        assert!((0..snap.len() as u32).map(EntryId).any(|i| snap.name_string(i) == "notes.txt"));
        assert!((0..snap.len() as u32).map(EntryId).any(|i| snap.name_string(i) == "b.iso"));
    }
}

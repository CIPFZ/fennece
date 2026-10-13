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
use fnc_index::{BulkLoader, Change, ChangeBatch, EntryRef, IndexOptions, IndexStore, Meta, SharedIndex};
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
}

type Callback = Arc<dyn Fn() + Send + Sync>;

/// Handle to the indexing threads.
pub struct Indexer {
    stop: StopFlag,
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
        let s = stop.clone();
        let spawn = std::thread::Builder::new()
            .name("fennec-indexer".into())
            .spawn(move || run(cfg, shared, refresh, events, notify, s));
        if let Err(e) = spawn {
            tracing::error!(error = %e, "cannot start indexer");
        }
        Self { stop }
    }

    /// Stops all indexing threads (they exit at their next check).
    pub fn stop(&self) {
        self.stop.stop();
    }

    /// An indexer that does nothing (the index is provided by the caller).
    #[cfg(test)]
    pub fn idle() -> Self {
        Self { stop: StopFlag::new() }
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
            match fnc_ntfs::NtfsSource::open(d, opts) {
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

fn run(
    cfg: IndexerConfig,
    shared: SharedIndex,
    refresh: Callback,
    events: Sender<IndexEvent>,
    notify: Callback,
    stop: StopFlag,
) {
    let emit = Emitter { tx: events, notify };
    let t0 = Instant::now();
    emit.status(IndexStatus::Starting);
    let mut store = IndexStore::new(IndexOptions { date_accessed: cfg.date_accessed });

    for efu in &cfg.file_lists {
        emit.status(IndexStatus::Building { volume: efu.display().to_string(), done: 0 });
        if let Err(e) = fnc_index::efu::load_efu_file(&mut store, efu) {
            emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())]));
        }
    }

    let mut sources = collect_sources(&cfg, &emit);
    let mut volumes: Vec<(VolumeId, Box<dyn VolumeSource>)> = Vec::new();
    for mut src in sources.drain(..) {
        if stop.is_stopped() {
            return;
        }
        let info = src.info();
        let name = info.display_name.clone();
        let vol = match store.add_volume(info, Some(src.root_frn())) {
            Ok(v) => v,
            Err(e) => {
                emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())]));
                continue;
            }
        };
        let t = Instant::now();
        let mut sink = LoadSink { bulk: store.bulk_load(vol), volume: name.clone(), emit: &emit, last: Instant::now() };
        let result = src.enumerate(&mut sink, &stop);
        let stats = sink.bulk.finish();
        match (result, stats) {
            (Ok(()), Ok(st)) => {
                tracing::info!(volume = %name, entries = st.entries, orphans = st.orphans, elapsed = ?t.elapsed(), "volume indexed")
            }
            (Err(SourceError::Stopped), _) => return,
            (Err(e), _) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
            (_, Err(e)) => emit.message(crate::i18n::t_args("index-error", &[("message", e.to_string().into())])),
        }
        if let Some((jid, usn)) = src.journal_position() {
            store.set_journal_position(vol, jid, usn);
        }
        volumes.push((vol, src));
    }
    store.shrink_to_fit();
    store.rebuild_name_order();
    let entries = store.live_count();
    shared.replace(store);
    refresh();
    let seconds = t0.elapsed().as_secs_f64();
    tracing::info!(entries, seconds, "index published");

    let refresher = Refresher::spawn(refresh);
    let needs_meta: Vec<VolumeId> = volumes.iter().filter(|(_, s)| !s.enumerates_metadata()).map(|(v, _)| *v).collect();
    if needs_meta.is_empty() {
        emit.status(IndexStatus::Ready { entries, seconds });
    }
    let emit = Arc::new(emit);
    let total_meta = needs_meta.len();
    let done_meta = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Reports "ready" once the last volume's back-fill finished.
    let meta_finished = {
        let (shared, emit, done_meta) = (shared.clone(), emit.clone(), done_meta.clone());
        Arc::new(move || {
            let finished = done_meta.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if finished == total_meta {
                let (entries, heap) = {
                    let s = shared.snapshot();
                    (s.live_count(), s.heap_bytes())
                };
                tracing::info!(entries, heap_mib = heap / (1 << 20), "index ready");
                emit.status(IndexStatus::Ready { entries, seconds: t0.elapsed().as_secs_f64() });
            }
        })
    };
    for (vol, mut src) in volumes {
        let shared = shared.clone();
        let stop = stop.clone();
        let emit = emit.clone();
        let refresher = refresher.clone();
        let wants_meta = needs_meta.contains(&vol);
        // Back-fill concurrently with journal monitoring when the source allows it, so
        // the journal is read from the start (no risk of it wrapping during a long
        // back-fill).
        // SPEC-GAP: a folder listed by the back-fill after a USN-triggered refresh of one
        // of its files can overwrite that refresh with slightly older size/dates until
        // the file changes again; M2 tags metadata with the index generation.
        let fetcher = if wants_meta { src.metadata_fetcher() } else { None };
        let inline_meta = wants_meta && fetcher.is_none();
        if let Some(fetcher) = fetcher {
            let (shared, stop, emit, refresher, done) =
                (shared.clone(), stop.clone(), emit.clone(), refresher.clone(), meta_finished.clone());
            let r = std::thread::Builder::new().name(format!("fennec-meta-{}", vol.0)).spawn(move || {
                fill_metadata(&|d, o, s| fetcher.fetch(d, o, s), vol, &shared, &stop, &emit, &refresher);
                done();
            });
            if let Err(e) = r {
                tracing::error!(error = %e, "cannot start metadata thread");
            }
        }
        let done = meta_finished.clone();
        let r = std::thread::Builder::new().name(format!("fennec-volume-{}", vol.0)).spawn(move || {
            if inline_meta {
                fill_metadata(&|d, o, s| src.fetch_metadata(d, o, s), vol, &shared, &stop, &emit, &refresher);
                done();
            }
            let name = src.info().display_name;
            let mut sink = WatchSink { shared: shared.clone(), refresher, vol };
            match src.watch(vol, &mut sink, &stop) {
                Ok(()) | Err(SourceError::Stopped | SourceError::Unsupported(_)) => {}
                Err(SourceError::Offline(_)) => {
                    shared.write().set_volume_online(vol, false);
                    sink.refresher.poke();
                }
                Err(e) => emit.message(crate::i18n::t_args(
                    "index-watch-error",
                    &[("volume", name.into()), ("message", e.to_string().into())],
                )),
            }
        });
        if let Err(e) = r {
            tracing::error!(error = %e, "cannot start volume thread");
        }
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
) {
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
            return;
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
            return;
        }
        let mut batch = std::mem::take(&mut collector.out);
        while !batch.is_empty() {
            let rest = if batch.len() > MAX_BATCH { batch.split_off(MAX_BATCH) } else { Vec::new() };
            shared.write().apply(batch);
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

//! Portable folder-index source (SPEC ch.1 §3): parallel directory walk, change
//! notification (`notify`: ReadDirectoryChangesW / inotify / FSEvents), periodic and
//! overflow rescans merged per folder (`PathChange::SyncDir`, ids stay stable), and an
//! exclusion hook.
//!
//! Entries get synthetic, sequential FRNs during the walk (root = 1); later entries are
//! numbered by the index. Changes are addressed by path relative to the root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fnc_core::flags::attr;
use fnc_core::{EntryId, FileTime, VolumeId};
use fnc_index::{Change, ChangeBatch, Meta, PathChange, RelPath, VolumeInfo, VolumeKind};

use crate::source::{ChangeSink, EntrySink, MetaSink, RawEntry, SourceError, StopFlag, VolumeSource};

/// Decides whether an entry is left out of the index: `(full path, name, is_dir,
/// attributes)`. Excluded folders are not descended into.
pub type ExcludeFn = Arc<dyn Fn(&Path, &str, bool, u32) -> bool + Send + Sync>;

/// Folder index options.
#[derive(Clone)]
pub struct FolderOptions {
    /// Full rescan interval (zero = only on start and after notification overflow).
    pub rescan_interval: Duration,
    /// Watch for changes.
    pub watch: bool,
    /// Parallel directory readers (network shares: 4).
    pub threads: usize,
    /// Exclusion rules.
    pub exclude: Option<ExcludeFn>,
    /// Incremented to request a rescan of every folder index (`-update`).
    pub rescan_epoch: Option<Arc<AtomicU64>>,
}

impl Default for FolderOptions {
    fn default() -> Self {
        Self { rescan_interval: Duration::from_secs(3600), watch: true, threads: 8, exclude: None, rescan_epoch: None }
    }
}

impl std::fmt::Debug for FolderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderOptions")
            .field("rescan_interval", &self.rescan_interval)
            .field("watch", &self.watch)
            .field("threads", &self.threads)
            .field("exclude", &self.exclude.is_some())
            .finish()
    }
}

/// Root FRN used by folder sources.
pub const FOLDER_ROOT_FRN: u64 = 1;

/// Folder index for one root.
pub struct FolderSource {
    root: PathBuf,
    info: VolumeInfo,
    opts: FolderOptions,
    /// Rescan at the start of the next `watch` (after being offline).
    stale: bool,
    /// `enumerate` ran and the watch has not caught up with it yet.
    walked: bool,
    /// Last rescan request seen (see [`FolderOptions::rescan_epoch`]).
    seen_epoch: u64,
}

impl FolderSource {
    /// Source for `root` (must be an existing directory) with default options.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_options(root, FolderOptions::default())
    }

    /// Source for `root` with `opts`.
    pub fn with_options(root: impl Into<PathBuf>, mut opts: FolderOptions) -> Self {
        let root = root.into();
        let display = root.to_string_lossy().into_owned();
        let mut info = VolumeInfo::new(VolumeKind::Folder, display);
        info.separator = std::path::MAIN_SEPARATOR;
        info.fs_name = "folder".into();
        if is_network_path(&root) {
            opts.threads = opts.threads.min(4);
        }
        let seen_epoch = opts.rescan_epoch.as_ref().map_or(0, |e| e.load(Ordering::Acquire));
        Self { root, info, opts, stale: false, walked: false, seen_epoch }
    }

    fn excluded(&self, path: &Path, name: &std::ffi::OsStr, md: &std::fs::Metadata) -> bool {
        let Some(ex) = &self.opts.exclude else { return false };
        let attrs = meta_from_fs(name, md).attributes.unwrap_or(0);
        ex(path, &name.to_string_lossy(), md.is_dir(), attrs)
    }

    /// Path components of `p` below the root (`None` outside it).
    fn rel(&self, p: &Path) -> Option<RelPath> {
        let r = p.strip_prefix(&self.root).ok()?;
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for c in r.components() {
            if let std::path::Component::Normal(n) = c {
                os_name_to_utf16(n, &mut buf);
                out.push(buf.clone().into_boxed_slice());
            }
        }
        Some(out)
    }

    /// Lists `dir` (not recursing) as `(name, meta, is_dir)`, applying exclusions.
    fn list(&self, dir: &Path) -> std::io::Result<Vec<(std::ffi::OsString, Meta, bool)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
            let name = entry.file_name();
            if self.excluded(&path, &name, &md) {
                continue;
            }
            let meta = meta_from_fs(&name, &md);
            // Directory symlinks/junctions are listed but not followed (no cycles).
            let is_dir = md.is_dir() && !md.file_type().is_symlink();
            out.push((name, meta, is_dir));
        }
        Ok(out)
    }

    /// Rescan of the folder `dir` and everything below: one `SyncDir` per folder,
    /// parents first.
    fn rescan(&self, dir: &Path, stop: &StopFlag) -> Result<ChangeBatch, SourceError> {
        let vol = VolumeId(0); // replaced by the caller
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        let mut name = Vec::new();
        while let Some(d) = stack.pop() {
            if stop.is_stopped() {
                return Err(SourceError::Stopped);
            }
            let Some(rel) = self.rel(&d) else { continue };
            let items = match self.list(&d) {
                Ok(v) => v,
                Err(e) if d == self.root => return Err(offline_or_io(&self.root, e)),
                Err(_) => continue,
            };
            let mut children = Vec::with_capacity(items.len());
            for (n, meta, is_dir) in items {
                os_name_to_utf16(&n, &mut name);
                children.push((name.clone().into_boxed_slice(), meta));
                if is_dir {
                    stack.push(d.join(&n));
                }
            }
            out.push(Change::Path { volume: vol, change: PathChange::SyncDir { dir: rel, children } });
        }
        Ok(out)
    }

    /// Root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Converts an OS file name to UTF-16 (lossless on Windows; lossy for non-UTF-8 names
/// elsewhere).
// SPEC-GAP: non-UTF-8 names on Unix are stored lossily (U+FFFD).
pub fn os_name_to_utf16(name: &std::ffi::OsStr, out: &mut Vec<u16>) {
    out.clear();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        out.extend(name.encode_wide());
    }
    #[cfg(not(windows))]
    {
        out.extend(name.to_string_lossy().encode_utf16());
    }
}

/// Maps `std::fs::Metadata` to index metadata.
pub fn meta_from_fs(name: &std::ffi::OsStr, md: &std::fs::Metadata) -> Meta {
    #[cfg(windows)]
    let mut attributes = {
        use std::os::windows::fs::MetadataExt;
        let _ = name;
        md.file_attributes()
    };
    #[cfg(not(windows))]
    let mut attributes = {
        let mut a = if md.is_dir() { attr::DIRECTORY } else { attr::ARCHIVE };
        if name.to_string_lossy().starts_with('.') {
            a |= attr::HIDDEN;
        }
        if md.permissions().readonly() {
            a |= attr::READONLY;
        }
        if md.file_type().is_symlink() {
            a |= attr::REPARSE_POINT;
        }
        a
    };
    if md.is_dir() {
        attributes |= attr::DIRECTORY;
    }
    let ft = |t: std::io::Result<std::time::SystemTime>| t.ok().map(FileTime::from_system_time);
    Meta {
        attributes: Some(attributes),
        size: (!md.is_dir()).then_some(md.len()),
        date_modified: ft(md.modified()),
        date_created: ft(md.created()),
        date_accessed: ft(md.accessed()),
    }
}

impl FolderSource {
    /// Turns a notification into paths to refresh.
    fn classify(
        &self,
        ev: notify::Event,
        pending_from: &mut HashMap<usize, PathBuf>,
        dirty: &mut Vec<PathBuf>,
        rescan_all: &mut bool,
    ) {
        use notify::EventKind;
        use notify::event::{ModifyKind, RenameMode};
        if ev.need_rescan() {
            *rescan_all = true;
            return;
        }
        match ev.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                if let (Some(t), Some(p)) = (ev.attrs.tracker(), ev.paths.first()) {
                    pending_from.insert(t, p.clone());
                } else {
                    dirty.extend(ev.paths);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                if let Some(from) = ev.attrs.tracker().and_then(|t| pending_from.remove(&t)) {
                    dirty.push(from);
                }
                dirty.extend(ev.paths);
            }
            EventKind::Access(_) => {}
            _ => dirty.extend(ev.paths),
        }
    }

    /// Changes that bring `p` up to date: removed if gone, upserted if present, and a
    /// folder (new, renamed in, or changed) is synced with its subtree.
    fn refresh_path(&self, p: &Path, stop: &StopFlag) -> Result<ChangeBatch, SourceError> {
        let Some(rel) = self.rel(p) else { return Ok(Vec::new()) };
        if rel.is_empty() {
            return self.rescan(&self.root, stop);
        }
        let vol = VolumeId(0);
        match std::fs::symlink_metadata(p) {
            Err(_) => Ok(vec![Change::Path { volume: vol, change: PathChange::Remove { path: rel } }]),
            Ok(md) => {
                let name = p.file_name().unwrap_or_default();
                if self.excluded(p, name, &md) {
                    return Ok(vec![Change::Path { volume: vol, change: PathChange::Remove { path: rel } }]);
                }
                let mut out = vec![Change::Path {
                    volume: vol,
                    change: PathChange::Upsert { path: rel, meta: meta_from_fs(name, &md) },
                }];
                if md.is_dir() && !md.file_type().is_symlink() {
                    out.extend(self.rescan(p, stop)?);
                }
                Ok(out)
            }
        }
    }
}

fn offline_or_io(root: &Path, e: std::io::Error) -> SourceError {
    if root.exists() {
        SourceError::io(format!("read {}", root.display()), e)
    } else {
        SourceError::Offline(root.display().to_string())
    }
}

/// `\\server\share` paths and mapped network drives (Windows).
fn is_network_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("\\\\") && !s.starts_with("\\\\?\\") || s.starts_with("//")
}

impl VolumeSource for FolderSource {
    fn info(&self) -> VolumeInfo {
        self.info.clone()
    }

    fn root_frn(&self) -> u64 {
        FOLDER_ROOT_FRN
    }

    fn enumerate(&mut self, sink: &mut dyn EntrySink, stop: &StopFlag) -> Result<(), SourceError> {
        if let Err(e) = std::fs::read_dir(&self.root) {
            return Err(SourceError::io(format!("read {}", self.root.display()), e));
        }
        // Workers list folders in parallel; this thread feeds the sink.
        type Item = (u64, u64, Vec<u16>, Meta);
        let (tx, rx) = crossbeam_channel::bounded::<Vec<Item>>(64);
        // Path-derived ids (stable across walks, see `fnc_index::path_frn`). Collisions
        // within a folder take the next probe; folder ids are also checked against every
        // folder id issued (children hang off them), files are de-duplicated by the index.
        let folders = std::sync::Mutex::new(std::collections::HashSet::<u64>::from([FOLDER_ROOT_FRN]));
        let queue = crossbeam_channel::unbounded::<(PathBuf, u64)>();
        let pending = AtomicU64::new(1);
        queue.0.send((self.root.clone(), FOLDER_ROOT_FRN)).ok();
        let this = &*self;
        let mut result = Ok(());
        std::thread::scope(|scope| {
            for _ in 0..this.opts.threads.max(1) {
                let (qtx, qrx) = (queue.0.clone(), queue.1.clone());
                let (tx, pending, folders) = (tx.clone(), &pending, &folders);
                scope.spawn(move || {
                    let mut name = Vec::with_capacity(256);
                    while pending.load(Ordering::Acquire) > 0 {
                        let Ok((dir, frn)) = qrx.recv_timeout(Duration::from_millis(20)) else { continue };
                        if stop.is_stopped() {
                            pending.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let mut batch = Vec::new();
                        match this.list(&dir) {
                            Ok(items) => {
                                let mut fold = Vec::with_capacity(256);
                                let mut here: Vec<u64> = Vec::with_capacity(items.len());
                                for (n, meta, is_dir) in items {
                                    os_name_to_utf16(&n, &mut name);
                                    fold.clear();
                                    fnc_core::fold::fold_utf16_into(&name, &mut fold);
                                    // Folded names are unique within a folder, so only
                                    // a hash collision probes (order-independent in practice).
                                    let mut id = fnc_index::path_frn(frn, &fold);
                                    while here.contains(&id) {
                                        id = fnc_index::path_frn_probe(id);
                                    }
                                    if is_dir {
                                        let mut f = folders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                        while here.contains(&id) || !f.insert(id) {
                                            id = fnc_index::path_frn_probe(id);
                                        }
                                    }
                                    if here.len() < 64 {
                                        here.push(id);
                                    }
                                    if is_dir {
                                        pending.fetch_add(1, Ordering::AcqRel);
                                        let _ = qtx.send((dir.join(&n), id));
                                    }
                                    batch.push((id, frn, name.clone(), meta));
                                }
                            }
                            Err(e) => tracing::debug!(dir = %dir.display(), error = %e, "skipping unreadable folder"),
                        }
                        if !batch.is_empty() {
                            let _ = tx.send(batch);
                        }
                        pending.fetch_sub(1, Ordering::AcqRel);
                    }
                });
            }
            drop(tx);
            let mut done = 0u64;
            for batch in rx {
                if result.is_err() {
                    continue;
                }
                for (frn, parent, name, meta) in batch {
                    if let Err(e) = sink.entry(&RawEntry { frn, parent_frn: parent, name: &name, meta }) {
                        result = Err(e);
                        break;
                    }
                    done += 1;
                    if done.is_multiple_of(65_536) {
                        sink.progress(done, 0);
                    }
                }
            }
            sink.progress(done, done);
        });
        if stop.is_stopped() {
            return Err(SourceError::Stopped);
        }
        self.walked = true;
        result
    }

    fn watch(&mut self, volume: VolumeId, sink: &mut dyn ChangeSink, stop: &StopFlag) -> Result<(), SourceError> {
        use notify::Watcher;
        let (tx, rx) = crossbeam_channel::unbounded::<notify::Result<notify::Event>>();
        let mut watcher = None;
        if self.opts.watch {
            match notify::recommended_watcher(move |e| {
                let _ = tx.send(e);
            }) {
                Ok(mut w) => match w.watch(&self.root, notify::RecursiveMode::Recursive) {
                    Ok(()) => watcher = Some(w),
                    Err(e) => {
                        tracing::info!(root = %self.root.display(), error = %e, "no change notification; rescanning")
                    }
                },
                Err(e) => tracing::info!(error = %e, "no change notification; rescanning"),
            }
        }
        let interval = if watcher.is_none() && self.opts.rescan_interval.is_zero() {
            // Without notifications something must keep the index current.
            Duration::from_secs(3600)
        } else {
            self.opts.rescan_interval
        };
        let mut next_rescan = (!interval.is_zero()).then(|| Instant::now() + interval);
        let with_vol = |mut b: ChangeBatch| {
            for c in &mut b {
                if let Change::Path { volume: v, .. } = c {
                    *v = volume;
                }
            }
            b
        };
        let mut pending_from: HashMap<usize, PathBuf> = HashMap::new();
        let mut dirty: Vec<PathBuf> = Vec::new();
        // Changes between the walk and the watch registration were not seen: one
        // catch-up pass (SyncDir diff; unchanged folders cost a listing, nothing else).
        let mut rescan_all = std::mem::take(&mut self.stale) || (watcher.is_some() && self.walked);
        self.walked = false;
        while !stop.is_stopped() {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => {
                    let mut collect = |ev: notify::Result<notify::Event>| match ev {
                        Ok(ev) => self.classify(ev, &mut pending_from, &mut dirty, &mut rescan_all),
                        Err(e) => {
                            tracing::debug!(error = %e, "watch error; rescanning");
                            rescan_all = true;
                        }
                    };
                    collect(ev);
                    // Coalesce a burst (100 ms quiet period, at most 1 s).
                    let t = Instant::now();
                    while t.elapsed() < Duration::from_secs(1) {
                        match rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(ev) => collect(ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
            }
            // A "rename from" without its "to" moved out of the tree.
            for (_, from) in pending_from.drain() {
                dirty.push(from);
            }
            if !self.root.exists() {
                return Err(SourceError::Offline(self.root.display().to_string()));
            }
            if let Some(e) = &self.opts.rescan_epoch {
                let now = e.load(Ordering::Acquire);
                if now != self.seen_epoch {
                    self.seen_epoch = now;
                    rescan_all = true;
                }
            }
            if next_rescan.is_some_and(|t| Instant::now() >= t) {
                rescan_all = true;
                next_rescan = Some(Instant::now() + interval);
            }
            let batch = if rescan_all {
                rescan_all = false;
                dirty.clear();
                tracing::info!(root = %self.root.display(), "rescanning folder index");
                self.rescan(&self.root, stop)?
            } else {
                let mut b = Vec::new();
                dirty.sort();
                dirty.dedup();
                for p in std::mem::take(&mut dirty) {
                    b.extend(self.refresh_path(&p, stop)?);
                }
                b
            };
            if !batch.is_empty() {
                if !sink.changes(with_vol(batch)) {
                    return Ok(());
                }
            } else {
                sink.idle();
            }
        }
        drop(watcher);
        Err(SourceError::Stopped)
    }

    fn fetch_metadata(
        &self,
        _dirs: &[(EntryId, String)],
        _out: &mut dyn MetaSink,
        _stop: &StopFlag,
    ) -> Result<(), SourceError> {
        Ok(())
    }

    fn enumerates_metadata(&self) -> bool {
        true
    }

    fn reconnect(&mut self) -> bool {
        self.stale = std::fs::read_dir(&self.root).is_ok();
        self.stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Collect(Vec<(u64, u64, String, Meta)>);

    impl EntrySink for Collect {
        fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
            self.0.push((e.frn, e.parent_frn, String::from_utf16_lossy(e.name), e.meta));
            Ok(())
        }
    }

    #[test]
    fn walks_tree_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/file.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("top.md"), b"x").unwrap();
        let mut src = FolderSource::new(dir.path());
        let mut c = Collect(Vec::new());
        src.enumerate(&mut c, &StopFlag::new()).unwrap();
        assert_eq!(c.0.len(), 4);
        let file = c.0.iter().find(|r| r.2 == "file.txt").unwrap();
        assert_eq!(file.3.size, Some(5));
        assert!(file.3.date_modified.is_some());
        let b = c.0.iter().find(|r| r.2 == "b").unwrap();
        assert_eq!(file.1, b.0, "file's parent is b");
        assert!(b.3.attributes.unwrap() & attr::DIRECTORY != 0);
        assert_eq!(c.0.iter().find(|r| r.2 == "a").unwrap().1, FOLDER_ROOT_FRN);
    }

    #[test]
    fn file_ids_are_stable_across_walks_and_live_creates() {
        let dir = tempfile::tempdir().unwrap();
        for d in 0..20 {
            std::fs::create_dir_all(dir.path().join(format!("d{d}/sub"))).unwrap();
            for f in 0..30 {
                std::fs::write(dir.path().join(format!("d{d}/sub/f{f}.txt")), b"x").unwrap();
            }
        }
        let walk = || {
            let mut src = FolderSource::new(dir.path());
            let mut c = Collect(Vec::new());
            src.enumerate(&mut c, &StopFlag::new()).unwrap();
            let mut v: Vec<(u64, u64, String)> = c.0.into_iter().map(|r| (r.0, r.1, r.2)).collect();
            v.sort();
            v
        };
        let a = walk();
        assert_eq!(a.len(), 20 * 32);
        assert_eq!(a, walk(), "same ids on every walk");
        let ids: std::collections::HashSet<u64> = a.iter().map(|r| r.0).collect();
        assert_eq!(ids.len(), a.len(), "unique");
        // A file created later (watcher path) gets the id a fresh walk gives it.
        let mut store = fnc_index::IndexStore::default();
        let src = FolderSource::new(dir.path());
        let vol = store.add_volume(src.info(), Some(src.root_frn())).unwrap();
        let mut b = store.bulk_load(vol);
        for (frn, parent, name) in &a {
            let w: Vec<u16> = name.encode_utf16().collect();
            let attrs = if name.contains('.') { 32 } else { attr::DIRECTORY };
            b.push(*frn, *parent, &w, &Meta::attrs(attrs)).unwrap();
        }
        b.finish().unwrap();
        std::fs::write(dir.path().join("d3/sub/late.txt"), b"y").unwrap();
        let rel: Vec<Box<[u16]>> = ["d3", "sub", "late.txt"]
            .iter()
            .map(|c| c.encode_utf16().collect::<Vec<u16>>().into_boxed_slice())
            .collect();
        store.apply(vec![fnc_index::Change::Path {
            volume: vol,
            change: fnc_index::PathChange::Upsert { path: rel.clone(), meta: Meta::attrs(32) },
        }]);
        let late = store.resolve_path(vol, &rel).unwrap();
        let fresh = walk();
        let expected = fresh.iter().find(|r| r.2 == "late.txt").unwrap().0;
        assert_eq!(store.frn(late), expected);
    }

    #[test]
    fn into_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x/y.txt"), b"").unwrap();
        let mut src = FolderSource::new(dir.path());
        let mut store = fnc_index::IndexStore::default();
        let vol = store.add_volume(src.info(), Some(src.root_frn())).unwrap();
        struct Sink<'a>(fnc_index::BulkLoader<'a>);
        impl EntrySink for Sink<'_> {
            fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
                self.0
                    .push(e.frn, e.parent_frn, e.name, &e.meta)
                    .map(|_| ())
                    .map_err(|e| SourceError::io("push", std::io::Error::other(e)))
            }
        }
        let mut sink = Sink(store.bulk_load(vol));
        src.enumerate(&mut sink, &StopFlag::new()).unwrap();
        sink.0.finish().unwrap();
        let y = (0..store.len() as u32).map(EntryId).find(|&i| store.name_string(i) == "y.txt").unwrap();
        let mut p = String::new();
        store.full_path(y, &mut p);
        let expected = dir.path().join("x").join("y.txt");
        assert_eq!(p, expected.to_string_lossy());
    }

    struct Applier(fnc_index::SharedIndex, crossbeam_channel::Sender<()>);

    impl ChangeSink for Applier {
        fn changes(&mut self, batch: ChangeBatch) -> bool {
            self.0.write().apply(batch);
            let _ = self.1.send(());
            true
        }
    }

    fn find(shared: &fnc_index::SharedIndex, name: &str) -> Option<String> {
        let s = shared.snapshot();
        let id = (0..s.len() as u32).map(EntryId).find(|&i| s.is_live(i) && s.name_string(i) == name)?;
        let mut p = String::new();
        s.full_path(id, &mut p);
        Some(p)
    }

    fn wait(shared: &fnc_index::SharedIndex, ok: impl Fn(&fnc_index::SharedIndex) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if ok(shared) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn watches_creates_renames_deletes_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/one.txt"), b"1").unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("node_modules/x/big.js"), b"").unwrap();
        let ex: ExcludeFn = Arc::new(|_p, name, is_dir, _a| is_dir && name == "node_modules");
        let opts = FolderOptions { exclude: Some(ex), rescan_interval: Duration::ZERO, ..FolderOptions::default() };
        let mut src = FolderSource::with_options(&root, opts);
        let mut store = fnc_index::IndexStore::default();
        let vol = store.add_volume(src.info(), Some(src.root_frn())).unwrap();
        {
            struct Sink<'a>(fnc_index::BulkLoader<'a>);
            impl EntrySink for Sink<'_> {
                fn entry(&mut self, e: &RawEntry<'_>) -> Result<(), SourceError> {
                    self.0
                        .push(e.frn, e.parent_frn, e.name, &e.meta)
                        .map(|_| ())
                        .map_err(|e| SourceError::io("push", std::io::Error::other(e)))
                }
            }
            let mut sink = Sink(store.bulk_load(vol));
            src.enumerate(&mut sink, &StopFlag::new()).unwrap();
            sink.0.finish().unwrap();
        }
        let shared = fnc_index::SharedIndex::new(store);
        assert!(find(&shared, "one.txt").is_some());
        assert!(find(&shared, "node_modules").is_none(), "excluded folder");
        assert!(find(&shared, "big.js").is_none());
        let stop = StopFlag::new();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let (sh, st) = (shared.clone(), stop.clone());
        let h = std::thread::spawn(move || {
            let mut sink = Applier(sh, tx);
            src.watch(vol, &mut sink, &st)
        });
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(root.join("a/two.txt"), b"22").unwrap();
        assert!(wait(&shared, |s| find(s, "two.txt").is_some()), "create seen");
        std::fs::rename(root.join("a"), root.join("b")).unwrap();
        let expected = root.join("b").join("one.txt").to_string_lossy().into_owned();
        assert!(wait(&shared, |s| find(s, "one.txt").as_deref() == Some(expected.as_str())), "folder rename seen");
        // A folder moved in from outside arrives with its content.
        let outside = tempfile::tempdir_in(root.parent().unwrap()).unwrap();
        std::fs::create_dir_all(outside.path().join("pkg/sub")).unwrap();
        std::fs::write(outside.path().join("pkg/sub/deep.rs"), b"").unwrap();
        std::fs::rename(outside.path().join("pkg"), root.join("pkg")).unwrap();
        assert!(wait(&shared, |s| find(s, "deep.rs").is_some()), "moved-in subtree seen");
        std::fs::remove_file(root.join("b/two.txt")).unwrap();
        assert!(wait(&shared, |s| find(s, "two.txt").is_none()), "delete seen");
        std::fs::create_dir_all(root.join("b/node_modules")).unwrap();
        std::fs::write(root.join("b/after.txt"), b"").unwrap();
        assert!(wait(&shared, |s| find(s, "after.txt").is_some()));
        assert!(find(&shared, "node_modules").is_none(), "new excluded folder stays out");
        stop.stop();
        assert!(matches!(h.join().unwrap(), Err(SourceError::Stopped)));
    }
}

/// Walk speed of a large tree: `FENNEC_WALK_DIR=<dir> cargo test --release -p fnc-platform walk_speed -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore = "needs FENNEC_WALK_DIR"]
fn walk_speed() {
    struct Count(usize);
    impl EntrySink for Count {
        fn entry(&mut self, _: &RawEntry<'_>) -> Result<(), SourceError> {
            self.0 += 1;
            Ok(())
        }
    }
    let dir = std::env::var("FENNEC_WALK_DIR").unwrap_or_else(|_| "/usr".into());
    for _ in 0..3 {
        let mut src = FolderSource::new(Path::new(&dir));
        let mut c = Count(0);
        let t = std::time::Instant::now();
        src.enumerate(&mut c, &StopFlag::new()).unwrap();
        eprintln!("{} entries in {:?}", c.0, t.elapsed());
    }
}

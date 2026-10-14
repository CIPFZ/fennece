//! Cache of the values the file-reading result columns show and sort by (SPEC ch.3 §4.3:
//! link target, file signature, recycle bin original location), persisted as
//! `files.fnx`.
//!
//! Values are keyed by file identity like the [`crate::hash::HashCache`]: volume root,
//! FRN (or the folded full path on volumes without stable FRNs), size and date modified —
//! a changed file is read again, and the cache needs no remapping when the index is
//! compacted or rebuilt. An empty value means "read, nothing to show" (not a link target,
//! unknown signature, unreadable `$I` record); an absent value means "not read yet".

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fnc_core::EntryId;
use fnc_index::IndexStore;
use parking_lot::Mutex;
use xxhash_rust::xxh3::xxh3_64;

use crate::fnx::{Dec, Enc, FnxError, FnxResult, FnxWriter, segments};
use crate::hash::HashKey;

/// `files.fnx` magic.
pub const FILES_MAGIC: [u8; 4] = *b"FNXF";
/// `files.fnx` layout version.
pub const FILES_VERSION: u32 = 1;

/// A value read from a file for a result column.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileField {
    /// Target of a shortcut, internet shortcut, symbolic link or junction.
    LinkTarget,
    /// File signature (`fsi:` name).
    Signature,
    /// Original location of a recycle bin item.
    RecycleOrigin,
}

impl FileField {
    /// Every field.
    pub const ALL: [FileField; 3] = [FileField::LinkTarget, FileField::Signature, FileField::RecycleOrigin];

    /// Column / sort key name (`linktarget`, `filesignature`, `recyclebinoriginalpath`).
    pub fn name(self) -> &'static str {
        match self {
            FileField::LinkTarget => "linktarget",
            FileField::Signature => "filesignature",
            FileField::RecycleOrigin => "recyclebinoriginalpath",
        }
    }

    /// Looks up a column / sort key name (case-insensitive, `-` and `_` ignored).
    pub fn lookup(name: &str) -> Option<FileField> {
        let n = crate::schema::normalize(name);
        Self::ALL.into_iter().find(|f| f.name() == n)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Cache key of entry `id` (files and folders; `buf` is a reusable path buffer, no
/// allocation once it has grown). `None` if the entry's volume is unknown.
pub fn file_key(s: &IndexStore, id: EntryId, buf: &mut Vec<u8>) -> Option<HashKey> {
    let vol = s.volume_info(s.volume_of(id))?;
    let stable = matches!(vol.info.kind, fnc_index::VolumeKind::Ntfs | fnc_index::VolumeKind::Refs);
    let file = if stable {
        s.frn(id)
    } else {
        buf.clear();
        s.full_path_fold(id, buf);
        xxh3_64(buf)
    };
    let size = if s.is_dir(id) { u64::MAX } else { s.size(id).unwrap_or(u64::MAX) };
    Some(HashKey { volume: xxh3_64(vol.info.root_path.as_bytes()), file, size, dm: s.date_modified(id).0 })
}

type Values = [Option<Arc<str>>; 3];

/// Thread-safe cache of values read from files.
#[derive(Debug)]
pub struct FileValueCache {
    map: Mutex<HashMap<HashKey, Values>>,
    generation: AtomicU64,
    saved: AtomicU64,
    /// Entry limit; beyond it the cache is cleared (default 4 M).
    pub max_entries: usize,
}

impl Default for FileValueCache {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            saved: AtomicU64::new(0),
            max_entries: 4 << 20,
        }
    }
}

impl FileValueCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Incremented by every insertion.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Cached value (`Some("")`: read, nothing to show; `None`: not read).
    pub fn get(&self, key: &HashKey, f: FileField) -> Option<Arc<str>> {
        self.map.lock().get(key).and_then(|v| v[f.index()].clone())
    }

    /// [`FileValueCache::get`] waiting at most `timeout` for the lock (UI thread);
    /// `None` when the lock was busy, else `Some` of the cached value.
    pub fn try_get(&self, key: &HashKey, f: FileField, timeout: Duration) -> Option<Option<Arc<str>>> {
        let m = self.map.try_lock_for(timeout)?;
        Some(m.get(key).and_then(|v| v[f.index()].clone()))
    }

    /// `true` if `f` of `key` is cached.
    pub fn contains(&self, key: &HashKey, f: FileField) -> bool {
        self.map.lock().get(key).is_some_and(|v| v[f.index()].is_some())
    }

    /// Looks up many keys under one lock: appends `(tag, value)` for every cached
    /// non-empty value of `f` (sort keys; callers chunk the keys so the lock is short).
    pub fn lookup_many(&self, keys: &[(u32, HashKey)], f: FileField, out: &mut Vec<(u32, Arc<str>)>) {
        let m = self.map.lock();
        for (tag, k) in keys {
            if let Some(v) = m.get(k).and_then(|v| v[f.index()].as_ref()).filter(|v| !v.is_empty()) {
                out.push((*tag, v.clone()));
            }
        }
    }

    /// Of `keys`, how many lack at least one of their fields (under one lock).
    pub fn count_missing(&self, keys: &[(HashKey, u8)]) -> usize {
        let m = self.map.lock();
        keys.iter()
            .filter(|(k, fields)| {
                let v = m.get(k);
                FileField::ALL
                    .iter()
                    .any(|f| fields & (1 << f.index()) != 0 && v.is_none_or(|v| v[f.index()].is_none()))
            })
            .count()
    }

    /// Stores a value (`""`: read, nothing to show).
    pub fn insert(&self, key: HashKey, f: FileField, value: &str) {
        let mut m = self.map.lock();
        if m.len() >= self.max_entries.max(1) && !m.contains_key(&key) {
            m.clear();
        }
        m.entry(key).or_default()[f.index()] = Some(value.into());
        drop(m);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Serialises the cache.
    pub fn to_bytes(&self) -> Vec<u8> {
        let m = self.map.lock();
        let mut e = Enc::default();
        e.u32(m.len() as u32);
        for (k, v) in m.iter() {
            e.u64(k.volume);
            e.u64(k.file);
            e.u64(k.size);
            e.i64(k.dm);
            let mask: u8 = v.iter().enumerate().filter(|(_, x)| x.is_some()).fold(0, |a, (i, _)| a | (1 << i));
            e.u8(mask);
            for x in v.iter().flatten() {
                e.str(x);
            }
        }
        drop(m);
        let mut w = FnxWriter::new(FILES_MAGIC, FILES_VERSION);
        w.segment(*b"FVAL", &e.0);
        w.finish()
    }

    /// Decodes cache bytes.
    pub fn from_bytes(buf: &[u8]) -> FnxResult<Self> {
        let segs = segments(buf, FILES_MAGIC, FILES_VERSION, "file value cache")?;
        let seg =
            segs.iter().find(|(t, _)| t == b"FVAL").map(|(_, s)| *s).ok_or(FnxError::Corrupt("missing segment"))?;
        let mut d = Dec::new(seg);
        let n = d.count(33)?;
        let mut map = HashMap::with_capacity(n);
        for _ in 0..n {
            let key = HashKey { volume: d.u64()?, file: d.u64()?, size: d.u64()?, dm: d.i64()? };
            let mask = d.u8()?;
            let mut v: Values = Default::default();
            for f in FileField::ALL {
                if mask & (1 << f.index()) != 0 {
                    v[f.index()] = Some(d.str()?.into());
                }
            }
            map.insert(key, v);
        }
        Ok(Self { map: Mutex::new(map), ..Self::new() })
    }

    /// Loads `path`; a missing or unusable file gives an empty cache (logged).
    pub fn load_or_default(path: Option<&Path>) -> Self {
        let Some(path) = path.filter(|p| p.exists()) else { return Self::new() };
        match crate::fnx::read_file(path).and_then(|b| Self::from_bytes(&b)) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "files.fnx unusable; starting empty");
                Self::new()
            }
        }
    }

    /// Saves atomically to `path` if anything was inserted since the last save or load;
    /// returns whether it wrote.
    pub fn save_if_changed(&self, path: &Path) -> FnxResult<bool> {
        let g = self.generation();
        if g == self.saved.load(Ordering::Relaxed) {
            return Ok(false);
        }
        crate::fnx::write_atomic(path, &self.to_bytes())?;
        self.saved.store(g, Ordering::Relaxed);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_lookup_persist() {
        let c = FileValueCache::new();
        let k = HashKey { volume: 1, file: 2, size: 3, dm: 4 };
        assert_eq!(c.get(&k, FileField::Signature), None);
        c.insert(k, FileField::Signature, "png");
        c.insert(k, FileField::LinkTarget, "");
        assert_eq!(c.get(&k, FileField::Signature).as_deref(), Some("png"));
        assert_eq!(c.get(&k, FileField::LinkTarget).as_deref(), Some(""), "read, empty");
        assert!(!c.contains(&k, FileField::RecycleOrigin));
        assert_eq!(c.try_get(&k, FileField::Signature, Duration::from_millis(1)).unwrap().as_deref(), Some("png"));
        // Another size or date: another file version, not read.
        let changed = HashKey { size: 99, ..k };
        assert_eq!(c.get(&changed, FileField::Signature), None);
        let sig = 1 << FileField::Signature.index();
        let link = 1 << FileField::LinkTarget.index();
        let rec = 1 << FileField::RecycleOrigin.index();
        assert_eq!(c.count_missing(&[(k, sig | link), (k, rec), (changed, sig)]), 2);
        let mut out = Vec::new();
        c.lookup_many(&[(7, k), (8, changed)], FileField::Signature, &mut out);
        assert_eq!(out, vec![(7, Arc::from("png"))]);
        out.clear();
        c.lookup_many(&[(7, k)], FileField::LinkTarget, &mut out);
        assert!(out.is_empty(), "empty values are not sort values");
        // Persistence.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("files.fnx");
        assert!(c.save_if_changed(&p).unwrap());
        assert!(!c.save_if_changed(&p).unwrap(), "unchanged");
        let back = FileValueCache::load_or_default(Some(&p));
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(&k, FileField::Signature).as_deref(), Some("png"));
        assert_eq!(back.get(&k, FileField::LinkTarget).as_deref(), Some(""));
        assert!(!back.save_if_changed(&p).unwrap(), "loaded = saved");
        let mut bad = c.to_bytes();
        let mid = bad.len() / 2;
        bad[mid] ^= 1;
        assert!(FileValueCache::from_bytes(&bad).is_err());
        assert!(FileValueCache::load_or_default(Some(&dir.path().join("none.fnx"))).is_empty());
        let small = FileValueCache { max_entries: 1, ..FileValueCache::new() };
        small.insert(k, FileField::Signature, "a");
        small.insert(changed, FileField::Signature, "b");
        assert_eq!(small.len(), 1);
        assert_eq!(FileField::lookup("Link-Target"), Some(FileField::LinkTarget));
        assert_eq!(FileField::lookup("recycle_bin_original_path"), Some(FileField::RecycleOrigin));
    }

    #[test]
    fn keys_follow_size_and_date() {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\d\",,,,16\n\
                   \"C:\\d\\a.lnk\",10,3,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        let id = (0..s.len() as u32).map(EntryId).find(|&i| s.name_string(i) == "a.lnk").unwrap();
        let dir = (0..s.len() as u32).map(EntryId).find(|&i| s.name_string(i) == "d").unwrap();
        let mut buf = Vec::new();
        let k = file_key(&s, id, &mut buf).unwrap();
        assert_eq!(k.size, 10);
        assert_eq!(file_key(&s, dir, &mut buf).unwrap().size, u64::MAX, "folders have keys too");
        s.apply(vec![fnc_index::Change::Meta {
            target: fnc_index::EntryRef::Id(id),
            meta: fnc_index::Meta { size: Some(11), ..fnc_index::Meta::default() },
        }]);
        assert_ne!(file_key(&s, id, &mut buf).unwrap(), k);
    }
}

//! Cancellation and shared caches.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fnc_core::EntryId;
use fnc_index::IndexStore;
use parking_lot::Mutex;

use crate::paths::DirPaths;
use crate::tree::{LinkCounts, NameLookup, TreeAggregates};
use crate::types::{TypeNames, type_name};

/// Cooperative cancellation flag shared by a query and its issuer.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// New, not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// `true` once cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Returned when a query was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("query cancelled")]
pub struct Cancelled;

/// Direct child counts per row.
pub struct ChildCounts {
    /// `(instance, generation)`.
    pub key: (u64, u64),
    /// Child files per folder.
    pub files: Vec<u32>,
    /// Child folders per folder.
    pub folders: Vec<u32>,
}

impl ChildCounts {
    fn build(s: &IndexStore) -> Self {
        let mut files = vec![0u32; s.len()];
        let mut folders = vec![0u32; s.len()];
        for i in 0..s.len() as u32 {
            let id = EntryId(i);
            if !s.is_live(id) {
                continue;
            }
            let p = s.parent(id);
            if p.is_some() {
                if s.is_dir(id) {
                    folders[p.idx()] += 1;
                } else {
                    files[p.idx()] += 1;
                }
            }
        }
        Self { key: (s.instance(), s.generation()), files, folders }
    }
}

/// Folder sort ranks by full path (natural order), for sorting by Path.
pub struct DirRank {
    /// `(instance, tree_generation)`.
    pub key: (u64, u64),
    /// Rank per row (folders only; others `u32::MAX`).
    pub rank: Vec<u32>,
}

/// Caches reused across queries while the index does not change.
#[derive(Default)]
pub struct Caches {
    dir_paths: Mutex<Option<Arc<DirPaths>>>,
    child_counts: Mutex<Option<Arc<ChildCounts>>>,
    dir_rank: Mutex<Option<Arc<DirRank>>>,
    tree: Mutex<Option<Arc<TreeAggregates>>>,
    links: Mutex<Option<Arc<LinkCounts>>>,
    names: Mutex<Option<Arc<NameLookup>>>,
    type_names: Mutex<Option<TypeNames>>,
    type_cache: Mutex<HashMap<Box<[u8]>, Arc<str>>>,
    /// Hashes computed when the app supplies no cache (kept for the engine's lifetime).
    hashes: Mutex<Option<Arc<fnc_props::hash::HashCache>>>,
    /// Properties read on demand (across queries).
    on_demand: Mutex<Option<Arc<crate::ext::OnDemandCache>>>,
    /// Link targets, signatures and recycle bin records read on demand (across queries).
    file_texts: Mutex<Option<Arc<crate::ext::FileTextCache>>>,
    /// Recent `si:` results (across queries, short-lived).
    system_index: Mutex<Option<Arc<crate::sysindex::SystemIndexCache>>>,
}

impl Caches {
    /// New, empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folder path cache for `s`, rebuilt if stale.
    pub fn dir_paths(&self, s: &IndexStore) -> Arc<DirPaths> {
        let mut slot = self.dir_paths.lock();
        let key = (s.instance(), s.tree_generation());
        match slot.as_ref() {
            Some(p) if p.key == key => p.clone(),
            _ => {
                let p = Arc::new(DirPaths::build(s));
                *slot = Some(p.clone());
                p
            }
        }
    }

    /// Child counts for `s`, rebuilt if stale.
    pub fn child_counts(&self, s: &IndexStore) -> Arc<ChildCounts> {
        let mut slot = self.child_counts.lock();
        let key = (s.instance(), s.generation());
        match slot.as_ref() {
            Some(c) if c.key == key => c.clone(),
            _ => {
                let c = Arc::new(ChildCounts::build(s));
                *slot = Some(c.clone());
                c
            }
        }
    }

    /// Folder path ranks for `s`, rebuilt if stale.
    pub fn dir_rank(&self, s: &IndexStore) -> Arc<DirRank> {
        let key = (s.instance(), s.tree_generation());
        if let Some(r) = self.dir_rank.lock().as_ref().filter(|r| r.key == key) {
            return r.clone();
        }
        let paths = self.dir_paths(s);
        let mut dirs: Vec<EntryId> = paths.folders().to_vec();
        use rayon::prelude::*;
        dirs.par_sort_unstable_by(|&a, &b| {
            let pa = paths.get(a).unwrap_or_default();
            let pb = paths.get(b).unwrap_or_default();
            fnc_core::natsort::natural_cmp(pa, pb).then(a.cmp(&b))
        });
        let mut rank = vec![u32::MAX; s.len()];
        for (i, d) in dirs.iter().enumerate() {
            rank[d.idx()] = i as u32;
        }
        let r = Arc::new(DirRank { key, rank });
        *self.dir_rank.lock() = Some(r.clone());
        r
    }

    /// Descendant counts and folder sizes for `s`, rebuilt if stale.
    pub fn tree_aggregates(&self, s: &IndexStore) -> Arc<TreeAggregates> {
        let key = (s.instance(), s.generation());
        if let Some(t) = self.tree.lock().as_ref().filter(|t| t.key == key) {
            return t.clone();
        }
        let direct = self.child_counts(s);
        let t = Arc::new(TreeAggregates::build(s, &direct));
        *self.tree.lock() = Some(t.clone());
        t
    }

    /// Hard link counts for `s`, rebuilt if stale.
    pub fn link_counts(&self, s: &IndexStore) -> Arc<LinkCounts> {
        let mut slot = self.links.lock();
        let key = (s.instance(), s.generation());
        match slot.as_ref() {
            Some(l) if l.key == key => l.clone(),
            _ => {
                let l = Arc::new(LinkCounts::build(s));
                *slot = Some(l.clone());
                l
            }
        }
    }

    /// Name lookup index for `s`, rebuilt if stale.
    pub fn name_lookup(&self, s: &IndexStore) -> Arc<NameLookup> {
        let mut slot = self.names.lock();
        let key = (s.instance(), s.generation());
        match slot.as_ref() {
            Some(l) if l.key == key => l.clone(),
            _ => {
                let l = Arc::new(NameLookup::build(s));
                *slot = Some(l.clone());
                l
            }
        }
    }

    /// Installs the type-name provider used by `type:` (`None` = fallback names
    /// only). Cached descriptions are dropped when the provider changes.
    pub fn set_type_names(&self, provider: Option<TypeNames>) {
        let mut cur = self.type_names.lock();
        if *cur != provider {
            *cur = provider;
            self.type_cache.lock().clear();
        }
    }

    /// Type description of a file with folded extension `ext` (cached per extension).
    pub fn type_description(&self, ext: &[u8]) -> Arc<str> {
        if let Some(d) = self.type_cache.lock().get(ext) {
            return d.clone();
        }
        let provider = self.type_names.lock().clone();
        let d: Arc<str> = type_name(&String::from_utf8_lossy(ext), provider.as_ref()).into();
        self.type_cache.lock().insert(ext.into(), d.clone());
        d
    }

    /// Fallback hash cache used when the search options carry none.
    pub fn query_hashes(&self) -> Arc<fnc_props::hash::HashCache> {
        self.hashes.lock().get_or_insert_with(|| Arc::new(fnc_props::hash::HashCache::new())).clone()
    }

    /// Cache of properties read from files (keyed by file identity, so it survives index
    /// changes and compaction).
    pub fn on_demand_props(&self) -> Arc<crate::ext::OnDemandCache> {
        self.on_demand.lock().get_or_insert_with(Default::default).clone()
    }

    /// Cache of link targets, file signatures and recycle bin records read from files
    /// (keyed by file identity).
    pub fn file_texts(&self) -> Arc<crate::ext::FileTextCache> {
        self.file_texts.lock().get_or_insert_with(Default::default).clone()
    }

    /// Recent results of system index queries (`si:`), by query text.
    pub fn system_index(&self) -> Arc<crate::sysindex::SystemIndexCache> {
        self.system_index.lock().get_or_insert_with(Default::default).clone()
    }

    /// Drops everything (e.g. after compaction). The type-name provider stays.
    pub fn clear(&self) {
        *self.dir_paths.lock() = None;
        *self.child_counts.lock() = None;
        *self.dir_rank.lock() = None;
        *self.tree.lock() = None;
        *self.links.lock() = None;
        *self.names.lock() = None;
        self.type_cache.lock().clear();
    }
}

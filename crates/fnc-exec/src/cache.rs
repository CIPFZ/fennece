//! Cancellation and shared caches.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fnc_core::EntryId;
use fnc_index::IndexStore;
use parking_lot::Mutex;

use crate::paths::DirPaths;

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

    /// Drops everything (e.g. after compaction).
    pub fn clear(&self) {
        *self.dir_paths.lock() = None;
        *self.child_counts.lock() = None;
        *self.dir_rank.lock() = None;
    }
}

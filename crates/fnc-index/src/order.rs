//! Name fast-sort index ("quick sort index" for the Name column, SPEC ch.1 §6).
//!
//! `rank[id]` is the position of `id` in the natural name order at build time. The
//! relative order of entries that have not been renamed since stays valid, so a
//! result set sorts by name with a `u32` key sort over clean entries plus a
//! comparator sort over the few dirty ones, merged in `O(n)` (see `fnc-exec`).

use fnc_core::{EntryFlags, EntryId};
use rayon::prelude::*;

use crate::store::IndexStore;

const NO_RANK: u32 = u32::MAX;

/// Name rank per entry.
#[derive(Clone, Debug, Default)]
pub struct NameOrder {
    rank: Vec<u32>,
    dirty: Vec<u64>,
    dirty_count: usize,
}

impl NameOrder {
    /// Builds from the current names (parallel sort).
    pub fn build(store: &IndexStore) -> Self {
        let flags = store.flags_column();
        let mut ids: Vec<EntryId> =
            (0..store.len() as u32).map(EntryId).filter(|id| !flags[id.idx()].contains(EntryFlags::DELETED)).collect();
        ids.par_sort_unstable_by(|&a, &b| store.cmp_names(a, b));
        let mut rank = vec![NO_RANK; store.len()];
        for (pos, id) in ids.iter().enumerate() {
            rank[id.idx()] = pos as u32;
        }
        let words = store.len().div_ceil(64);
        Self { rank, dirty: vec![0; words], dirty_count: 0 }
    }

    /// `true` if `id`'s rank is valid.
    #[inline]
    pub fn is_clean(&self, id: EntryId) -> bool {
        match self.rank.get(id.idx()) {
            Some(&r) if r != NO_RANK => self.dirty[id.idx() / 64] & (1 << (id.idx() % 64)) == 0,
            _ => false,
        }
    }

    /// Rank of a clean entry.
    #[inline]
    pub fn rank(&self, id: EntryId) -> u32 {
        self.rank[id.idx()]
    }

    /// Invalidates `id`'s rank (renamed or created).
    pub fn mark_dirty(&mut self, id: EntryId) {
        if id.idx() < self.rank.len() {
            let w = &mut self.dirty[id.idx() / 64];
            let bit = 1u64 << (id.idx() % 64);
            if *w & bit == 0 {
                *w |= bit;
                self.dirty_count += 1;
            }
        }
    }

    /// Number of rows covered by the last build.
    pub fn covered(&self) -> usize {
        self.rank.len()
    }

    /// Entries invalidated since the last build (excluding rows added after it).
    pub fn dirty_count(&self) -> usize {
        self.dirty_count
    }

    /// Carries ranks over an id renumbering (`remap[old] = new` or `u32::MAX`).
    pub fn remapped(&self, remap: &[u32], new_len: usize) -> Self {
        let mut out = Self { rank: vec![NO_RANK; new_len], dirty: vec![0; new_len.div_ceil(64)], dirty_count: 0 };
        for (old, &new) in remap.iter().enumerate() {
            if new == u32::MAX {
                continue;
            }
            let old_id = EntryId(old as u32);
            if self.is_clean(old_id) {
                out.rank[new as usize] = self.rank[old];
            }
        }
        out
    }

    /// The rank column if it covers all `len` rows and nothing is dirty (persisted).
    pub fn complete_ranks(&self, len: usize) -> Option<&[u32]> {
        (self.rank.len() == len && self.dirty_count == 0).then_some(&self.rank[..])
    }

    /// Restores a persisted rank column (see [`NameOrder::complete_ranks`]).
    pub fn from_ranks(rank: Vec<u32>) -> Self {
        let words = rank.len().div_ceil(64);
        Self { rank, dirty: vec![0; words], dirty_count: 0 }
    }

    /// Heap bytes.
    pub fn heap_bytes(&self) -> usize {
        self.rank.capacity() * 4 + self.dirty.capacity() * 8
    }
}

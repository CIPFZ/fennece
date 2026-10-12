//! `children` reverse index: CSR adjacency built in bulk, plus an overlay for
//! entries created or moved since the last rebuild.
//!
//! Invariant: for every live entry `c` with parent `p`, `c` appears in
//! `csr(p) ∪ overlay(p)`. Stale occurrences (entries that moved away or were
//! deleted) are filtered on read by checking `parent[c] == p` and the tombstone bit,
//! so moves never need to search-and-remove.

use std::collections::HashMap;

use fnc_core::EntryId;

/// Reverse parent → children index.
#[derive(Clone, Debug, Default)]
pub struct Children {
    /// `start[p]..start[p+1]` indexes `list` for entries built in bulk.
    start: Vec<u32>,
    /// Child ids grouped by parent, ascending within each group.
    list: Vec<EntryId>,
    /// Children added after the last rebuild.
    overlay: HashMap<EntryId, Vec<EntryId>>,
    overlay_len: usize,
}

impl Children {
    /// Rebuilds from the parent column (counting sort; `O(n)`).
    pub fn build(parent: &[EntryId], live: impl Fn(usize) -> bool) -> Self {
        let n = parent.len();
        let mut start = vec![0u32; n + 1];
        for (i, p) in parent.iter().enumerate() {
            if p.is_some() && live(i) {
                start[p.idx() + 1] += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut list = vec![EntryId::NONE; start[n] as usize];
        for (i, p) in parent.iter().enumerate() {
            if p.is_some() && live(i) {
                let slot = &mut fill[p.idx()];
                list[*slot as usize] = EntryId(i as u32);
                *slot += 1;
            }
        }
        Self { start, list, overlay: HashMap::new(), overlay_len: 0 }
    }

    /// Bulk-built children of `p` (may contain stale entries).
    #[inline]
    fn csr(&self, p: EntryId) -> &[EntryId] {
        match (self.start.get(p.idx()), self.start.get(p.idx() + 1)) {
            (Some(&a), Some(&b)) => &self.list[a as usize..b as usize],
            _ => &[],
        }
    }

    /// Candidate children of `p` (callers filter by parent and liveness).
    pub fn candidates(&self, p: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        self.csr(p).iter().copied().chain(self.overlay.get(&p).into_iter().flatten().copied())
    }

    /// Records that `child` now has parent `p`.
    pub fn add(&mut self, p: EntryId, child: EntryId) {
        if self.csr(p).binary_search(&child).is_ok() {
            return;
        }
        let v = self.overlay.entry(p).or_default();
        if !v.contains(&child) {
            v.push(child);
            self.overlay_len += 1;
        }
    }

    /// Number of overlay entries (rebuild when large).
    pub fn overlay_len(&self) -> usize {
        self.overlay_len
    }

    /// Heap bytes (approximate).
    pub fn heap_bytes(&self) -> usize {
        self.start.capacity() * 4 + self.list.capacity() * 4 + self.overlay_len * 4 + self.overlay.capacity() * 32
    }
}

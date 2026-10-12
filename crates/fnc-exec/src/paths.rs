//! Folded full paths of folders, for `path:` matching and path sorting.
//!
//! Built breadth-first from the volume roots (parents before children), so each
//! folder's path is its parent's path + `\` + its fold key. Separators are normalised
//! to `\` regardless of the source (the planner normalises patterns the same way).
//! Rebuilt when the index's `tree_generation` changes.

use fnc_core::{EntryId, FoldArena};
use fnc_index::IndexStore;

const NO_SLOT: u32 = u32::MAX;

/// Folded full paths of all folders.
pub struct DirPaths {
    /// `(instance, tree_generation)` this cache was built for.
    pub key: (u64, u64),
    slot: Vec<u32>,
    arena: FoldArena,
    /// Folder ids in breadth-first order (parents first).
    order: Vec<EntryId>,
}

impl DirPaths {
    /// Builds the cache.
    pub fn build(s: &IndexStore) -> Self {
        let mut slot = vec![NO_SLOT; s.len()];
        let mut arena = FoldArena::with_capacity(s.stats().folders + 16, s.stats().folders * 48);
        let mut order = Vec::with_capacity(s.stats().folders + 16);
        let mut buf: Vec<u8> = Vec::with_capacity(512);
        let mut queue: std::collections::VecDeque<EntryId> = std::collections::VecDeque::new();
        for v in s.volumes() {
            for &r in &v.roots {
                if s.is_live(r) {
                    buf.clear();
                    buf.extend(s.fold(r).iter().map(|&b| if b == b'/' { b'\\' } else { b }));
                    if let Ok(k) = arena.push(&buf) {
                        slot[r.idx()] = k as u32;
                        order.push(r);
                        queue.push_back(r);
                    }
                }
            }
        }
        while let Some(d) = queue.pop_front() {
            let Some(base) = slot.get(d.idx()).copied().filter(|&k| k != NO_SLOT) else { continue };
            for c in s.children(d) {
                if !s.is_dir(c) {
                    continue;
                }
                buf.clear();
                buf.extend_from_slice(arena.get(base as usize));
                if buf.last() != Some(&b'\\') {
                    buf.push(b'\\');
                }
                buf.extend_from_slice(s.fold(c));
                if let Ok(k) = arena.push(&buf) {
                    slot[c.idx()] = k as u32;
                    order.push(c);
                    queue.push_back(c);
                }
            }
        }
        Self { key: (s.instance(), s.tree_generation()), slot, arena, order }
    }

    /// Folded path of folder `dir`.
    #[inline]
    pub fn get(&self, dir: EntryId) -> Option<&[u8]> {
        match self.slot.get(dir.idx()) {
            Some(&k) if k != NO_SLOT => Some(self.arena.get(k as usize)),
            _ => None,
        }
    }

    /// Folders in breadth-first order.
    pub fn folders(&self) -> &[EntryId] {
        &self.order
    }

    /// Writes the folded full path of any entry into `out` (cleared first).
    #[inline]
    pub fn full_path_into(&self, s: &IndexStore, id: EntryId, out: &mut Vec<u8>) {
        out.clear();
        if let Some(p) = self.get(id) {
            out.extend_from_slice(p);
            return;
        }
        if let Some(p) = self.get(s.parent(id)) {
            out.extend_from_slice(p);
            if out.last() != Some(&b'\\') {
                out.push(b'\\');
            }
        }
        out.extend_from_slice(s.fold(id));
    }

    /// Heap bytes.
    pub fn heap_bytes(&self) -> usize {
        self.slot.capacity() * 4 + self.arena.heap_bytes() + self.order.capacity() * 4
    }
}

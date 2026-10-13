//! Entry ↔ row mapping shared by the sparse stores.
//!
//! A store keeps its data in dense rows; `row_of[EntryId]` points at the row of an entry
//! (or `NONE`). Rows of removed entries are recycled. The map is bound to one
//! [`IndexStore::instance`] id space and follows compaction through [`RowMap::remap`].
//!
//! On disk, rows are identified by `(volume root path, FRN)` plus a check hash of name,
//! parent name, size and date modified, so a saved store re-attaches to a reloaded (and
//! possibly compacted or rebuilt) index; rows whose entry changed are dropped.

use fnc_core::EntryId;
use fnc_index::{EntryRef, IndexStore};
use xxhash_rust::xxh3::Xxh3;

use crate::fnx::{Dec, Enc, FnxResult};

/// "No row" / "no entry".
pub const NONE: u32 = u32::MAX;

/// Dense row allocation keyed by entry id.
#[derive(Clone, Debug, Default)]
pub struct RowMap {
    instance: u64,
    row_of: Vec<u32>,
    id_of: Vec<u32>,
    free: Vec<u32>,
    live: usize,
}

impl RowMap {
    /// Empty map for index instance `instance`.
    pub fn new(instance: u64) -> Self {
        Self { instance, ..Self::default() }
    }

    /// Index instance the entry ids belong to.
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// Entries with a row.
    pub fn len(&self) -> usize {
        self.live
    }

    /// `true` if no entry has a row.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Allocated rows (including free ones); columns are at most this long.
    pub fn rows(&self) -> usize {
        self.id_of.len()
    }

    /// Row of `id`.
    #[inline]
    pub fn row(&self, id: EntryId) -> Option<u32> {
        self.row_of.get(id.idx()).copied().filter(|&r| r != NONE)
    }

    /// Entry of every row (`NONE` for free rows).
    #[inline]
    pub fn ids(&self) -> &[u32] {
        &self.id_of
    }

    /// Row of `id`, allocating one. Returns `(row, new)`.
    pub fn ensure(&mut self, id: EntryId) -> (u32, bool) {
        if let Some(r) = self.row(id) {
            return (r, false);
        }
        if self.row_of.len() <= id.idx() {
            self.row_of.resize(id.idx() + 1, NONE);
        }
        let r = match self.free.pop() {
            Some(r) => {
                self.id_of[r as usize] = id.0;
                r
            }
            None => {
                self.id_of.push(id.0);
                (self.id_of.len() - 1) as u32
            }
        };
        self.row_of[id.idx()] = r;
        self.live += 1;
        (r, true)
    }

    /// Frees the row of `id`; returns it.
    pub fn remove(&mut self, id: EntryId) -> Option<u32> {
        let r = self.row(id)?;
        self.row_of[id.idx()] = NONE;
        self.id_of[r as usize] = NONE;
        self.free.push(r);
        self.live -= 1;
        Some(r)
    }

    /// Follows [`IndexStore::compact`]: `remap[old] = new` (`u32::MAX` = removed).
    /// Returns the rows that were freed (their entry disappeared).
    pub fn remap(&mut self, remap: &[u32], new_instance: u64) -> Vec<u32> {
        let new_len = remap.iter().filter(|&&n| n != NONE).map(|&n| n as usize + 1).max().unwrap_or(0);
        let mut row_of = vec![NONE; new_len];
        let mut freed = Vec::new();
        for (r, id) in self.id_of.iter_mut().enumerate() {
            if *id == NONE {
                continue;
            }
            match remap.get(*id as usize).copied().unwrap_or(NONE) {
                NONE => {
                    *id = NONE;
                    freed.push(r as u32);
                }
                n => {
                    row_of[n as usize] = r as u32;
                    *id = n;
                }
            }
        }
        self.live -= freed.len();
        self.free.extend(&freed);
        self.row_of = row_of;
        self.instance = new_instance;
        freed
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        let inst = self.instance;
        *self = Self::new(inst);
    }

    /// Approximate heap use in bytes.
    pub fn heap_bytes(&self) -> usize {
        (self.row_of.capacity() + self.id_of.capacity() + self.free.capacity()) * 4
    }

    /// Encodes the identities of all rows (`ROWS` segment); free rows are written as
    /// empty identities so row numbers stay stable within the file.
    pub fn encode(&self, s: &IndexStore) -> Vec<u8> {
        let mut vols: Vec<&str> = Vec::new();
        let mut e = Enc::default();
        let mut rows = Enc::default();
        rows.u32(self.id_of.len() as u32);
        for &id in &self.id_of {
            let id = EntryId(id);
            if id.0 == NONE || id.idx() >= s.len() || !s.is_live(id) {
                rows.u16(u16::MAX);
                rows.u64(0);
                rows.u64(0);
                continue;
            }
            let root = s.volume_info(s.volume_of(id)).map_or("", |v| v.info.root_path.as_str());
            let vi = match vols.iter().position(|v| *v == root) {
                Some(i) => i,
                None => {
                    vols.push(root);
                    vols.len() - 1
                }
            };
            rows.u16(vi as u16);
            rows.u64(s.frn(id));
            rows.u64(check(s, id));
        }
        e.u32(vols.len() as u32);
        for v in &vols {
            e.str(v);
        }
        e.0.extend_from_slice(&rows.0);
        e.0
    }

    /// Decodes a `ROWS` segment against `s`: returns the map and, per stored row, the new
    /// row it was assigned (`NONE` if its entry is gone or changed).
    pub fn decode(seg: &[u8], s: &IndexStore) -> FnxResult<(RowMap, Vec<u32>)> {
        let mut d = Dec::new(seg);
        let nv = d.count(4)?;
        let mut vols = Vec::with_capacity(nv);
        for _ in 0..nv {
            let root = d.str()?;
            vols.push(s.volumes().iter().find(|v| v.info.root_path == root).map(|v| v.id));
        }
        let n = d.count(18)?;
        let mut map = RowMap::new(s.instance());
        let mut assign = Vec::with_capacity(n);
        for _ in 0..n {
            let (vi, frn, chk) = (d.u16()?, d.u64()?, d.u64()?);
            let id = vols
                .get(vi as usize)
                .copied()
                .flatten()
                .and_then(|v| s.resolve(EntryRef::Frn(v, frn)))
                .filter(|&id| check(s, id) == chk);
            assign.push(match id {
                Some(id) if map.row(id).is_none() => map.ensure(id).0,
                _ => NONE,
            });
        }
        Ok((map, assign))
    }
}

/// Identity check of an entry: folded name, parent's folded name, size, date modified.
pub fn check(s: &IndexStore, id: EntryId) -> u64 {
    let mut h = Xxh3::new();
    h.update(s.fold(id));
    h.update(b"\\");
    let p = s.parent(id);
    if p.is_some() {
        h.update(s.fold(p));
    }
    h.update(&s.size(id).unwrap_or(u64::MAX).to_le_bytes());
    h.update(&s.date_modified(id).0.to_le_bytes());
    h.digest()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_free_and_remap() {
        let mut m = RowMap::new(1);
        assert_eq!(m.ensure(EntryId(5)), (0, true));
        assert_eq!(m.ensure(EntryId(2)), (1, true));
        assert_eq!(m.ensure(EntryId(5)), (0, false));
        assert_eq!(m.len(), 2);
        assert_eq!(m.remove(EntryId(5)), Some(0));
        assert_eq!(m.row(EntryId(5)), None);
        assert_eq!(m.ensure(EntryId(7)), (0, true), "freed row reused");
        // Compaction: 2 → 0, 7 → removed.
        let mut remap = vec![NONE; 8];
        remap[2] = 0;
        let freed = m.remap(&remap, 9);
        assert_eq!(freed, vec![0]);
        assert_eq!(m.row(EntryId(0)), Some(1));
        assert_eq!(m.len(), 1);
        assert_eq!(m.instance(), 9);
        assert_eq!(m.ids()[1], 0);
        m.clear();
        assert!(m.is_empty());
    }
}

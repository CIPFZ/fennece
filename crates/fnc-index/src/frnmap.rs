//! Per-volume FRN → `EntryId` map.
//!
//! NTFS file reference numbers are `(sequence:16 | mft_record:48)`; MFT record numbers
//! are dense, so the map is normally a `Vec<u32>` indexed by record number (4 bytes per
//! MFT record instead of ~24 bytes per entry for a hash map). Records far beyond the
//! current table go to a small overflow map. Sources with sparse ids (ReFS, inode
//! numbers, synthetic ids) use a `HashMap` keyed by the full id. In dense mode the
//! sequence bits are not stored, so callers verify the candidate against the entry's
//! full FRN.

use std::collections::HashMap;

use fnc_core::EntryId;

const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const EMPTY: u32 = u32::MAX;

/// Adaptive FRN map.
#[derive(Clone, Debug)]
pub enum FrnMap {
    /// Indexed by MFT record number.
    Dense {
        /// `slots[record]` = entry id or `EMPTY`.
        slots: Vec<u32>,
        /// Records too far beyond `slots.len()` to grow the table for.
        overflow: HashMap<u64, EntryId>,
    },
    /// Keyed by full FRN.
    Sparse(HashMap<u64, EntryId>),
}

impl Default for FrnMap {
    fn default() -> Self {
        FrnMap::Sparse(HashMap::new())
    }
}

impl FrnMap {
    /// Builds the best representation for `(frn, id)` pairs. `ntfs_like` enables the
    /// dense representation when record numbers are compact.
    pub fn build(pairs: &[(u64, EntryId)], ntfs_like: bool) -> Self {
        let max_rec = pairs.iter().map(|&(f, _)| f & RECORD_MASK).max().unwrap_or(0);
        if ntfs_like && (max_rec as usize) < pairs.len().saturating_mul(4) + 4096 {
            let mut slots = vec![EMPTY; max_rec as usize + 1];
            for &(frn, id) in pairs {
                slots[(frn & RECORD_MASK) as usize] = id.0;
            }
            FrnMap::Dense { slots, overflow: HashMap::new() }
        } else {
            FrnMap::Sparse(pairs.iter().copied().collect())
        }
    }

    /// Candidate entry for `frn` (verify the full FRN before use in dense mode).
    #[inline]
    pub fn get(&self, frn: u64) -> Option<EntryId> {
        match self {
            FrnMap::Dense { slots, overflow } => {
                let rec = frn & RECORD_MASK;
                match slots.get(rec as usize) {
                    Some(&id) if id != EMPTY => Some(EntryId(id)),
                    Some(_) => None,
                    None => overflow.get(&rec).copied(),
                }
            }
            FrnMap::Sparse(m) => m.get(&frn).copied(),
        }
    }

    /// Inserts or replaces.
    pub fn insert(&mut self, frn: u64, id: EntryId) {
        match self {
            FrnMap::Dense { slots, overflow } => {
                let rec = (frn & RECORD_MASK) as usize;
                if rec < slots.len() {
                    slots[rec] = id.0;
                } else if rec < slots.len().saturating_mul(2) + 65_536 {
                    slots.resize(rec + 1, EMPTY);
                    slots[rec] = id.0;
                } else {
                    overflow.insert(rec as u64, id);
                }
            }
            FrnMap::Sparse(m) => {
                m.insert(frn, id);
            }
        }
    }

    /// Removes `frn` if it currently maps to `id`.
    pub fn remove(&mut self, frn: u64, id: EntryId) {
        match self {
            FrnMap::Dense { slots, overflow } => {
                let rec = frn & RECORD_MASK;
                match slots.get_mut(rec as usize) {
                    Some(slot) => {
                        if *slot == id.0 {
                            *slot = EMPTY;
                        }
                    }
                    None => {
                        if overflow.get(&rec) == Some(&id) {
                            overflow.remove(&rec);
                        }
                    }
                }
            }
            FrnMap::Sparse(m) => {
                if m.get(&frn) == Some(&id) {
                    m.remove(&frn);
                }
            }
        }
    }

    /// Heap bytes (approximate).
    pub fn heap_bytes(&self) -> usize {
        match self {
            FrnMap::Dense { slots, overflow } => slots.capacity() * 4 + overflow.capacity() * 16,
            FrnMap::Sparse(m) => m.capacity() * 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_for_ntfs_records() {
        let pairs: Vec<_> = (0..100u64).map(|r| ((7u64 << 48) | r, EntryId(r as u32))).collect();
        let mut m = FrnMap::build(&pairs, true);
        assert!(matches!(m, FrnMap::Dense { .. }));
        assert_eq!(m.get((7u64 << 48) | 42), Some(EntryId(42)));
        m.insert((1u64 << 48) | 150, EntryId(500));
        assert_eq!(m.get(150), Some(EntryId(500)));
        m.insert(10_000_000, EntryId(9));
        assert_eq!(m.get(10_000_000), Some(EntryId(9)));
        m.remove(10_000_000, EntryId(9));
        assert_eq!(m.get(10_000_000), None);
        m.remove(42, EntryId(41));
        assert_eq!(m.get(42), Some(EntryId(42)), "remove only if it maps to the id");
        m.remove(42, EntryId(42));
        assert_eq!(m.get(42), None);
    }

    #[test]
    fn sparse_for_scattered_ids() {
        let pairs = vec![(u64::MAX - 1, EntryId(1)), (5, EntryId(2))];
        let m = FrnMap::build(&pairs, true);
        assert!(matches!(m, FrnMap::Sparse(_)));
        assert_eq!(m.get(u64::MAX - 1), Some(EntryId(1)));
        assert_eq!(m.get(6), None);
    }
}

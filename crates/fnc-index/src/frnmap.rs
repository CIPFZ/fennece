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
    /// Indexed by MFT record number (or by the id itself for small synthetic ids).
    Dense {
        /// `slots[key]` = entry id or `EMPTY`.
        slots: Vec<u32>,
        /// Keys too far beyond `slots.len()` to grow the table for.
        overflow: HashMap<u64, EntryId>,
        /// `true`: key = FRN without the sequence bits (NTFS); `false`: key = FRN.
        masked: bool,
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
    /// Builds the best representation for `(frn, id)` pairs. The dense representation
    /// is used when keys are compact: NTFS record numbers (`ntfs_like`, sequence bits
    /// masked off) or small synthetic ids.
    pub fn build(pairs: &[(u64, EntryId)], ntfs_like: bool) -> Self {
        let key = |f: u64| if ntfs_like { f & RECORD_MASK } else { f };
        let max_key = pairs.iter().map(|&(f, _)| key(f)).max().unwrap_or(0);
        if max_key < (pairs.len() as u64).saturating_mul(4) + 4096 {
            let mut slots = vec![EMPTY; max_key as usize + 1];
            for &(frn, id) in pairs {
                slots[key(frn) as usize] = id.0;
            }
            FrnMap::Dense { slots, overflow: HashMap::new(), masked: ntfs_like }
        } else {
            FrnMap::Sparse(pairs.iter().copied().collect())
        }
    }

    #[inline]
    fn key(masked: bool, frn: u64) -> u64 {
        if masked { frn & RECORD_MASK } else { frn }
    }

    /// Candidate entry for `frn` (verify the full FRN before use in dense mode).
    #[inline]
    pub fn get(&self, frn: u64) -> Option<EntryId> {
        match self {
            FrnMap::Dense { slots, overflow, masked } => {
                let k = Self::key(*masked, frn);
                match usize::try_from(k).ok().and_then(|k| slots.get(k)) {
                    Some(&id) if id != EMPTY => Some(EntryId(id)),
                    Some(_) => None,
                    None => overflow.get(&k).copied(),
                }
            }
            FrnMap::Sparse(m) => m.get(&frn).copied(),
        }
    }

    /// Inserts or replaces.
    pub fn insert(&mut self, frn: u64, id: EntryId) {
        match self {
            FrnMap::Dense { slots, overflow, masked } => {
                let k = Self::key(*masked, frn);
                if k < slots.len() as u64 {
                    slots[k as usize] = id.0;
                } else if k < (slots.len() as u64).saturating_mul(2) + 65_536 {
                    slots.resize(k as usize + 1, EMPTY);
                    slots[k as usize] = id.0;
                    // Keys that now fall inside the table must live in it, because
                    // `get` only consults `overflow` beyond the table.
                    let len = slots.len() as u64;
                    let moved: Vec<u64> = overflow.keys().copied().filter(|&o| o < len).collect();
                    for o in moved {
                        if let Some(e) = overflow.remove(&o) {
                            slots[o as usize] = e.0;
                        }
                    }
                } else {
                    overflow.insert(k, id);
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
            FrnMap::Dense { slots, overflow, masked } => {
                let k = Self::key(*masked, frn);
                match usize::try_from(k).ok().and_then(|k| slots.get_mut(k)) {
                    Some(slot) => {
                        if *slot == id.0 {
                            *slot = EMPTY;
                        }
                    }
                    None => {
                        if overflow.get(&k) == Some(&id) {
                            overflow.remove(&k);
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
            FrnMap::Dense { slots, overflow, .. } => slots.capacity() * 4 + overflow.capacity() * 16,
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
    fn overflow_keys_survive_table_growth() {
        let pairs: Vec<_> = (0..100u64).map(|r| (r, EntryId(r as u32))).collect();
        let mut m = FrnMap::build(&pairs, false);
        m.insert(100_000, EntryId(7)); // beyond growth limit → overflow
        assert_eq!(m.get(100_000), Some(EntryId(7)));
        m.insert(60_000, EntryId(8)); // grows the table to 60_001
        m.insert(100_001, EntryId(9)); // grows past 100_000
        assert_eq!(m.get(100_000), Some(EntryId(7)));
        assert_eq!(m.get(100_001), Some(EntryId(9)));
        assert_eq!(m.get(60_000), Some(EntryId(8)));
        m.remove(100_000, EntryId(7));
        assert_eq!(m.get(100_000), None);
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

//! Append-only string pools.
//!
//! Every entry name lives in one contiguous buffer instead of an individual `String`
//! (SPEC ch.1 §1.1, AGENT iron rule 4). Item `i` is addressed by `(off[i], len[i])`;
//! in the index, item `i` belongs to `EntryId(i)`.
//!
//! * [`NameArena`] stores names as UTF-16 (lossless with NTFS, including unpaired
//!   surrogates).
//! * [`FoldArena`] stores fold keys as UTF-8 so matching can use byte-oriented SIMD
//!   substring search (`memchr::memmem`). See ADR-002.
//!
//! Replacing an item (rename) writes in place when the new value fits, otherwise
//! appends and leaves the old bytes as garbage; [`Arena::compact`] reclaims it.

use crate::error::{CoreError, Result};

/// Maximum number of units in one item.
pub const MAX_ITEM_LEN: usize = u16::MAX as usize;

/// A pool of variable-length slices of `T`, addressed by dense item index.
#[derive(Clone, Default)]
pub struct Arena<T: Copy> {
    data: Vec<T>,
    off: Vec<u32>,
    len: Vec<u16>,
    garbage: usize,
}

/// UTF-16 name pool.
pub type NameArena = Arena<u16>;
/// UTF-8 fold-key pool.
pub type FoldArena = Arena<u8>;

impl<T: Copy> Arena<T> {
    /// Empty arena.
    pub fn new() -> Self {
        Self { data: Vec::new(), off: Vec::new(), len: Vec::new(), garbage: 0 }
    }

    /// Arena with room for `items` items totalling `units` units.
    pub fn with_capacity(items: usize, units: usize) -> Self {
        Self {
            data: Vec::with_capacity(units),
            off: Vec::with_capacity(items),
            len: Vec::with_capacity(items),
            garbage: 0,
        }
    }

    /// Number of items.
    #[inline]
    pub fn len(&self) -> usize {
        self.off.len()
    }

    /// `true` if there are no items.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.off.is_empty()
    }

    /// Total units stored, including garbage.
    #[inline]
    pub fn units(&self) -> usize {
        self.data.len()
    }

    /// Units no longer referenced by any item.
    #[inline]
    pub fn garbage_units(&self) -> usize {
        self.garbage
    }

    /// Heap bytes used by this arena.
    pub fn heap_bytes(&self) -> usize {
        self.data.capacity() * std::mem::size_of::<T>() + self.off.capacity() * 4 + self.len.capacity() * 2
    }

    fn append_data(&mut self, s: &[T]) -> Result<u32> {
        if s.len() > MAX_ITEM_LEN {
            return Err(CoreError::StringTooLong(s.len()));
        }
        let off = u32::try_from(self.data.len()).map_err(|_| CoreError::ArenaFull)?;
        if (off as usize).saturating_add(s.len()) > u32::MAX as usize {
            return Err(CoreError::ArenaFull);
        }
        self.data.extend_from_slice(s);
        Ok(off)
    }

    /// Appends a new item and returns its index.
    pub fn push(&mut self, s: &[T]) -> Result<usize> {
        let off = self.append_data(s)?;
        self.off.push(off);
        // Length checked in `append_data`.
        self.len.push(s.len() as u16);
        Ok(self.off.len() - 1)
    }

    /// Item `i`. Panics if `i` is out of bounds (ids are produced by the index).
    #[inline]
    pub fn get(&self, i: usize) -> &[T] {
        let off = self.off[i] as usize;
        &self.data[off..off + self.len[i] as usize]
    }

    /// Item `i`, or `None` when out of bounds.
    #[inline]
    pub fn try_get(&self, i: usize) -> Option<&[T]> {
        let off = *self.off.get(i)? as usize;
        let len = *self.len.get(i)? as usize;
        self.data.get(off..off + len)
    }

    /// Length (in units) of item `i`.
    #[inline]
    pub fn item_len(&self, i: usize) -> usize {
        self.len[i] as usize
    }

    /// Replaces item `i`. Writes in place if the new value is not longer.
    pub fn replace(&mut self, i: usize, s: &[T]) -> Result<()> {
        let old_len = self.len[i] as usize;
        if s.len() <= old_len {
            let off = self.off[i] as usize;
            self.data[off..off + s.len()].copy_from_slice(s);
            self.garbage += old_len - s.len();
            self.len[i] = s.len() as u16;
            return Ok(());
        }
        let off = self.append_data(s)?;
        self.garbage += old_len;
        self.off[i] = off;
        self.len[i] = s.len() as u16;
        Ok(())
    }

    /// Releases item `i`'s storage (the item becomes empty). Used for tombstones.
    pub fn clear_item(&mut self, i: usize) {
        self.garbage += self.len[i] as usize;
        self.len[i] = 0;
    }

    /// Rebuilds the buffer without garbage, keeping item order and indexes.
    pub fn compact(&mut self) {
        if self.garbage == 0 {
            return;
        }
        let live = self.data.len() - self.garbage;
        let mut data = Vec::with_capacity(live);
        for i in 0..self.off.len() {
            let off = self.off[i] as usize;
            let len = self.len[i] as usize;
            self.off[i] = data.len() as u32;
            data.extend_from_slice(&self.data[off..off + len]);
        }
        self.data = data;
        self.garbage = 0;
    }

    /// Builds a new arena containing items in the order given by `order`
    /// (`new item k = old item order[k]`). Used when entry ids are renumbered.
    pub fn permuted(&self, order: &[u32]) -> Self {
        let units: usize = order.iter().map(|&i| self.len[i as usize] as usize).sum();
        let mut out = Self::with_capacity(order.len(), units);
        for &i in order {
            let s = self.get(i as usize);
            out.off.push(out.data.len() as u32);
            out.len.push(s.len() as u16);
            out.data.extend_from_slice(s);
        }
        out
    }

    /// Raw parts `(data, offsets, lengths)` for persistence.
    pub fn raw_parts(&self) -> (&[T], &[u32], &[u16]) {
        (&self.data, &self.off, &self.len)
    }

    /// Rebuilds from raw parts, validating every slice. Returns `None` if inconsistent.
    pub fn from_raw_parts(data: Vec<T>, off: Vec<u32>, len: Vec<u16>) -> Option<Self> {
        if off.len() != len.len() {
            return None;
        }
        let mut used = 0usize;
        for (&o, &l) in off.iter().zip(&len) {
            let end = (o as usize).checked_add(l as usize)?;
            if end > data.len() {
                return None;
            }
            used += l as usize;
        }
        let garbage = data.len().saturating_sub(used);
        Some(Self { data, off, len, garbage })
    }

    /// Shrinks capacity to fit (after bulk load).
    pub fn shrink_to_fit(&mut self) {
        self.data.shrink_to_fit();
        self.off.shrink_to_fit();
        self.len.shrink_to_fit();
    }
}

impl NameArena {
    /// Appends a `&str` (encoded to UTF-16).
    pub fn push_str(&mut self, s: &str) -> Result<usize> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.push(&units)
    }

    /// Decodes item `i` into `out` (lossy for unpaired surrogates), appending.
    pub fn write_str(&self, i: usize, out: &mut String) {
        utf16_to_string_into(self.get(i), out);
    }
}

/// Appends UTF-16 `units` to `out`, replacing unpaired surrogates with U+FFFD.
/// Allocation-free when `out` has enough capacity.
pub fn utf16_to_string_into(units: &[u16], out: &mut String) {
    if units.iter().all(|&u| u < 0x80) {
        out.reserve(units.len());
        for &u in units {
            out.push(u as u8 as char);
        }
        return;
    }
    for c in char::decode_utf16(units.iter().copied()) {
        out.push(c.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &NameArena, i: usize) -> String {
        let mut o = String::new();
        a.write_str(i, &mut o);
        o
    }

    #[test]
    fn push_get_replace() {
        let mut a = NameArena::new();
        let i0 = a.push_str("hello.txt").unwrap();
        let i1 = a.push_str("文件.doc").unwrap();
        assert_eq!((i0, i1), (0, 1));
        assert_eq!(s(&a, 0), "hello.txt");
        assert_eq!(s(&a, 1), "文件.doc");
        a.replace(0, &"hi".encode_utf16().collect::<Vec<_>>()).unwrap();
        assert_eq!(s(&a, 0), "hi");
        assert_eq!(a.garbage_units(), 7);
        a.replace(1, &"a-much-longer-name.txt".encode_utf16().collect::<Vec<_>>()).unwrap();
        assert_eq!(s(&a, 1), "a-much-longer-name.txt");
        a.compact();
        assert_eq!(a.garbage_units(), 0);
        assert_eq!(a.units(), 2 + 22);
        assert_eq!(s(&a, 0), "hi");
        assert_eq!(s(&a, 1), "a-much-longer-name.txt");
    }

    #[test]
    fn unpaired_surrogate_is_preserved_in_storage() {
        let mut a = NameArena::new();
        let raw = [0x61, 0xD800, 0x62];
        a.push(&raw).unwrap();
        assert_eq!(a.get(0), &raw);
        assert_eq!(s(&a, 0), "a\u{FFFD}b");
    }

    #[test]
    fn too_long_is_rejected() {
        let mut a = FoldArena::new();
        assert!(a.push(&vec![b'a'; MAX_ITEM_LEN + 1]).is_err());
        assert!(a.push(&vec![b'a'; MAX_ITEM_LEN]).is_ok());
    }

    #[test]
    fn raw_parts_roundtrip_and_validation() {
        let mut a = FoldArena::new();
        a.push(b"abc").unwrap();
        a.push(b"de").unwrap();
        let (d, o, l) = a.raw_parts();
        let b = FoldArena::from_raw_parts(d.to_vec(), o.to_vec(), l.to_vec()).unwrap();
        assert_eq!(b.get(1), b"de");
        assert!(FoldArena::from_raw_parts(vec![1, 2], vec![1], vec![5]).is_none());
    }

    #[test]
    fn permuted_reorders() {
        let mut a = FoldArena::new();
        for w in ["x", "yy", "zzz"] {
            a.push(w.as_bytes()).unwrap();
        }
        let p = a.permuted(&[2, 0, 1]);
        assert_eq!(p.get(0), b"zzz");
        assert_eq!(p.get(1), b"x");
        assert_eq!(p.get(2), b"yy");
    }
}

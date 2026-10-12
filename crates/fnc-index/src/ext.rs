//! Extension dictionary: folded extension → small id (`ext_id` column).

use std::collections::HashMap;

/// `ext_id` of entries without an extension.
pub const NO_EXT: u16 = 0;
/// `ext_id` used once the dictionary is full; such entries are matched by string.
pub const OVERFLOW_EXT: u16 = u16::MAX;

/// Interning table for extensions (stored folded, without the dot).
#[derive(Clone, Debug, Default)]
pub struct ExtDict {
    by_name: HashMap<Box<[u8]>, u16>,
    names: Vec<Box<[u8]>>,
}

impl ExtDict {
    /// Empty dictionary (id 0 is reserved for "no extension").
    pub fn new() -> Self {
        Self { by_name: HashMap::new(), names: vec![Box::from(&b""[..])] }
    }

    /// Id of `ext` (folded, no dot), inserting it if needed.
    pub fn intern(&mut self, ext: &[u8]) -> u16 {
        if ext.is_empty() {
            return NO_EXT;
        }
        if let Some(&id) = self.by_name.get(ext) {
            return id;
        }
        if self.names.len() >= OVERFLOW_EXT as usize {
            return OVERFLOW_EXT;
        }
        let id = self.names.len() as u16;
        self.names.push(Box::from(ext));
        self.by_name.insert(Box::from(ext), id);
        id
    }

    /// Id of `ext` if known.
    pub fn lookup(&self, ext: &[u8]) -> Option<u16> {
        if ext.is_empty() {
            return Some(NO_EXT);
        }
        self.by_name.get(ext).copied()
    }

    /// Extension text of `id` (empty for [`NO_EXT`] / [`OVERFLOW_EXT`]).
    pub fn name(&self, id: u16) -> &[u8] {
        self.names.get(id as usize).map_or(&[][..], |b| &b[..])
    }

    /// Number of distinct extensions.
    pub fn len(&self) -> usize {
        self.names.len() - 1
    }

    /// `true` if no extension has been interned.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterates `(id, ext)` pairs (excluding [`NO_EXT`]).
    pub fn iter(&self) -> impl Iterator<Item = (u16, &[u8])> {
        self.names.iter().enumerate().skip(1).map(|(i, n)| (i as u16, &n[..]))
    }
}

/// Extension part of a folded name (`b"photo.jpg"` → `b"jpg"`); empty if none.
///
/// A leading dot counts as an extension separator (`.gitignore` → `gitignore`),
/// matching `PathFindExtensionW`.
#[inline]
pub fn ext_of(fold: &[u8]) -> &[u8] {
    match memrchr_dot(fold) {
        Some(p) if p + 1 < fold.len() => &fold[p + 1..],
        _ => &[],
    }
}

/// Stem of a folded name (`b"photo.jpg"` → `b"photo"`).
#[inline]
pub fn stem_of(fold: &[u8]) -> &[u8] {
    match memrchr_dot(fold) {
        Some(p) if p + 1 < fold.len() => &fold[..p],
        _ => fold,
    }
}

#[inline]
fn memrchr_dot(s: &[u8]) -> Option<usize> {
    s.iter().rposition(|&b| b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_and_stem() {
        assert_eq!(ext_of(b"photo.jpg"), b"jpg");
        assert_eq!(ext_of(b"archive.tar.gz"), b"gz");
        assert_eq!(ext_of(b"readme"), b"");
        assert_eq!(ext_of(b"trailing."), b"");
        assert_eq!(ext_of(b".gitignore"), b"gitignore");
        assert_eq!(stem_of(b"photo.jpg"), b"photo");
        assert_eq!(stem_of(b"readme"), b"readme");
        assert_eq!(stem_of(b".gitignore"), b"");
    }

    #[test]
    fn intern_is_stable() {
        let mut d = ExtDict::new();
        let a = d.intern(b"jpg");
        let b = d.intern(b"png");
        assert_eq!(d.intern(b"jpg"), a);
        assert_ne!(a, b);
        assert_eq!(d.intern(b""), NO_EXT);
        assert_eq!(d.name(a), b"jpg");
        assert_eq!(d.lookup(b"png"), Some(b));
        assert_eq!(d.lookup(b"gif"), None);
        assert_eq!(d.len(), 2);
    }
}

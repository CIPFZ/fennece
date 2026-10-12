//! Strongly typed identifiers.

use std::fmt;

/// Index of an entry inside an `IndexStore` (columnar row number).
///
/// Ids are dense (`0..len`) so every column is a plain `Vec` indexed by `EntryId.0`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct EntryId(pub u32);

impl EntryId {
    /// Sentinel meaning "no entry" (used as the parent of volume roots).
    pub const NONE: EntryId = EntryId(u32::MAX);

    /// Row index for column access.
    #[inline]
    pub const fn idx(self) -> usize {
        self.0 as usize
    }

    /// `true` unless this is [`EntryId::NONE`].
    #[inline]
    pub const fn is_some(self) -> bool {
        self.0 != u32::MAX
    }
}

impl fmt::Debug for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_some() { write!(f, "#{}", self.0) } else { f.write_str("#NONE") }
    }
}

/// Identifier of an indexed volume (NTFS/ReFS volume, folder index root or file list).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
#[repr(transparent)]
pub struct VolumeId(pub u16);

impl VolumeId {
    /// Row index into the volume table.
    #[inline]
    pub const fn idx(self) -> usize {
        self.0 as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_not_some() {
        assert!(!EntryId::NONE.is_some());
        assert!(EntryId(0).is_some());
        assert_eq!(format!("{:?}", EntryId(7)), "#7");
        assert_eq!(format!("{:?}", EntryId::NONE), "#NONE");
    }

    #[test]
    fn sizes_are_minimal() {
        assert_eq!(std::mem::size_of::<EntryId>(), 4);
        assert_eq!(std::mem::size_of::<VolumeId>(), 2);
    }
}

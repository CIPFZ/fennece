//! Per-entry flag bits and Win32 file attribute constants.

/// Win32 `FILE_ATTRIBUTE_*` values (stored verbatim in the `attributes` column).
pub mod attr {
    /// Read-only.
    pub const READONLY: u32 = 0x0000_0001;
    /// Hidden.
    pub const HIDDEN: u32 = 0x0000_0002;
    /// System.
    pub const SYSTEM: u32 = 0x0000_0004;
    /// Directory.
    pub const DIRECTORY: u32 = 0x0000_0010;
    /// Archive.
    pub const ARCHIVE: u32 = 0x0000_0020;
    /// Device.
    pub const DEVICE: u32 = 0x0000_0040;
    /// Normal (no other attributes).
    pub const NORMAL: u32 = 0x0000_0080;
    /// Temporary.
    pub const TEMPORARY: u32 = 0x0000_0100;
    /// Sparse file.
    pub const SPARSE_FILE: u32 = 0x0000_0200;
    /// Reparse point (symlink, junction, ...).
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    /// Compressed.
    pub const COMPRESSED: u32 = 0x0000_0800;
    /// Offline.
    pub const OFFLINE: u32 = 0x0000_1000;
    /// Not content indexed.
    pub const NOT_CONTENT_INDEXED: u32 = 0x0000_2000;
    /// Encrypted.
    pub const ENCRYPTED: u32 = 0x0000_4000;
    /// Integrity stream (ReFS).
    pub const INTEGRITY_STREAM: u32 = 0x0000_8000;
    /// Virtual.
    pub const VIRTUAL: u32 = 0x0001_0000;
    /// No scrub data.
    pub const NO_SCRUB_DATA: u32 = 0x0002_0000;
    /// Recall on open.
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    /// Pinned.
    pub const PINNED: u32 = 0x0008_0000;
    /// Unpinned.
    pub const UNPINNED: u32 = 0x0010_0000;
    /// Recall on data access.
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    /// Attributes of reparse points that are not links: cloud placeholders (OneDrive,
    /// `IO_REPARSE_TAG_CLOUD_*`: recall/pinned/offline) and deduplicated or
    /// single-instance files (sparse). Symbolic links and junctions never carry them.
    pub const NON_LINK_REPARSE: u32 =
        OFFLINE | SPARSE_FILE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS | PINNED | UNPINNED;

    /// `true` if `a` describes a reparse point that is probably a symbolic link, junction
    /// or mount point (the index has no reparse tags).
    pub const fn is_link_reparse(a: u32) -> bool {
        a & REPARSE_POINT != 0 && a & NON_LINK_REPARSE == 0
    }
}

/// Compact per-entry flag set (`u16`), the hot subset of attributes plus index state.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct EntryFlags(pub u16);

impl EntryFlags {
    /// Entry is a folder.
    pub const IS_DIR: EntryFlags = EntryFlags(1 << 0);
    /// Hidden attribute.
    pub const HIDDEN: EntryFlags = EntryFlags(1 << 1);
    /// System attribute.
    pub const SYSTEM: EntryFlags = EntryFlags(1 << 2);
    /// Read-only attribute.
    pub const READONLY: EntryFlags = EntryFlags(1 << 3);
    /// Reparse point.
    pub const REPARSE: EntryFlags = EntryFlags(1 << 4);
    /// Compressed.
    pub const COMPRESSED: EntryFlags = EntryFlags(1 << 5);
    /// Encrypted.
    pub const ENCRYPTED: EntryFlags = EntryFlags(1 << 6);
    /// Offline (attribute, or entry from an offline volume / file list).
    pub const OFFLINE: EntryFlags = EntryFlags(1 << 7);
    /// Temporary.
    pub const TEMP: EntryFlags = EntryFlags(1 << 8);
    /// Sparse.
    pub const SPARSE: EntryFlags = EntryFlags(1 << 9);
    /// Not content indexed.
    pub const NOT_CONTENT_INDEXED: EntryFlags = EntryFlags(1 << 10);
    /// Tombstone: the entry was deleted and must be skipped by every scan.
    pub const DELETED: EntryFlags = EntryFlags(1 << 11);
    /// Additional name of a hard-linked file (same FRN as another entry).
    pub const HARDLINK_ALIAS: EntryFlags = EntryFlags(1 << 12);
    /// Entry is a volume root.
    pub const ROOT: EntryFlags = EntryFlags(1 << 13);

    /// Empty set.
    pub const fn empty() -> Self {
        EntryFlags(0)
    }

    /// `true` if every bit of `other` is set.
    #[inline]
    pub const fn contains(self, other: EntryFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` if any bit of `other` is set.
    #[inline]
    pub const fn intersects(self, other: EntryFlags) -> bool {
        self.0 & other.0 != 0
    }

    /// Sets or clears `other`.
    #[inline]
    pub fn set(&mut self, other: EntryFlags, on: bool) {
        if on {
            self.0 |= other.0;
        } else {
            self.0 &= !other.0;
        }
    }

    /// Union.
    #[inline]
    #[must_use]
    pub const fn union(self, other: EntryFlags) -> Self {
        EntryFlags(self.0 | other.0)
    }

    /// Derives the flag subset from raw Win32 attributes.
    pub const fn from_attributes(a: u32) -> Self {
        let mut f = 0u16;
        if a & attr::DIRECTORY != 0 {
            f |= Self::IS_DIR.0;
        }
        if a & attr::HIDDEN != 0 {
            f |= Self::HIDDEN.0;
        }
        if a & attr::SYSTEM != 0 {
            f |= Self::SYSTEM.0;
        }
        if a & attr::READONLY != 0 {
            f |= Self::READONLY.0;
        }
        if a & attr::REPARSE_POINT != 0 {
            f |= Self::REPARSE.0;
        }
        if a & attr::COMPRESSED != 0 {
            f |= Self::COMPRESSED.0;
        }
        if a & attr::ENCRYPTED != 0 {
            f |= Self::ENCRYPTED.0;
        }
        if a & attr::OFFLINE != 0 {
            f |= Self::OFFLINE.0;
        }
        if a & attr::TEMPORARY != 0 {
            f |= Self::TEMP.0;
        }
        if a & attr::SPARSE_FILE != 0 {
            f |= Self::SPARSE.0;
        }
        if a & attr::NOT_CONTENT_INDEXED != 0 {
            f |= Self::NOT_CONTENT_INDEXED.0;
        }
        EntryFlags(f)
    }

    /// Mask of the bits derived from attributes (everything else is index state).
    pub const ATTRIBUTE_MASK: EntryFlags = EntryFlags(
        Self::IS_DIR.0
            | Self::HIDDEN.0
            | Self::SYSTEM.0
            | Self::READONLY.0
            | Self::REPARSE.0
            | Self::COMPRESSED.0
            | Self::ENCRYPTED.0
            | Self::OFFLINE.0
            | Self::TEMP.0
            | Self::SPARSE.0
            | Self::NOT_CONTENT_INDEXED.0,
    );
}

impl std::fmt::Debug for EntryFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const NAMES: [&str; 14] = [
            "DIR",
            "HIDDEN",
            "SYSTEM",
            "READONLY",
            "REPARSE",
            "COMPRESSED",
            "ENCRYPTED",
            "OFFLINE",
            "TEMP",
            "SPARSE",
            "NCI",
            "DELETED",
            "HARDLINK",
            "ROOT",
        ];
        let mut first = true;
        f.write_str("EntryFlags(")?;
        for (i, n) in NAMES.iter().enumerate() {
            if self.0 & (1 << i) != 0 {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(n)?;
                first = false;
            }
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_mapping() {
        let f = EntryFlags::from_attributes(attr::DIRECTORY | attr::HIDDEN | attr::ARCHIVE);
        assert!(f.contains(EntryFlags::IS_DIR));
        assert!(f.contains(EntryFlags::HIDDEN));
        assert!(!f.contains(EntryFlags::SYSTEM));
        assert!(EntryFlags::ATTRIBUTE_MASK.contains(f));
    }

    #[test]
    fn set_and_clear() {
        let mut f = EntryFlags::empty();
        f.set(EntryFlags::DELETED, true);
        assert!(f.intersects(EntryFlags::DELETED));
        f.set(EntryFlags::DELETED, false);
        assert_eq!(f, EntryFlags::empty());
        assert_eq!(format!("{:?}", EntryFlags::IS_DIR.union(EntryFlags::ROOT)), "EntryFlags(DIR|ROOT)");
    }
}

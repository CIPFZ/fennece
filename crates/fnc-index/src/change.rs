//! Changes applied to the index (from USN records, folder watchers, file lists).

use fnc_core::{EntryId, FileTime, VolumeId};

/// Optional metadata carried by a record or change (`None` = unchanged / unknown).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// Win32 attributes.
    pub attributes: Option<u32>,
    /// File size in bytes.
    pub size: Option<u64>,
    /// Date modified.
    pub date_modified: Option<FileTime>,
    /// Date created.
    pub date_created: Option<FileTime>,
    /// Date accessed.
    pub date_accessed: Option<FileTime>,
}

impl Meta {
    /// Only attributes known (the MFT enumeration phase).
    pub fn attrs(attributes: u32) -> Self {
        Self { attributes: Some(attributes), ..Self::default() }
    }
}

/// How a change identifies its entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntryRef {
    /// Direct entry id.
    Id(EntryId),
    /// File reference number within a volume.
    Frn(VolumeId, u64),
}

/// One change to the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new file or folder.
    Create {
        /// Volume of the new entry.
        volume: VolumeId,
        /// Its FRN (or synthetic id).
        frn: u64,
        /// Its parent.
        parent: EntryRef,
        /// Name (UTF-16).
        name: Box<[u16]>,
        /// Metadata (attributes should be present).
        meta: Meta,
    },
    /// Entry deleted; folders delete their whole subtree.
    Delete(EntryRef),
    /// Entry renamed and/or moved.
    Rename {
        /// Entry.
        target: EntryRef,
        /// New name.
        new_name: Box<[u16]>,
        /// New parent.
        new_parent: EntryRef,
    },
    /// Metadata update.
    Meta {
        /// Entry.
        target: EntryRef,
        /// New values (`None` fields unchanged).
        meta: Meta,
    },
}

/// A batch applied under one write lock.
pub type ChangeBatch = Vec<Change>;

/// Outcome counters of [`crate::IndexStore::apply`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// Entries created.
    pub created: usize,
    /// Entries deleted (including subtree members).
    pub deleted: usize,
    /// Entries renamed or moved.
    pub renamed: usize,
    /// Metadata updates.
    pub updated: usize,
    /// Changes whose target or parent could not be resolved (ignored).
    pub unresolved: usize,
}

impl ApplyStats {
    /// `true` if the batch changed anything.
    pub fn changed(&self) -> bool {
        self.created + self.deleted + self.renamed + self.updated > 0
    }
}

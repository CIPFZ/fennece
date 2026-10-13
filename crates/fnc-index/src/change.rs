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
    /// "The file `frn` currently has the name `name` in `parent`" without implying a
    /// rename: from USN data/attribute records and folder listings. Unknown FRNs are
    /// inserted (self-healing); a known file seen under a name it does not have yet
    /// gets that name as a hard-link alias; otherwise only metadata is updated.
    Touch {
        /// Volume.
        volume: VolumeId,
        /// File reference number.
        frn: u64,
        /// Folder containing the name.
        parent: EntryRef,
        /// Name (UTF-16).
        name: Box<[u16]>,
        /// Metadata.
        meta: Meta,
    },
    /// A change addressed by path within `volume` (folder indexes).
    Path {
        /// Volume.
        volume: VolumeId,
        /// The change.
        change: PathChange,
    },
}

/// Path components below a volume root (`["Docs", "a.txt"]`); used by sources without
/// stable file ids (folder indexes).
pub type RelPath = Vec<Box<[u16]>>;

/// Path-addressed changes from folder indexes (SPEC ch.1 §3): entries are found by
/// walking names from the volume root; new entries get synthetic FRNs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathChange {
    /// The entry at `path` exists with `meta` (created if missing, else updated).
    Upsert {
        /// Location.
        path: RelPath,
        /// Metadata (attributes decide file vs folder for new entries).
        meta: Meta,
    },
    /// The entry at `path` (and its subtree) is gone.
    Remove {
        /// Location.
        path: RelPath,
    },
    /// Renamed/moved from `from` to `to`.
    Rename {
        /// Old location.
        from: RelPath,
        /// New location.
        to: RelPath,
    },
    /// Folder `dir` contains exactly `children` (rescan merge): missing ones are created,
    /// existing ones updated, others deleted. Entry ids of surviving entries are kept.
    SyncDir {
        /// Folder (empty = the root).
        dir: RelPath,
        /// `(name, metadata)` of every child.
        children: Vec<(Box<[u16]>, Meta)>,
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

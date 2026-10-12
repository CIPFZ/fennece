//! Volume table.

use fnc_core::{EntryId, VolumeId};

/// Kind of index source.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VolumeKind {
    /// NTFS volume indexed through the MFT and the USN journal.
    Ntfs,
    /// ReFS volume indexed through the USN journal.
    Refs,
    /// Folder index (directory walk + change notifications).
    Folder,
    /// Mounted EFU file list (always offline).
    FileList,
}

impl VolumeKind {
    /// Name used by `volume-kind:`.
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeKind::Ntfs => "ntfs",
            VolumeKind::Refs => "refs",
            VolumeKind::Folder => "folder",
            VolumeKind::FileList => "filelist",
        }
    }
}

/// Static description of a volume, supplied by the source when it is added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Source kind.
    pub kind: VolumeKind,
    /// Root path as shown to the user: `C:\`, `\\server\share\dir`, `/home/u`, `D:\b.efu`.
    pub root_path: String,
    /// Display name (volume label or file name).
    pub display_name: String,
    /// Volume serial number (NTFS/ReFS), 0 otherwise.
    pub serial: u32,
    /// Persistent identity: `\\?\Volume{GUID}\` for NTFS/ReFS, root path otherwise.
    pub guid: String,
    /// File system name (`NTFS`, `ReFS`, `FAT32`, ...).
    pub fs_name: String,
    /// Path separator used when joining names (`\` on Windows sources, `/` elsewhere).
    pub separator: char,
}

impl VolumeInfo {
    /// Convenience constructor for tests and simple sources.
    pub fn new(kind: VolumeKind, root_path: impl Into<String>) -> Self {
        let root_path = root_path.into();
        let separator = if root_path.starts_with('/') { '/' } else { '\\' };
        Self {
            kind,
            display_name: root_path.clone(),
            guid: root_path.clone(),
            root_path,
            serial: 0,
            fs_name: String::new(),
            separator,
        }
    }
}

/// A volume in the index.
#[derive(Clone, Debug)]
pub struct Volume {
    /// Id (index into the volume table).
    pub id: VolumeId,
    /// Static information.
    pub info: VolumeInfo,
    /// Root entries of this volume (file lists may contain several drive roots).
    pub roots: Vec<EntryId>,
    /// USN journal id (NTFS/ReFS).
    pub usn_journal_id: u64,
    /// Next USN to read (NTFS/ReFS).
    pub next_usn: i64,
    /// Whether the volume is currently reachable.
    pub online: bool,
    /// Whether the volume takes part in searches.
    pub include_in_index: bool,
}

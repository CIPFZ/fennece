# fnc-ntfs

NTFS/ReFS backend (SPEC ch.1 §2). No directory recursion is used to index NTFS volumes.

**Responsibilities**
- `record`: `USN_RECORD_V2/V3` parsing from raw bytes (portable, unit-tested).
- `journal`: folding USN records into index changes (create+delete cancel, rename upsert, refresh on close).
- `win` (Windows): `FSCTL_QUERY/CREATE_USN_JOURNAL`, `FSCTL_ENUM_USN_DATA`, `FSCTL_READ_USN_JOURNAL`,
  `OpenFileById` stat, `FindFirstFileExW` size/date back-fill, volume enumeration.
- `source` (Windows): `NtfsSource: VolumeSource`; journal position captured before enumeration.

**Invariants**
- The journal position is read *before* MFT enumeration, so no change between enumeration and watching is lost.
- Journal wrap / id change is reported as `SourceError::JournalReset` (caller rebuilds the volume).
- `unsafe` limited to `win.rs`; every block documents buffer ownership and sizes.

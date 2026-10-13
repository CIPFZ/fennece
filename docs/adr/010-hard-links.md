# ADR-010: Hard links from directory listings

- Status: accepted (M1, found during Windows testing)
- Related: SPEC ch.1 §2.1 step 7, ch.1 §2.2

## Background
`FSCTL_ENUM_USN_DATA` returns one record per file, i.e. one name per hard-linked file. On a
real Windows Server 2022 system disk this lost ~36k names, including the whole of
`C:\Windows\System32` that links into `WinSxS` (`System32\kernel32.dll` was unsearchable).
USN records name a file by whichever link was used, so treating every record as an upsert
also moved entries between their links.

## Decision
- The size/date back-fill (which lists every folder anyway) uses
  `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` instead of `FindFirstFileExW`; each
  entry carries its 64-bit file id (= FRN on NTFS). A listed name that is not in the index but
  whose id is a known FRN becomes an alias row (`HARDLINK_ALIAS`, same FRN) via the new
  `Change::Touch`. Unknown ids are inserted (self-healing).
- USN folding emits `Change::Create` (may rename/move) only for `FILE_CREATE` /
  `RENAME_NEW_NAME`; every other record emits `Touch` ("this name exists"), which never moves
  an entry and adds the name as an alias if it is new.
- The index keeps `(volume, frn) → aliases`; metadata updates and deletes by FRN reach every
  name; deleting the primary row promotes an alias.
- ReFS listings report truncated ids, so they are not used for aliases there.

## Alternatives
Parsing `$MFT` records (all `$FILE_NAME` attributes) — exact and faster, planned with the
`$MFT` reader in M2 (ADR-008); `FindFirstFileNameW` per file — one call per file, far too slow.

## Consequences
Measured: 36k aliases on C:, back-fill time unchanged (~2 s for 42k folders). Removing one link
of a multi-link file is not detected (SPEC-GAP in `journal.rs`); the alias disappears when the
file is deleted or on a rebuild.

# ADR-008: NTFS size/date back-fill via FindFirstFileExW (SPEC Q1)

- Status: accepted (M1); revisit in M2
- Related: SPEC ch.1 §2.1 step 6, ch.6 §8 Q1

## Background
`FSCTL_ENUM_USN_DATA` yields names, parents and attributes but no sizes or dates.

## Decision
Follow the SPEC Q1 preference: after the index is published (searchable), a per-volume thread lists every
folder with `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)` in chunks of 1024 folders,
matches children by fold-key hash, and applies `Change::Meta` batches (≤ 4096 per write lock). The status
bar shows the percentage. USN changes after enumeration refresh single files with `OpenFileById` +
`GetFileInformationByHandleEx` (no path needed).

## Alternatives
Parsing `$MFT` records directly (path B, `ntfs-reader`): several times faster, but must handle
non-resident attributes and hard links; evaluated in M2.

## Consequences
Sizes/dates appear progressively after the names (as in Everything 1.5). Children are matched by a 64-bit
hash of the fold key (SPEC-GAP note in code: a collision would assign metadata to a sibling).

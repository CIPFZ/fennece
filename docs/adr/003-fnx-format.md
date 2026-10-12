# ADR-003: `.fnx` database format

- Status: accepted for implementation in M2 (IDX-04/05)
- Related: SPEC ch.1 §5

## Background
Restart must be searchable within 2 s for 1 M entries, survive crashes (atomic writes), detect corruption,
and let the USN journal be replayed from the saved position.

## Decision
Little-endian, 8-byte aligned, self-describing segments:

```text
Header   { magic "FNX1", version u32, flags u32, created FILETIME, instance-independent }
Segments (each 8-byte aligned, written in this order):
  VOLS   volume table: kind, root path, guid, serial, fs, separator, usn_journal_id, next_usn, roots
  NAMD/NAMO/NAML   NameArena data (u16[]), offsets (u32[]), lengths (u16[])
  FLDD/FLDO/FLDL   FoldArena data (u8[]), offsets, lengths
  PARN VOLM FLAG FRN_ EXTI SIZE DMOD DCRE DACC ATTR   one column per segment (u32/u16/u16/u64/u16/u64/i64…)
  EXTD   extension dictionary
  RANK   name fast-sort ranks (optional; rebuilt if absent or stale)
  RUNH RCHG   run history / index journal timestamps (sparse: id + value pairs)
Footer   { segment directory: tag u32, offset u64, length u64, xxh3-64 u64; count; footer xxh3; magic }
```

- Load: `memmap2`, verify footer + every segment checksum, then copy columns into `Vec`s (they are mutable;
  ~100 MB copy ≈ tens of ms). Children CSR and FRN maps are rebuilt (O(n)), not stored.
- Save: write `index.fnx.tmp`, `fsync`, rename over `index.fnx`, keep the previous file as `index.fnx.bak`.
  On exit, every 5 min (`db_save_interval`), and `next_usn` checkpoints more often.
- Validation per volume on load: GUID + serial + `UsnJournalID`; mismatch or a journal that no longer
  contains `next_usn` → rebuild that volume only (others are replayed).
- Version bump on any layout change; unknown versions or checksum failures → full rebuild (logged, file kept
  as `.bad`).

## Alternatives
- serde/rmp-serde of the whole store: simple but slow to load and requires full deserialisation.
- Zero-copy mmap of every column: fastest load, but columns must be mutable (USN updates) → copy-on-write
  complexity; deferred until measurements show the copy matters.

## Consequences
Format is private to Fennec (SPEC non-goal: no Everything.db compatibility); EFU covers interchange.

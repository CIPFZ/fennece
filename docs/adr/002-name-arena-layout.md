# ADR-002: Name arena layout and fold keys

- Status: accepted (M0)
- Related: SPEC ch.0 §6.2, ch.1 §1.1/§1.3/§6.1; AGENT iron rule 4

## Background
1 M entries must fit in < 150 MB without a `String` per entry; names must be lossless with NTFS
(UTF-16, including unpaired surrogates); substring search over all names must take < 10 ms.

## Decision
- Two append-only pools indexed by entry row (`EntryId.0`):
  - `NameArena` = `Vec<u16>` data + `off: Vec<u32>` + `len: Vec<u16>` (original names, UTF-16);
  - `FoldArena` = `Vec<u8>` data + `off` + `len` (fold keys, **UTF-8**).
- Fold key = NFKD → drop combining marks → lowercase/full case folding specials (`ß/ẞ→ss`, `ς→σ`)
  → base-letter table (`æ→ae`, `œ→oe`, `ø→o`, …). ASCII names take a byte-wise fast path.
- Rename: overwrite in place when the new value is not longer, otherwise append; a garbage counter drives
  `compact()`, which also renumbers ids after deletions.
- Matching runs on fold bytes with `memchr::memmem` (SIMD); case/diacritic-sensitive modifiers re-fold the
  UTF-16 name into a per-thread scratch buffer (slow path, only when requested).

## Deviation from SPEC
SPEC §1.1 suggests sharing the fold key with the name when identical. With UTF-16 names and UTF-8 fold
keys sharing is impossible; the cost is ~1 byte per ASCII character (≈ 12 MB per million names).
Measured total index heap for the 1 M fixture: **104.7 MiB** (budget 150), see `bench/results.md`.

## Alternatives
- UTF-16 fold keys (shareable): 2× the scan bandwidth and no byte-oriented SIMD search.
- Fold keys only (no original names): loses case and the exact name for display/`case:`.
- Latin-1 compaction of names: saves memory but complicates every accessor.

## Consequences
- Items are limited to 65 535 units (NTFS names are ≤ 255); arenas to 4 Gi units.
- Highlighting maps fold-space matches back with `fold_str_mapped` (visible rows only).

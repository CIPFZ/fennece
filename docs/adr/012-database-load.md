# ADR-012: Reading `.fnx` into memory instead of mmap

- Status: accepted (M2), amends ADR-003; unreadable files: superseded by ADR-038 (`.v<N>.bak` / `.corrupt.bak` instead of `.bad`)
- Related: SPEC ch.1 §5

## Background
ADR-003 planned `memmap2` + copy of the mutable columns. Every column is mutable (USN updates
touch names, parents, sizes, dates, flags), so all of them are copied anyway, and `fnc-index`
forbids `unsafe` (mapping a file that another process may truncate is undefined behaviour in
Rust's model without extra care).

## Decision
`persist::load` reads the file with one `std::fs::read`, verifies the footer and every segment
checksum, decodes the columns into `Vec`s, validates every stored id, and rebuilds derived
structures (FRN maps, children CSR, counters); complete name ranks are stored and reused. Saves
write all rows including tombstones (no renumbering of the live index) and compact on load when
≥ 20 % are tombstones. Atomic replace: `index.fnx.tmp` + `fsync` + rename, previous file kept as
`index.fnx.bak`; an unreadable file is renamed `index.fnx.bad` and the index rebuilt.

## Alternatives
Zero-copy mmap with copy-on-write columns — more complex, only worthwhile if load time matters
more than it does now.

## Consequences
Measured on Windows: 43 MiB database for 360k entries; restart to searchable 0.19 s including
journal replay (IDX-04 budget 2 s). A byte-flip test over the whole file shows every corruption is
detected or harmless.

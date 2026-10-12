# fnc-core

Data model shared by every Fennec crate.

**Responsibilities**
- Identifiers: `EntryId(u32)`, `VolumeId(u16)`, `FileTime` (Windows FILETIME, UTC).
- String pools: `NameArena` (UTF-16, lossless with NTFS) and `FoldArena` (UTF-8 fold keys) — see ADR-002.
- Fold keys: case fold + NFKD + diacritic stripping (`ẞ→ss`, `æ→ae`, full-width → ASCII).
- Natural ("logical") name comparison on fold keys.
- `Settings` with `#[derive(Settings)]` metadata and `settings.toml` persistence.
- `tracing` initialisation (daily rolling files), data directory resolution (installed / portable / instance).

**Invariants**
- No per-entry `String`: names live only in arenas; arena items ≤ 65 535 units.
- `fold_str` and `fold_utf16_into` produce identical keys for the same text (property-tested).
- `natural_cmp` is a total order consistent with `name_cmp` (antisymmetric, transitive; property-tested).
- Every setting has a key, a default equal to Everything's default where one exists, a page and a description.
- `#![forbid(unsafe_code)]`.

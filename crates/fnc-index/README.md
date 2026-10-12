# fnc-index

The in-memory columnar index (SPEC ch.1 §1, §6).

**Responsibilities**
- `IndexStore`: SoA columns (name/fold arenas, parent, volume, flags, frn, ext id, size, dates, attributes),
  run history, index-journal timestamps, per-volume FRN maps, children CSR + overlay, name fast-sort ranks.
- `BulkLoader`: initial enumeration in any record order (MFT order), parent resolution, orphans → `$Orphans`,
  parent-cycle breaking.
- `Change` / `apply`: create, delete (whole subtree), rename/move, metadata; `compact` reclaims tombstones.
- `efu`: Everything file list reader/writer.
- `SharedIndex`: `RwLock` with owned, `Send` read snapshots.

**Invariants**
- `EntryId`s are dense row numbers; deleted rows stay as tombstones until `compact` (which changes
  `instance()`), so result sets never dangle.
- For every live entry `c` with parent `p`, `c ∈ children(p)` (property-tested against a rebuild).
- `generation()` increases on every visible change; `tree_generation()` on folder/tree changes.
- No full paths are stored: paths are joined from the parent chain without allocation.
- Write batches are kept small (≤ 4096 changes) so the write lock is held < 5 ms.

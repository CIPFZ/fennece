# ADR-005: Index snapshots and the children index API

- Status: accepted (M1)
- Related: SPEC ch.0 §4.3, ch.1 §6, ch.6 §2 (interface contract)

## Background
The contract sketches `IndexStore::snapshot(&self) -> IndexSnapshot` as "generation + Arc columns" and
`children(&self, dir) -> &[EntryId]`.

## Decision
1. **Snapshot = owned read guard.** `SharedIndex` wraps `Arc<parking_lot::RwLock<IndexStore>>`;
   `snapshot()` returns `ArcRwLockReadGuard` (`Send`), so a query runs on worker threads holding a consistent
   view. Writers apply batches of ≤ 4096 changes (lock < 5 ms). The UI uses `try_snapshot_for(2 ms)`.
2. **Children = CSR + overlay, exposed as an iterator.** Bulk loads build a CSR (counting sort, O(n));
   later creates/moves append to a per-parent overlay; stale CSR entries are filtered by
   `parent[c] == dir && !deleted`. `children(dir)` therefore returns `impl Iterator<Item = EntryId>`.

## Alternatives
- Arc-per-column copy-on-write snapshots: every USN batch during a running query would clone whole columns
  (tens of MB) — rejected.
- `Vec<Vec<EntryId>>` children: one heap allocation per folder; the CSR uses 8 bytes per entry total.

## Consequences
- Deviation from the contract signatures is recorded here; semantics (consistent reads, O(children))
  are preserved. Moves never need search-and-remove; `compact()` rebuilds the CSR and drops the overlay.
- Result sets store `(instance, generation)`; reuse and row caches check both.

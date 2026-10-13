# ADR-015: Property/content stores and their place in the search

- Status: accepted (M3, IDX-08/09)
- Related: SPEC ch.1 §8, §9, §11, ch.2 §5.2–5.9, §8

## Background
Property and content functions must be instant when indexed, `unknown` (or slow, on demand)
when not, cost nothing for plain searches, survive restarts and index compaction, and be
shared between the app's extraction thread, the search thread and the UI. `dupe:` and
friends work on the sorted result set (Everything 1.5).

## Decision
- **Stores** (`fnc-props`): `RowMap` gives each extracted entry a dense row; properties are
  typed columns over rows (`i64`, `f64`, interned text, interned text lists). O(1) lookup,
  memory proportional to extracted files. An entry with a row was extracted for every
  configured property (missing value = known absent); no row = not indexed. Stores are bound
  to `IndexStore::instance()`, follow `compact()` via `remap`, and persist (`props.fnx`,
  `content.fnx`) with rows identified by volume root + FRN + a check hash of name, parent
  name, size and date modified, so they re-attach to a reloaded or compacted index and drop
  changed files.
- **Sharing**: `PropertyIndex` / `ContentIndex` = `Arc<RwLock<_>>`; the engine takes owned
  read guards (after the index snapshot) for one search; writers keep batches short; the UI
  uses `try_snapshot_for`. Lock order index → props → content; never take the index lock
  while holding a store write guard.
- **Evaluation** (`fnc-exec::ext`): indexed data is turned into per-entry bit sets when the
  query is compiled (one pass over the store's rows; text predicates evaluated once per
  dictionary value; `content:` via the inverted index), so the scan costs a bit test. Entries
  outside the stores go through `OnDemand` (the app installs `DefaultOnDemand` with the
  Windows Property System provider) when `SlowPolicy` allows, with one file budget per search
  (default 10,000 reads), cancellation, and a cross-query cache keyed by file identity.
  Without a source the leaf answers "unknown" and the search reports `NotIndexed`.
- **Set functions** are hoisted to `PlannedQuery::post` and applied after sorting
  (`execute_filter` → sort → `apply_post`); the pre-set ids are kept so a re-sort re-applies
  them. Hash keys are computed only for entries sharing size (and earlier keys).
- **Zero overhead**: plans without such predicates build no extension state; results record
  the stores' generation (`ResultSet::ext_generation`) so cached results and narrowing are
  invalidated by store updates.
- **API compatibility with the app crate** (edited concurrently): property sort columns are
  `SortSpec::props` (per key) instead of a new `SortColumn` variant (the app matches
  `SortColumn` exhaustively); warnings are structured (`ExecStats::warnings`) and also
  appended as text to `ExecStats::errors` (what the status bar shows today); no new
  `EngineEvent`/`PlanDiag` variants.

## Alternatives
- `HashMap<EntryId, Value>` per property: simpler, 3–5× the memory, no dictionary-level
  evaluation of text predicates.
- Evaluating property predicates per entry through a trait object: no compile-time pass,
  but a virtual call and a hash lookup per entry on the hot path.

## Consequences
`width:>4000` over 1M entries with 100k extracted images: ~2 ms. The app must call
`remap` right after `IndexStore::compact()` (until then the store is ignored, not wrong).
`SortColumn::Property` can replace `SortSpec::props` once the app handles it.

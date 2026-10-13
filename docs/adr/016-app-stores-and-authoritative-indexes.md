# ADR-016: Stores in the application — lifecycle, extraction and authoritative indexes

- Status: accepted (M3, IDX-08/09)
- Related: SPEC ch.1 §8, §9, §11, ch.2 §8.6, ch.3 §4.3/§4.6/§5; ADR-013, ADR-015

## Background
`fnc-props` provides the stores, extractors and the slow path; the app (`fennec.exe`) has to
create them, keep them in step with the live index, extract in the background, persist them
with the index and show them in the UI, without blocking the UI thread or slowing literal
searches. The library's slow path reads every entry a store does not cover (up to 10,000
files per search): with a property index on `D:\Photos`, `width:>4000` on a 300k-entry
machine would read 10,000 unrelated files and stop with a warning — not "instant" (IDX-08);
the same holds for `content:` and a content index (IDX-09).

## Decision
- **One `Stores` object** (`fnc-app::stores`) owns `PropertyIndex`, `ContentIndex`,
  `HashCache` and the `DefaultOnDemand` source (Windows Property System fallback,
  `fnc-platform::props`, ANSI code page from `GetACP`). Handles are created once;
  `Stores::apply_options` installs the same `Arc`s into every `SearchOptions`. The content
  handle is only installed while the content index is on.
- **Indexer hooks** (`indexer::IndexHooks`): `attach` runs on the indexer thread on the
  final private store right before it is published, after the start-up compaction, so
  `props.fnx`/`content.fnx` are decoded against the published id space (searchable at the
  first published frame, nothing to remap; FRN + check hash re-attach rows even after a
  rebuilt volume); `compacted` remaps attached stores (any later compaction); `ready`
  (sizes/dates complete) starts the first full scan; `changed` reports entries touched by
  live USN/watcher batches (collected around `IndexStore::apply` only while an index is on);
  `save` writes the stores with the same snapshot as `index.fnx` (serialised under read
  guards, written without them); `stop` ends the worker.
- **Worker**: one thread in `THREAD_MODE_BACKGROUND_BEGIN`. Full scan in 32k-entry chunks
  (extension filter before any path is built); live changes debounced 2 s (max 10 s):
  data key (size, date modified) changed → drop + re-extract, renamed → re-check scope,
  moved folders → their subtree, dead rows swept. Extraction of 64 files runs without locks,
  results go in under short write guards (index → props → content); failed/binary files are
  stored as empty text so they are not retried; cancellation by configuration epoch;
  refresh of the visible search at most once a second.
- **Authoritative indexes**: new `SlowPolicy::read_uncovered` (library default `true`, the
  app sets `false`): when every source of a property is indexed, or for `content:` with a
  content store attached, entries the store does not cover are absent instead of read from
  disk; `notindexed:` and the raw-encoding functions still read. Unindexed properties keep
  the on-demand path with the per-search budget (`slow_search_max_files`).
- **UI**: property columns are `ColumnId::Property(PropertyId)` with the canonical name as
  setting key and `SortKey::Property` for sorting (`SortSpec::by_property`/
  `push_property_key`), no new `SortColumn` variant (ADR-015); the duplicate group column
  is added automatically while a `dupe:` result is shown; the preview uses the extracted text
  for `content:` searches and scrolls to the first hit.

## Alternatives
- Loading the stores after publishing (in the worker): no delay before the index is
  searchable, but a gap in which indexed properties are unknown and slow-path reads start;
  measured attach time on the VM is small enough to do it before publishing.
- Passing the scope to the engine instead of `read_uncovered`: exact for out-of-scope files,
  but every engine would need the app's folder rules; Everything's include folders behave
  like the authoritative mode.
- Per-entry change notifications from `IndexStore::apply` (an API change in `fnc-index`):
  the watch sink resolves the references before/after `apply` itself.

## Consequences
With both indexes off (default) the worker sleeps and no changes are collected (zero
overhead). Files that are in scope but not extracted yet are absent from `width:` results
until the background extraction reaches them. Folder-index entries get new FRNs on every
start (parallel walk), so their rows are re-extracted after a restart.

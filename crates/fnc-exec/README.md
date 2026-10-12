# fnc-exec

Search execution (SPEC ch.1 §6.1, ch.2 §8).

**Responsibilities**
- Matchers: SIMD substring (`memchr::memmem`) on fold keys, prefix/suffix/exact, wildcards (`*`, `**`, `?`,
  `[sets]`) and regex (Rust `regex`), whole-word, ignore punctuation/whitespace, case/diacritics variants.
- Compilation with precomputed folder sets (`parent:`, `ancestor:`, `child:`), child counts, folder path cache.
- Parallel 64K-entry block scans (rayon) with cooperative cancellation; reuse of the previous result set.
- Sorting (name fast-sort ranks, folders first, path/size/date columns); highlighting for visible rows.
- `SearchEngine`: background search thread, newest-query-wins, refresh on index change.

**Invariants**
- Hot paths (per-entry matching with default options, path joining, name comparison) do not allocate
  (`tests/alloc.rs`, dhat).
- A cancelled query never produces a result; results carry the index `(instance, generation)` they belong to.

# ADR-021: Semantic index design

- Status: accepted (M5)
- Related: SPEC ch.5 §3–§6, ADR-020 (llama.cpp)

## Decisions
1. **Keys are paths, not entry ids.** The chunk table stores the document's full path, size
   and date modified. Entry ids change on compaction/re-indexing; paths survive both. A query
   maps hits back with `IndexStore::find_path` (≤ 5000 lookups, sub-millisecond each).
2. **One usearch index for all kinds** (cosine, int8, M = 16) with the kind in the chunk
   table and `filtered_search` for `semname:`/`semcontent:`/`semcode:`. SPEC suggests one
   file per kind; one graph is smaller and a kind filter costs little at these sizes.
   usearch's optional numkong kernels are off (their C sources do not build under MSVC on a
   GBK code page); usearch's own i8 cosine is used.
3. **Change detection by polling the file index** (generation changes) instead of USN
   events: the semantic indexer diffs its scope against the index (size, date modified),
   embeds new files at once, changed files after a 10 s debounce, and drops deleted files
   immediately. It works the same for NTFS, folder and file-list volumes. Unchanged chunks
   (same kind and xxh3 of their text) keep their vectors.
4. **CPU limit = thread count**: the index lane's llama context gets
   `cores × semantic_cpu_limit %` threads (default 25 %), the query lane its own context
   with up to 8 threads, so typing never waits behind a batch. llama.cpp's worker threads
   do not inherit a background priority; THREAD_MODE_BACKGROUND_BEGIN is therefore not
   used (SPEC-GAP, recorded in ch.2 §9).
5. **Model or dimension change starts an empty store**, which is rebuilt in the background.
   Old vectors cannot be compared with new query vectors, and keeping both models in memory
   doubles the footprint, so semantic results come from the growing new index meanwhile.
6. **Text comes from the content extractor** of the content index (encoding detection,
   HTML, OOXML, PDF); no second set of parsers.
7. **Pictures (`semimage:`) are not embedded**: the llama.cpp multimodal projector path for
   embedding models is not exposed by `llama-cpp-2`. `semimage:` answers with a clear
   message (SPEC [MAY]).
8. **Download with the system `curl`** (Windows 10+ ships `curl.exe`) to `models/<file>.part`,
   resumable, then size and SHA-256 are checked before the rename. No HTTP client crate.
9. **Execution**: `sem:` leaves run last. As a top-level conjunct, fewer than 5000 literal
   candidates are scored exactly (their stored vectors), otherwise ANN top-k is intersected
   and k grows ×4 up to 5000. Nested terms become fixed ANN hit sets. `?sem:` and the
   "Semantic" toggle fuse literal and semantic rankings with RRF (k = 60).

## Consequences
Semantic search is off by default and costs nothing then (no thread, no model, no
`SearchOptions::semantic`). Moving a folder re-embeds its files (new paths).

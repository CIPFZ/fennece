# Changelog

All notable changes, one line per accepted unit (AGENT.md §4.1).

## M1 — MVP (2026-10-13)
- [M1][fnc-ntfs] USN_RECORD_V2/V3 parser and USN event folding (portable, tested with byte samples).
- [M1][fnc-ntfs] Win32 MFT enumeration (`FSCTL_ENUM_USN_DATA`), journal query/create/read, `OpenFileById` stat, `FindFirstFileExW` back-fill; `NtfsSource`.
- [M1][fnc-platform] `VolumeSource` contract, folder walker, shell open/reveal, single instance (mutex + local socket), elevation check.
- [M1][fnc-app] eframe window: search-as-you-type, virtualised result table with click-to-sort and highlighting, status bar, keyboard shortcuts, tray icon and global hotkey (Windows), command forwarding, Fluent i18n (en, zh-CN), CJK font fallback.
- [M1][fnc-app] background indexer: private build → publish → size/date back-fill → USN watch with debounced refresh (IDX-02 pipeline latency ≈ 50 ms).
- [M1][fnc-fes] `fes` offline mode over EFU file lists (`-filelist`, `-n`, `-o`, `-sort`, `-csv`, `-get-result-count`).
- [M1][fnc-exec] `EngineHandle`, thread-pool sizing.

## M0 — Skeleton (2026-10-12)
- [M0][workspace] Cargo workspace with all crates and tools, pinned toolchain 1.99.0, CI (Linux + Windows + Windows cross-clippy), cargo-deny.
- [M0][fnc-core] ids, `FileTime`, UTF-16 `NameArena` / UTF-8 `FoldArena`, fold keys (`ẞ→ss`, `æ→ae`, NFKD), natural sort (CJK, digit runs), `#[derive(Settings)]` + `settings.toml`, rolling logs, data paths.
- [M0][fnc-index] columnar `IndexStore`, bulk loader, children CSR + overlay, FRN maps, change application, compaction, name fast-sort ranks, EFU reader/writer (IDX-03: 100 k-child folder rename < 50 ms).
- [M0][fnc-query] Everything 1.5 lexer/parser, AST round-trip, function/modifier/macro tables, size/date values, cost-based planner with result-set reuse.
- [M0][fnc-exec] matchers, parallel scans with cancellation, sorting, highlighting, search engine; dhat zero-allocation test for hot paths.
- [M0][tools] `gen-fixtures` (1 M synthetic entries), `bench` (criterion + baseline report), `gen-icons` (SVG/PNG/ICO/tray).
- [M0][docs] ADR-001…009.

## M1 — fixes from two independent code reviews (2026-10-13)
- [M1][fnc-query] `< >` nesting limit; comparison `<`/`>` only after known functions; no panics on non-ASCII / absurd dates; huge sizes clamp.
- [M1][fnc-index] batch-order-independent `apply` (retry unresolved); FrnMap overflow migration; `$Orphans` keeps dense maps; bounded walks; clones get a new instance id.
- [M1][fnc-ntfs] enumerate with HighUsn = MAX; every USN record upserts (self-healing); device-gone → offline; root id via FileIdInfo; fixed drives only (removable in M2).
- [M1][fnc-exec] submit under the token lock (no lost searches); per-query panic containment.
- [M1][fnc-app] concurrent back-fill and journal watch; single instance kept alive on Windows; tray/close-to-tray/start-hidden fixes; run count off the UI thread; exact-modifier shortcuts.

## M1 — verified on Windows Server 2022 (2026-10-13)
- [M1][fnc-ntfs] synthesize `$Extend`; hard-link names via `FileIdBothDirectoryInfo` back-fill; USN data records → `Touch` (ADR-010).
- [M1][fnc-index] `Change::Touch`, hard-link alias table, `apply_initial` (back-fill is not an index-journal change), shrink after back-fill.
- [M1][fnc-platform] Windows UI language detection; [fnc-app] keep search focus after sorting; `ntfs_end_to_end` real-volume test.
- [M1][bench] `bench/results-windows.md`; report `docs/reports/M1-windows.md`.

## M2
- [M2][fnc-query] date values: `this/last/next/past[N]<unit>` constants (year … second, quarter), `sept`/`tues`/`thurs`, Excel serial days, wildcard dates (`*-02`, `*-*-*T9:00..*-*-*T17:00`) as `DateWild`; `duration` module (`hh:mm:ss`, `mm:ss`, `90s`, `5min`, `1.5hours`, granularity).
- [M2][fnc-query] `<folder:>`: `<`/`>` are comparison operators only after numeric functions or `len:`/`count:`/`atoi:`/`atof:` (round-trip proptest regression); `len:>20` plans numerically.
- [M2][fnc-query][fnc-exec] `descendant:` `descendant[file|folder]count:` `sibling:` `parent-sibling:` `ancestor-sibling:` `siblingcount:` `foldersize:`/`fs:` `hardlinkcount:` `nameparts:` `type:` (injectable type-name provider, fallback "EXT File") `filelist:` `fileexists:`/`folderexists:` (`$name:` `$stem:` `$ext:` `$path:` `$parent:` `$fullpath:`, resolved in the index) `omit:` (`OmitSet`) `weight:` (ignored); per-generation caches (tree aggregates, link counts, name lookup, type descriptions).
- [M2][fnc-exec] `pinyin:`/`py:` `pinyinquan:` `pinyinjian:` (allocation-free NFA, heteronyms, `ü`=`v`/`u`), `ascii:`, `fuzzy:` (edit distance ≤ 1, Wu–Manber); `SearchOptions::{pinyin_default, omit, type_names}`, `PlanEnv::{pinyin_default, omit}`.
- [M2][fnc-query][fnc-exec] filters: `Filter`/`FilterSet`/`combine_filter`; `SearchRequest::filter: Option<FilterSet>` with per-filter case/ww/path/diacritics/regex and `and-filter:`/`or-filter:`; `Filter::user_macro` for macro-named filters.
- [M2][fnc-query] parser suite 422 table-driven cases (+62 planner cases); built-in macro lists checked against SPEC ch.2 §6; SPEC ch.2 §9 differences updated.
- [M2][fnc-index] `.fnx` database (`persist`): segmented, per-segment xxh3, atomic save with `.bak`, validated load, tombstone compaction; `clear_volume`/`drop_volume`/`absorb`; `Volume::metadata_complete`.
- [M2][fnc-ntfs][fnc-service] `VolumeAccess` (direct or service), framed read-only protocol (`proto`), `fennec-service` (windows-service, DACL SY/BA/IU), `-install-service`/`-uninstall-service` (IDX-10).
- [M2][fnc-app] indexer: load → resume per volume / rebuild changed journals (IDX-04/05) → publish; periodic + exit save; `-reindex` `-nodb` `-db`; volume arrival/removal monitor; pause updates.
- [M2][fnc-platform] `fileops`: Explorer context menu (IContextMenu2/3), CF_HDROP copy/cut/paste, drag out (DoDragDrop), IFileOperation delete/rename/copy/move/new folder, properties, Open With; shell type names; Ctrl+V/Shift+Del window subclass.
- [M2][fnc-exec] multi-level sort (3 keys), more sort columns; monomorphised single-key comparators.
- [M2][fnc-app] full UI: command system + Everything keymap with overrides, menu bar, 19 columns (chooser, reorder, widths), multi-selection, rename, context menu, drag & drop, clipboard, filters (combo, bar, manager, CSV import), bookmarks, search history, completion, search builder, export, `/key=value`, Options window (all pages, Keyboard, Advanced), theme switch, window geometry, permanent-delete confirmation.
- [M2][docs] report `docs/reports/M2.md`, ADR-011 (service), ADR-012 (database load), bench `results-windows.md`.

## M3
- [M3][fnc-props] property table (`PropertyId`, ~95 properties of SPEC ch.2 §5.5–5.9 with aliases, titles, types, units, `System.*` names), `Value` + display/format, `PropertyIndexConfig`/`ContentIndexConfig`/`Scope`.
- [M3][fnc-props] extractors: image header (`image`, no decoding) + EXIF (`kamadak-exif`), audio (`lofty`), MP4/MOV (`mp4parse` on `moov`, seeks over `mdat`), Matroska/WebM (EBML), PE + `VS_VERSIONINFO`, OOXML docProps, PDF `/Info`/pages (own reader); bounded + panic-contained; `PropertyProvider` fallback trait (ADR-014).
- [M3][fnc-props] `PropertyStore` (rows + typed/interned columns, remap on compaction, `props.fnx` re-attached by volume + FRN + identity check) and `PropertyIndex` handle (ADR-015).
- [M3][fnc-props] content: encoding detection (BOM/UTF-16/UTF-8/`chardetng`), HTML stripping, OOXML/PDF text; `ContentStore` (LZ4 blocks, token inverted index with substring tokens and CJK bigrams, line/word/char stats, `content.fnx`) (ADR-013); `HashCache` (CRC-32/CRC-64/MD5/SHA-1/SHA-256/SHA-512, `hashes.fnx`); `OnDemand`/`DefaultOnDemand`.
- [M3][fnc-query] every property name is a function (`FuncId::Prop`); property values with units, named values, float order keys, `dimensions:`, `aspect-ratio:`, `len:`/`count:`/`atoi:`/`atof:`/`allstrings:` on properties; `content:` `utf8content:` `ansicontent:` `utf16content:` `utf16becontent:` `textplain:` `binary:content:` `contenthits:`; hash functions; set functions hoisted to `PlannedQuery::post`; cost hints `PlanEnv::indexed_props`.
- [M3][fnc-exec] `ExtCtx` + `SearchOptions::{props, content, hashes, on_demand, slow}`: indexed data as compile-time bit sets, slow path with per-search file budget/cancellation/cross-query cache, `notindexed:`, `ExecStats::warnings`; `dupe:` `sizedupe:` `namepartdupe:` `attribdupe:` `dadupe:` `dcdupe:` `dmdupe:` `dupe:size;sha256` `distinct:` `unique:` `dupe-count:` `dupe-max:` after sorting with `ResultSet::dupe_group`; `ResultSet::content_hit`, `content_highlights`; `SortSpec::props` + `sort_ids_ext` (property columns).
- [M3][bench] `bench-report` M3 metrics: `width:>4000` over 1M entries (10 % with width) 2.0 ms; `content:"quarterly plan"` over 1M entries with 10k indexed documents 14 ms (IDX-09 budget 200 ms); literal queries unchanged (paired A/B within noise).
- [M3][docs] ADR-013 (content store), ADR-014 (extractor dependencies, own PDF reader), ADR-015 (stores and search integration); SPEC ch.2 §9 rows for property/content/hash/duplicate semantics.
- [M3][fnc-core] settings pages `properties`/`content`: `property_names` `property_folders` `property_extensions` `property_fast_sort` `property_read_unindexed` `slow_search_max_files` `content_enabled` `content_folders` `content_extensions` `content_max_file_mb` `content_max_total_mb` `content_office` `content_pdf` `content_read_unindexed`.
- [M3][fnc-app] `stores`: one `Stores` (props/content/hash handles + on-demand source, same `Arc`s for every search); `IndexHooks` (attach before publish, remap on compaction, ready, live changes, save with `index.fnx`, stop); background-mode worker: full scan, 2 s debounced live changes, lock-free extraction in 64-file batches, cancellation, status bar progress (ADR-016).
- [M3][fnc-app] `props.fnx`/`content.fnx`/`hashes.fnx` saved with the periodic, `-save-db` and exit saves; re-attached in 12 ms on restart (72 photos + 10,000 notes).
- [M3][fnc-platform] `props`: Windows Property System `PropertyProvider` (`SHGetPropertyStoreFromParsingName` + `IPropertyStore`, unit conversion); `privilege::enter_background_mode`, `locale::ansi_code_page`.
- [M3][fnc-exec] `SlowPolicy::read_uncovered`: indexed properties and the content index can be authoritative (no disk reads outside their scope; `notindexed:` still reads).
- [M3][fnc-app] UI: ~95 sortable property columns (chooser submenu, `width:90` column keys, `width-descending` sort keys, `-sort width`), localised titles/values; duplicate group column for `dupe:` results; preview shows extracted text with all `content:` matches highlighted and scrolls to the first hit, plus indexed properties; localised `ExecStats::warnings`.
- [M3][fnc-app] verified on Windows Server 2022 (388k NTFS entries): IDX-08 `width:>4000` 1.0 ms p50 / 1.2 ms max, IDX-09 `content:"quarterly plan"` 1.8 ms p50 / 2.1 ms max (`ntfs_property_and_content_index`); report `docs/reports/M3.md`, ADR-016.

## M4
- [M4][fnc-ipc] native protocol on `Fennec.<instance>.ipc`: length-prefixed MessagePack, strict decoding, columnar results, paging (`QueryContinue`), counts/sizes/versions/status/run counts/admin/volumes/filters/bookmarks; `IndexBackend` + `Host` (queries on snapshots, cache, time limits); threaded server with connection limit and SY/BA/IU pipe DACL; blocking `Client` with timeouts (ADR-030).
- [M4][fnc-ipc] Everything 1.4 compatibility: `QUERYW`/`QUERY2` (W/A) decoding, `LISTW`/`LIST2` encoding with all request flags, run-count copydata, `EVERYTHING_WM_IPC` commands, `compat_window` example (reference Win32 plumbing); verified with official es.exe 1.1.0.38 and Everything64.dll (ADR-031, INT-03).
- [M4][fnc-ipc] `export`: EFU/CSV/TSV/TXT/M3U/M3U8/JSON/plain writers with es rules, `export_entries` for the app (ADR-033, INT-07).
- [M4][fnc-fes] es.exe 1.1 compatible `fes` over IPC: full switch grammar, sorts, columns, formats, exports, info/run-count/lifecycle commands, settings file; `-offline` (`index.fnx`) and `-filelist`; exit codes 0/1/2/3 (+5/7/9) (ADR-033, INT-02).
- [M4][fnc-http] HTTP server: search page, Everything JSON, index-resolved downloads, `/api/v1/status|volumes`; Basic auth, limits, timeouts, CSP, static root (ADR-032, INT-04).
- [M4][fnc-sdk] `fennec_sdk` Rust builder API and Everything-SDK style C ABI with `include/fennec_sdk.h`.
- [M4][docs] ADR-030…033; SPEC ch.2 §9 rows for fes/compat/HTTP differences.
- [M4][fnc-query][fnc-exec] `count:<max>` result limit (`FuncId::Count` → `SetKind::Limit`, applied last after sorting) in every front end; `count:` before anything but digits stays the comparison modifier; the IPC backend's `extract_count_limit` is gone.
- [M4][fnc-platform] `compat`: hidden Everything-compatible window on its own thread (`CompatHandler`, query worker sending the reply `WM_COPYDATA`, conflict detection, `EVERYTHING_IPC_CREATED`, client `WM_CLOSE` = exit); `session`: session id / integrity level of IPC clients (ADR-034).
- [M4][fnc-core] settings page `server`: `ipc_server`, `everything_ipc`, `http_enabled` `http_bind` `http_port` `http_username` `http_password` `http_allow_downloads` `http_page_size` `http_max_results` `http_max_concurrency` `http_timeout_ms` `http_static_root` `http_access_log`.
- [M4][fnc-app] `ipc_host::AppHost` (filters, bookmarks, options, status, run counts, `DeleteRunHistory`, same-session authorization) and `servers` (IPC pipe, Everything window, HTTP started/stopped from the settings on `fennec-servers`); HTTP address/error in the status bar (ADR-034, INT-02/03/04/05).
- [M4][fnc-app] File → Export and Copy as CSV/EFU through `fnc_ipc::export` (EFU/CSV/TSV/JSON/M3U/M3U8/TXT by extension, visible columns) (INT-07).
- [M4][fnc-app] Enter runs a `/key=value` inline command instead of opening the selection; passwords are masked in Options and never echoed.
- [M4][docs] ADR-034, `docs/reports/M4.md`.

## M5 — semantic search
- [M5][third_party] vendored `llama-cpp-sys-2`/`llama-cpp-2` 0.1.159 with llama.cpp master (EmbeddingGemma-2 `gemma-embedding2`), trimmed to 42 MB (ADR-020).
- [M5][fnc-semantic] `Embedder` trait, llama.cpp embedder with separate query/index contexts, Matryoshka truncation, `ModelHost` (lazy load, idle unload, failed-load cache), task prefixes, model registry (`models.toml`), chunker (noise-free file names, sentence-bounded text with overlap, code windows), usearch int8 cosine store with chunk reuse by text hash, checksummed save/load, compaction, background semantic indexer (scope, debounce, CPU limit, status), RRF (ADR-021).
- [M5][sem-eval] `sem-eval` (Recall@1/@10, MRR, query p95) and `tools/sem-corpus.py` (10k-document acceptance corpus).
- [M5][fnc-query] `sem:` `semantic:` `semname:` `semcontent:` `semcode:` `semimage:`, `sem:>0.6 text`, `sem-min-score:`, `sem-top:`, `?sem:`; semantic plans never reuse previous results.
- [M5][fnc-exec] `SemanticSource`; top-level terms run after the literal filters (exact scoring below 5000 candidates, widening ANN above), nested terms as ANN sets; `?sem:`/smart-mode fusion; `SortColumn::Relevance`; best chunk per hit.
- [M5][fnc-app] semantic search in the app: engine from settings, index catalog, model download with SHA-256 check, "Semantic" toggle, status bar, setup window, relevance column, preview of the matching passage, `semantic_gpu_layers` + `vulkan` feature (SEM-06/07 kittests).
- [M5][fnc-platform] `process::{cpu_time, private_bytes}` (CPU-limit and memory checks).
- [M5] measured: query embedding 32–85 ms (Linux, 8 threads) / p95 78 ms (Windows); ANN top-200 over 1M vectors p95 1.27 ms; sem-eval Recall@1 100 % on the sample corpus.

## M6 — polish
- [M6][fnc-query][fnc-exec] `eval:` formulas: Excel-style operators and ~70 functions over entry fields and every property; formula-aware lexing of `<`/`>`.
- [M6][fnc-app] custom columns 1–9 (`custom_columns`, `Title=formula`).
- [M6][fnc-app] tabs (Ctrl+T/W/Tab, per-tab search/sort/selection/engine, tab bar) and sessions (`restore_session`, `session.toml`).
- [M6][fnc-app] find bar (case/whole word/regex, F3/Shift+F3, `findbar_ctrl_f`).
- [M6][fnc-app] folder/bookmark sidebar (lazy tree, `"path\"` search, shell menu, drag-to-reorder bookmarks), customizable toolbar, back/forward per tab (Alt+Left/Right).
- [M6][fnc-app][fnc-platform] advanced rename (template variables, counter, literal/regex find-replace, case, trim, conflict preview, swap-safe steps) and undo/redo (Ctrl+Z/Ctrl+Y) of delete (recycle-bin restore via `undelete`), rename, new folder and advanced rename; cross-device trash on Linux.
- [M6][fnc-app] UI languages: en, zh-CN, zh-TW, ja, de, fr, ru, pl, tr; high-contrast theme.
- [M6][packaging] portable zip and WiX MSI (service, Explorer menu, autostart features); settings/Everything.ini import, filters/bookmarks/history CSV export.
- [M6] Linux backend: folder index + inotify, freedesktop trash; the app runs under X11 (verified on Xvfb).

## After M6 — closing the SPEC ch.2 §9 gaps (2026-10-13)
- [fnc-exec] plain name substrings scan with one `memmem` finder per term (Windows: `abc` −30 %, `a b c` −40 %).
- [fnc-app][fnc-index] live compaction (≥ 10 k and ≥ 20 % dead entries, every 30 s): clone, compact, swap; selection, omissions and open renames follow the id remap; writer epochs retire stale indexer threads.
- [fnc-index][fnc-platform] folder-index ids derived from parent id + folded name: properties/content of folder indexes survive restarts; changes between the walk and the watch are caught up.
- [fnc-app][fnc-ipc] rebuild the index while running (`Rebuild index` command, IPC `AdminCommand::Rebuild`, index-source settings changes).
- [fnc-service][fnc-platform] per-connection check of the client process: interactive session and at least medium integrity.
- [fnc-core][fnc-http] HTTP/ETP passwords stored as PBKDF2-HMAC-SHA256 (legacy plain values still accepted and rehashed on save).
- [fnc-platform][fnc-app] undo/redo of clipboard and drag-and-drop copies and moves (`IFileOperationProgressSink` results).
- [fnc-core][fnc-app] pinyin name order for Chinese interfaces (`name_collation`: auto/codepoint/pinyin; persisted ranks keyed by collation).
- [fnc-platform][fnc-query][fnc-app] date field order and first day of the week from the OS locale (`GetLocaleInfoEx`; `date_input_order`, `week_start`).
- [fnc-query][fnc-exec][fnc-props] `filelistfilename:`, `parent+N:`, `target:`/`linktarget:`/`shortcuttarget:` (symlinks, junctions, pure-Rust `.lnk` MS-SHLLINK reader, `.url`), `fsi:`/`filesignature:` (magic-number table, OOXML/epub/jar/apk via ZIP headers) and `recycled:`/`recyclebinoriginalpath:` (`$I` records v1/v2); file-reading functions use the on-demand slow path (budget, cancellation, cross-query cache).
- [fnc-exec][fnc-app] custom formula columns sort (`SortKey::Custom`, `SearchOptions::formulas`, `custom1-descending`); the Type column sorts by type description (`SortColumn::Type`).
- [fnc-props] property table completed to 305 entries (SPEC ch.2 appendix B): Windows shell properties with `System.*` names, types and units checked against the Windows Server 2022 schema, aliases, named values, titles in all nine languages; more EXIF/ID3/OOXML fields from the built-in parsers; column chooser filter box.
- [fnc-etp][fnc-app] ETP/FTP server (FTP subset over the index + Everything's `SITE EVERYTHING`/`EVERYTHING` search extension; password required, read-only, loopback by default; Options → ETP/FTP Server) and client (Tools → Connect to ETP Server, `-connect`: the server's results as a read-only view, downloads via `RETR`; ADR-036).
- [fnc-platform][fnc-app] IFilter content extraction in a host process (`fennec.exe -ifilter-host`, ADR-035): per-file timeout with kill/respawn, job object, low priority; fallback of `content:` and the content index for formats without a built-in parser (setting `content_ifilter`).
- [fnc-ipc][fnc-fes] es.exe / fes / HTTP searches use the window's property, content and hash stores and file access; `fes -offline` / `-filelist` load the saved stores.
- [fnc-exec] reparse points with cloud-placeholder or dedup attributes are not treated as links (`symlink:`, `junction:`, `target:`).
- [fnc-app] property columns read unindexed values for the visible rows in the background (`LazyCells`); link target, file signature and recycle bin original location columns.
- [fnc-app] folders on indexed NTFS/ReFS volumes or inside another indexed folder are not walked twice.
- [fnc-platform][fnc-query][fnc-exec][fnc-app] `si:` / `system-index:` query the Windows Search system index (AQS → SQL via `ISearchQueryHelper`, OLE DB `Search.CollatorDSO`); paths resolved to index entries once per search (cached 30 s), bounded, cancellable; localised warning when the service is unavailable.

## 0.2.0 — release
- [0.2.0] version 0.2.0 in every crate, the MSI, `-version`, the About dialog, the Everything IPC version (`1.4.1.200`) and the HTTP `Server` header (`Fennec/0.2.0`) (REL-01).
- [0.2.0][docs] ADR-037: `unsafe` scope (fnc-ntfs, fnc-platform, fnc-ipc, fnc-sdk, fnc-service; per-crate sites, purpose, lint overrides; new crates need an ADR); SPEC ch.6 §5 points to it (REL-10).
- [0.2.0][fnc-core][fnc-app][fnc-fes][fnc-service][fnc-platform] global panic hook in `fennec`, `fes`, `fennec-service` and the IFilter host: `logs/crash-<YYYYMMDD-HHMMSS>[-<binary>].log` with version, OS build, thread, message/location, backtrace, the last 20 searches (`crash_redact_queries`: length + hash only) and an index summary; reports of processes that ended normally become `*.caught.log`; the next start shows "Fennec did not exit normally last time — view log" in the status bar (click: open the folder, right-click: dismiss; `crash.ack`); Windows unhandled-exception filter writes a minimal report; hidden `-debug-panic` (REL-02).
- [0.2.0][fnc-platform][fnc-app] `fennec -version` / `-help` (and tool commands) print into the console they were started from in release builds (GUI subsystem: `AttachConsole(ATTACH_PARENT_PROCESS)` when stderr is not redirected; no console window from Explorer) (REL-01).
- [0.2.0][fnc-index][fnc-app] live compaction no longer starves under continuous changes: the copy is compacted under an upgradable read guard (searches and the UI keep reading, index writers wait for the compaction, then it is swapped in); before, every attempt was discarded while USN changes kept arriving, so tombstones grew without bound (found by the REL-05 soak trial).
- [0.2.0][fnc-app][fnc-exec][fnc-props] link target / file signature / recycle bin original location columns sortable: header click counts the unread files of the whole result, asks "N files need to be read (about X s)", reads them in the background (status bar progress, Cancel/Esc/new search cancel), then sorts (`SortKey::File`, `SortSpec::files`); values persist in `files.fnx` (file identity + size + date modified) (REL-06).
- [0.2.0][fnc-core][fnc-index][fnc-props][fnc-semantic][fnc-app] upgrade compatibility: every store reads its format versions `oldest..=current`; an older-unreadable, newer or damaged `index.fnx`/`props.fnx`/`content.fnx`/`hashes.fnx`/vector store is moved to `<name>.v<N>.bak`/`.corrupt.bak` (never deleted, never overwritten by a later save) and rebuilt with a localised status-bar notice; `settings.toml` gets `schema_version` (missing = 1; not a user option) with a table-driven `migrate`, newer schemas / bad values / syntax errors keep a copy and load what can be read; filters, bookmarks, search history and session files are versioned the same way; files written by 0.1.0 load unchanged (`tests/fixtures/0.1.0`) (REL-03, ADR-038).
- [0.2.0][fnc-props][fnc-app] `files.fnx` (REL-06 column value cache) follows the ADR-038 policy like the other stores (old/future/damaged file set aside as `.v<N>.bak`/`.corrupt.bak` with a notice).
- [packaging] build matrix: `build.ps1 -Variant standard|experimental|all` → `fennec-<ver>-x64-{standard,experimental}.msi` + `-portable.zip` + `SHA256SUMS.txt`; separate target folders, fes/service built once; MSI product name shows the variant, one upgrade code (variants replace each other, 0.1.0 upgrades); uninstall (not upgrades) removes `%ProgramData%\Fennec` (service logs) and, with the Remove-page checkbox or `REMOVEUSERDATA=1`, the user's `%APPDATA%\Fennec` (default keep) (REL-07).
- [fnc-core][fnc-app] portable copy in a read-only folder: data falls back to `%LOCALAPPDATA%\Fennec\portable` with a permanent status-bar warning; an unwritable log folder no longer stops start-up (REL-07).
- [0.2.0][fnc-ntfs] the NTFS size/date back-fill opens junctions, directory symlinks and mount points themselves instead of following them: their targets' children were added under the link (a junction to `C:\Windows\System32` grew a 10 k-entry volume to 319 k entries; found during REL-06).
- [0.2.0][fnc-exec][fnc-app] search refreshes after index changes no longer restart a search in flight and are rate-limited (gap = 4 × the last refresh, 250 ms – 1 s; new searches are never delayed); while the main window is hidden or minimised index changes do not re-run the search at all (caught up when shown). Before, a busy USN journal (REL-05 soak: 50 changes/s) kept 1.6–3 cores busy with the window in the tray.
- [fnc-semantic][fnc-app] turning semantic search off returns its memory: `SemanticEngine::unload` stops the indexer, saves and drops the vectors and chunk tables, the query cache and the model (lane threads joined, contexts and weights freed, the llama.cpp backend refcounted and freed with the last model) through the same release path as the idle unload; the `sem:` source held by views, tabs, windows, the IPC host and search threads references the engine weakly, so stale copies no longer keep it alive ("semantic search is off"); injectable clock for the idle-unload timer (REL-04).
- [soak] `tools/soak/`: `soak-churn` (token-bucket create/rename/delete churn with a 10,000-child folder and a periodically recreated subtree; `report` turns the samples into the markdown/SVG soak report), `soak.ps1` (starts fennec on the interactive desktop, samples memory/CPU/handles/threads/index size/crash files/compactions into CSV), `cpu-probe.ps1` (REL-05).

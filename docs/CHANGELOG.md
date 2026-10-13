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

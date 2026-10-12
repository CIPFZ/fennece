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

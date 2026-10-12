# M0/M1 — understanding and plan (written before implementation)

**Understanding.** M0 delivers a workspace whose layout, contracts, tooling and baselines are final, so
later milestones only add behaviour. M1 is the MVP: NTFS MFT enumeration + USN monitoring, the in-memory
index, a subset of the query language, parallel search with cancellation and reuse, and the egui window
(search box, virtual table, open / open path, status bar, tray, hotkey, single instance).

**Crates/modules to create.**
- `fnc-core`: ids, `FileTime`, `NameArena` (UTF-16) + `FoldArena` (UTF-8), fold keys, natural sort,
  `Settings` + `#[derive(Settings)]` (`fnc-macros`), logging, data paths.
- `fnc-index`: columnar `IndexStore`, bulk loader, children CSR + overlay, FRN maps, `apply`, compaction,
  name fast-sort ranks, EFU reader/writer, `SharedIndex`.
- `fnc-query`: lexer/parser/AST/display, function/modifier/macro tables, sizes/dates, planner.
- `fnc-exec`: matchers, scans, sort, highlight, search engine thread.
- `fnc-platform` (traits, folder walker, shell, single instance), `fnc-ntfs` (USN parsing, Win32 backend).
- `fnc-app` (`fennec`), `fnc-fes` (`fes`), `fnc-service`; stubs with READMEs for M3–M5 crates.
- tools: `gen-fixtures`, `bench`, `sem-eval` (stub), `gen-icons`.

**Acceptance targeted.** M0: CI config (fmt/clippy/test/bench smoke, Linux + Windows), 1 M fixture,
baseline `bench/results.md`. M1: IDX-01/02/03, UI-01/02/06 — verified as far as possible without a
Windows machine (index-level and headless UI tests; Windows code type-checked by cross-clippy).

**Risks.**
1. No Windows host: MFT/USN/tray/hotkey code cannot be executed here → isolate behind `cfg(windows)`,
   make parsing/folding pure and tested with byte samples, cross-check with `--target x86_64-pc-windows-msvc`.
2. Memory budget (150 MB/1 M) with separate UTF-16 names and UTF-8 fold keys → measure early.
3. Spec contradictions (EBNF vs OR-first precedence; snapshot/children signatures) → ADRs.
4. CJK rendering in egui (no bundled glyphs) → runtime system-font fallback.

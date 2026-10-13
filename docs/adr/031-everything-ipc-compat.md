# ADR-031: Everything 1.4 IPC compatibility layer

- Status: accepted (M4, INT-03)
- Related: SPEC ch.4 §3.2, ch.6 §8 Q5, ADR-030

## Background
es.exe, EverythingToolbar, Listary, Flow Launcher and Total Commander plugins talk to Everything
through a hidden window (`EVERYTHING_TASKBAR_NOTIFICATION`) with `WM_COPYDATA` queries and
`EVERYTHING_WM_IPC` commands (Everything SDK `everything_ipc.h`). The layouts were checked
against the SDK header and `Everything.c` (field order of `EVERYTHING_IPC_LIST2` item data) and
against voidtools ES 1.1.0.38 sources (what es sends and reads).

## Decision
- `fnc_ipc::compat` holds only pure code: constants, `decode_copydata` (QUERY v1/v2 in W and A,
  run-count commands, command line), `execute_copydata`, `encode_list`/`encode_list2`,
  `handle_wm_ipc`. All structures are `pack(1)` little-endian; `reply_hwnd` is 32 bits.
  The app owns the window: copy `lpData`, decode on the window thread, execute (fast for run
  counts; queries preferably on a worker) and send the reply with `SendMessageTimeoutW`
  (`examples/compat_window.rs` is the reference implementation and the es.exe test harness).
- Reported version: **1.4.1** with Fennec's version in the build number
  (`major*10000+minor*100+patch`, 0.1.0 → `1.4.1.100`), because clients gate `QUERY2`,
  `IS_DB_LOADED` and file-info calls on ≥ 1.4.1. `Host::everything_version` can override it.
- `IS_FAST_SORT` answers 1 for every valid sort (all sort keys are in-memory columns, ≤ 80 ms
  for 1 M results), `IS_FILE_INFO_INDEXED` 1 for file size, dates created/modified and
  attributes, 0 for folder sizes, date accessed only if the index keeps it.
- QUERY v1 replies sort by name with folders first and fill `totfolders/totfiles`.
  QUERY2 honours `sort_type` and `request_flags` (all 16 bits); unknown sorts fall back to
  name ascending and the reply reports the sort actually used.
- `A` variants decode UTF-8 (Latin-1 fallback) and reply in UTF-8 (SPEC-GAP: Everything uses
  the ANSI code page).
- Not implemented: the Everything 1.5 "IPC3" named pipe (`es -ipc3`; es falls back to IPC2), the
  search-window commands (500+) and window `WM_COMMAND` ids — they control Everything's UI.

## Consequences
Verified on Windows Server 2022 with `examples/compat_window.rs`: official es.exe 1.1.0.38
(IPC2 and `-ipc1`; version, search, `-n`/`-o`, sorts, columns, CSV/EFU/JSON, result count,
total size, regex/case, `/ad`, run counts, `-timeout`, `-no-result-error`) and the official
Everything SDK `Everything64.dll` (v1 and v2 queries, request flags, highlighted names, totals,
run counts, `IsFastSort`) work unmodified. es 1.1.0.38 sends `-n N` as a `count:N` filter, which
the backend applies (SPEC ch.2 §9). The app must not register the default class while real
Everything is running (SPEC §3.2 conflict rule).

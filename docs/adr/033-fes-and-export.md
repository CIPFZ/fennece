# ADR-033: fes and export formats

- Status: accepted (M4, INT-02/07)
- Related: SPEC ch.4 §2, §7

## Background
`fes` must accept `es.exe` 1.1 command lines and print what es prints. The reference is the
voidtools ES 1.1.0.38 source (MIT): switch grammar, column order, widths/alignment, quoting
rules, size/date formats, JSON layout.

## Decision
- Formats live in `fnc_ipc::export` (one `Exporter` for fes, the app's File → Export and
  tests) with es rules: CSV always quotes text, TSV only when needed, EFU always quotes and
  always carries `Filename,Size,Date Modified,Date Created,Attributes` (re-mountable, INT-07),
  TXT/M3U/M3U8 list full paths only, exports append `\` to folder paths, plain output pads
  columns to es widths (the last column never), CRLF line ends (`-lf`/`-nul` available).
- Default date format (`-date-format 0/4`): `YYYY-MM-DD HH:MM:SS` local time on every platform
  instead of the Windows locale format (SPEC-GAP, needs `GetDateFormat`); explicit ISO/FILETIME
  formats match es. M3U is written in UTF-8 (es: ANSI code page). An empty JSON export is `[]`
  (es prints nothing).
- `-sort <column>` without direction uses the column's default like current es (sizes, dates,
  run counts descending).
- Exit codes follow SPEC (0 ok, 1 no instance, 2 bad arguments, 3 timeout) instead of es's
  (8, 6, 4); the remaining es codes are kept (5 export file, 7 IPC failure, 9 no results).
  `-timeout` keeps es semantics (wait for the instance and its database), it does not limit
  the query.
- `-save-settings` stores the relevant switches in `<data dir>/fes.args` (one per line), loaded
  before the command line; `-clear-settings` deletes it.
- `-offline` loads `index.fnx` (`-db` overrides) and `-filelist` mounts EFU files; both run the
  same `IndexBackend` in-process, so output is identical to the IPC path.
- Accepted but ignored: colour, console-size, code-page, locale, `-pause` and `-ipc1/2/3`
  switches (column widths are honoured). Rejected: Everything 1.5 journal switches (no
  journal IPC).

## Consequences
One formatter for every export path; es-based scripts work with fes except where the locale
date format or exit codes were relied upon (documented in SPEC ch.2 §9).

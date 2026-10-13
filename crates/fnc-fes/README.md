# fnc-fes — `fes`

Command-line search compatible with voidtools `es.exe` 1.1 (SPEC ch.4 §2). Talks to a running
Fennec over the native IPC protocol (`fnc-ipc`), or queries directly without a running
instance: `-offline` loads `index.fnx` (default `%APPDATA%\Fennec\index.fnx`, `-db <file>`),
`-filelist <file.efu>` mounts EFU file lists.

Supported: all es search switches (`-r -i -w -p -a -prefix -suffix -ignore-*`, `-path`,
`-parent-path`, `-parent`, `/ad`, `/a-d`, `/a<attrs>`), paging (`-o`, `-n`), sorts (`-s`,
`-sort <col>[-ascending|-descending]`, `-sort-<col>`, `-sort-ascending/-descending`, `/on`…),
every column switch and `-no-<column>`, formats `-csv -efu -txt -m3u -m3u8 -tsv -json` and
`-export-<format> <file>`, `-no-header -double-quote -size-format -date-format
-no-digit-grouping -size-leading-zero -utf8-bom -lf/-crlf/-nul`, `-highlight` (ANSI on a
terminal, `*` markup otherwise), `-get-result-count -get-total-size -get-folder-size
-get-everything-version -get/-inc/-set-run-count`, `-exit -reindex -update -save-db`,
`-instance -timeout -no-result-error -save-settings -clear-settings`. Colour/width/console
switches are accepted; Everything 1.5 journal switches are rejected as unsupported.

Exit codes: 0 success, 1 Fennec not running, 2 bad arguments, 3 timeout, 5 export file
cannot be created, 7 query failed, 9 no results (`-no-result-error`).

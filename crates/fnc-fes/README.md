# fnc-fes — `fes`

Command-line search compatible with a subset of `es.exe` (SPEC ch.4 §2).

Today: offline mode over EFU file lists (`-filelist`), with `-n`, `-o`, `-sort <col>[-ascending|-descending]`,
`-s`, `-csv`, `-get-result-count`. Querying a running instance over IPC lands in M4.

Exit codes: 0 success, 1 IPC unavailable, 2 bad arguments, 3 search failed.

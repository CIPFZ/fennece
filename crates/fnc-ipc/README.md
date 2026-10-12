# fnc-ipc (M4)

Native named-pipe protocol (`\\.\pipe\Fennec.<instance>.ipc`, length-prefixed MessagePack) and the
read-only Everything 1.4 `WM_COPYDATA` compatibility layer used by `es.exe`, EverythingToolbar, Listary, …
(SPEC ch.4 §3).

**Invariants (planned)**: write operations require an interactive, non-low-integrity client; the
compatibility window is not registered while real Everything is running.

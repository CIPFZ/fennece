# fnc-ipc (M4)

Native local-socket protocol (`\\.\pipe\Fennec.<instance>.ipc`, length-prefixed MessagePack),
its server and blocking client, the Everything 1.4 `WM_COPYDATA` / `EVERYTHING_WM_IPC`
compatibility layer (pure byte layouts; the app owns the hidden window), and result export
in the `es.exe` formats (SPEC ch.4 §3, §7). See the crate rustdoc for the integration contract.

**Invariants**: requests are decoded strictly (unknown fields/ops, trailing bytes, NUL, length
limits → rejected and the connection closed); responses never exceed 256 MiB and pages never
exceed `BackendConfig::max_results` (default 1,000,000) rows; queries run on index snapshots
with a time limit and never touch the UI's search engine; run-count paths resolve through the
index only (no file system access); admin commands go through `Host::authorize`. On Windows
the pipe DACL admits only SYSTEM, Administrators and interactive users.

# ADR-030: Native IPC protocol and the shared query backend

- Status: accepted (M4, INT-02/03/04)
- Related: SPEC ch.4 §3.1, ADR-011 (service protocol)

## Background
fes, the SDK, the Everything 1.4 compatibility layer and the HTTP server all need to run
searches against the live index of a running Fennec. The UI's `SearchEngine` is a single
coalescing worker that cancels superseded queries — sharing it would make external clients
cancel the user's searches (and vice versa).

## Decision
- `fnc_ipc::IndexBackend { SharedIndex, Arc<dyn Host>, Caches }` runs every non-UI query
  itself: parse → plan → `execute` → `sort_ids` on a read snapshot, then pages and builds
  columns. Results are cached per (search, options, sort) while instance+generation are
  unchanged; each `Query` gets a query id (LRU, ≤ 16 ids, ≤ 64 MiB of ids) whose view
  (columns, highlighting) `QueryContinue` reuses. A changed index re-runs the search under
  the same id. Every query has a time limit (default 30 s) enforced by cancelling its token.
- Request flags of Everything (case/ww/path/diacritics/regex) always come from the request;
  the host supplies everything else (`Host::search_options`: precedence, macros, `*`
  semantics, pinyin, type names). Omitted results (`omit:`) are UI-only and ignored.
- Wire format: local socket `Fennec.<instance>.ipc`, frames `len u32 LE | MessagePack`
  (`rmp-serde`, named fields so other languages can implement it). Requests ≤ 1 MiB,
  responses ≤ 256 MiB, ≤ 1,000,000 rows per page. Strict decoding: unknown fields/ops,
  trailing bytes, NUL, over-long strings, > 32 columns or > 3 sort keys are rejected and the
  connection closed. Results are columnar (`ResultColumns`); sizes/dates use `u64::MAX` for
  unknown, as the Everything SDK does.
- Things only the app can do or know are behind `trait Host` (status, admin commands,
  filters/bookmarks, versions); admin commands pass `Host::authorize(cmd, Peer { pid })` so
  the app (which may use `unsafe` in fnc-platform) can check session id/integrity level.
  Run counts are written straight into the index; `Host::run_count_changed` notifies.
- Server: one accept thread, one thread per connection, at most 32 connections (`Busy`
  beyond), pipe DACL `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)` as for the service. The
  blocking client runs its exchange on a worker thread so calls can time out on Windows
  (named pipes have no read timeouts).

## Alternatives
Reusing `SearchEngine` (cancellation conflicts); bincode/hand-written binary (opaque to non-Rust
clients); JSON over the pipe (2–3× larger for million-row pages); returning entry ids (unstable
across compaction, useless to clients).

## Consequences
A query from IPC holds a read snapshot for its duration exactly like a UI query (writers wait;
budget unchanged). Each running query adds one short-lived watchdog thread.

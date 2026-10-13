# ADR-036: ETP/FTP server and client

- Status: accepted (after M6, SPEC ch.4 §5 `[MAY]`; ch.2 §9 ETP rows; ADR-030/032/034)

## Background
ETP is FTP plus Everything's `SITE EVERYTHING` extension. The reference is voidtools'
open-source ETP server plugin (`etp_server.c` 1.0.2.5, MIT): command set, reply texts,
`200-Query results … 200 End.` format, sort names, LIST/MLSD formats. What real clients send
was captured on Windows with a recording server: Everything 1.4.1.1032 logs in
(`USER`/`PASS`), sends `OPTS UTF8 ON`, then only the options that change as bare
`EVERYTHING <option> <value>` lines (`SIZE_COLUMN 1`, `DATE_MODIFIED_COLUMN 1`,
`PATH_COLUMN 1`, `COUNT <visible rows>`, `SORT NAME_ASCENDING`, `SEARCH …`, `CASE 1`, …)
followed by `EVERYTHING QUERY`; Everything 1.5.0.1423b sends the `SITE EVERYTHING` form
(default sort `DATE_MODIFIED_DESCENDING`, `OFFSET` while scrolling).

## Decision
- **Server** (`fnc-etp`, std threads, no Tokio): `fennec-etp` accepts, one
  `fennec-etp-session` thread per connection, transfers run on the session thread. Searches
  go through the shared `IndexBackend` (host search options, property/content functions,
  result-set cache) exactly like IPC/HTTP; `FILTER_SEARCH` and its options are combined at
  the AST level (`fnc_query::Filter` → `to_query_string`). Both `SITE EVERYTHING x` and the
  bare `EVERYTHING x` form are accepted.
- **Namespace from the index only**: `/C:/Data/a.txt` ↔ `C:\Data\a.txt`, UNC roots as
  `/UNC/server/share`, Unix roots unchanged; `/` lists the first component of every root,
  folders above roots are virtual. Paths are normalised (`..` cannot leave `/`) and resolved
  with `rows::resolve_path`; `RETR` opens the index's path for a live file of an online,
  non-file-list volume. Nothing is read from directories.
- **Security** (stricter than Everything, same as HTTP): off by default, binds 127.0.0.1,
  refuses to start without a password (PBKDF2 hash in `settings.toml`; empty user name =
  any), wrong passwords cost 0.8 s and three close the connection, read-only, data
  connections only from/to the control peer (no FTP bounce, no data-port theft), bounded
  line length, connections (`etp_max_connections`) and results per query. No TLS: Everything
  has none either; the dialog and Options page say that credentials travel in plain text.
- **Client = a remote view**: Tools → Connect to ETP Server (or `-connect
  user:pass@host:port`) switches the current view/tab. A `fennec-etp-client` thread owns the
  connection; each search fetches 256 results (shown at once), then the rest in 4096-row
  pages up to `etp_client_max_results` (10 000) unless a newer search arrives. The rows
  become an EFU-style **read-only, offline file-list index of their own**
  (`remote::build_results`), so the table, sorting, copy, export, find bar and status bar work
  unchanged through `FennecApp::view_index()`; nothing reads the remote paths (icons by type,
  no thumbnails, lazy columns or preview text). Sorting is sent to the server
  (`SORT …`); columns ETP cannot sort are sorted locally over the fetched rows.
- **Opening**: link type `ftp` (Fennec default) downloads with `RETR` on its own connection
  into `%TEMP%\Fennec ETP\<host_port>\<drive>\…` and opens/reveals the copy; folders are
  searched on the server. `server`/`share`/`admin-share` rewrite drive paths like Everything
  (`C:\x`, `\\host\C\x`, `\\host\C$\x`) and open them directly. Commands that change files
  are refused with a message.

## Alternatives
- Everything's lazy window (fetch `COUNT` = visible rows at `OFFSET`): the result list, the
  selection and every column are keyed by `EntryId` of an index; a virtual list without an
  index would touch every view module. Bounded prefetch is simple and needs no new UI path.
- Mounting remote results into the main index: local searches would see them, and the
  indexer/compaction would have to know about foreign volumes.
- Tokio/async server: the FTP state machine is sequential per connection; threads are
  cheaper to reason about and cost nothing when the server is off.

## Consequences
- More than `etp_client_max_results` remote results are not listed (the status bar shows the
  server's total). The client never stores the password. No client path rewriting.

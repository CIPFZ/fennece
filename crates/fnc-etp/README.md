# fnc-etp

ETP/FTP server and client (SPEC ch.4 §5, ADR-036). ETP is FTP with Everything's
`SITE EVERYTHING` extension: clients set search options (`SEARCH`, `CASE`, `PATH_COLUMN`,
`COUNT`, `OFFSET`, `SORT`, …) and run `QUERY`, answered by one multi-line `200-Query results`
reply. Everything 1.4/1.5 clients send the same sub-commands as a bare `EVERYTHING` command;
both forms are accepted. Plain FTP clients browse the index (`LIST`, `NLST`, `MLSD`, `MLST`,
`SIZE`, `MDTM`) and download files (`RETR`, `REST`) over passive (`PASV`/`EPSV`) or active
(`PORT`/`EPRT`) data connections.

* `fnc_etp::start(EtpConfig, Arc<IndexBackend>) -> EtpHandle` — std threads: `fennec-etp`
  accepts, one `fennec-etp-session` per connection; dropping the handle stops everything.
* `fnc_etp::EtpClient` — blocking client (login, `OPTS UTF8 ON`, changed options only, then
  `EVERYTHING QUERY`; `RETR`/`MLSD` over `EPSV`/`PASV`).
* `proto` — wire formats; `namespace` — FTP paths (`/C:/Data/a.txt`) ↔ index paths.

**Invariants**: off unless started by the app; binds 127.0.0.1 by default; refuses to start
without a password (stored hashed in settings); every command but USER/PASS/QUIT/NOOP/FEAT/
HELP needs a login; three wrong passwords close the connection; read-only (no STOR/DELE/…);
listings and searches come from the index, never from the disk; downloads open the path the
index holds for the entry (live files of online, non-file-list volumes) — never the client's
text, so `..` and paths outside the index cannot be reached; data connections are accepted
only from (and opened only to) the control connection's peer address; bounded line length,
connections and results per query.

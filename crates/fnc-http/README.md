# fnc-http (M4)

HTTP server (SPEC ch.4 §4): built-in search page, Everything-compatible JSON
(`/?search=…&json=1`), downloads resolved through the index, `/api/v1/status` and
`/api/v1/volumes`. `fnc_http::start(config, backend)` returns a handle; dropping it stops the
server. Runs its own Tokio runtime on a dedicated thread, so the app needs no async code.

**Invariants**: off unless started by the app; binds 127.0.0.1 by default; refuses to start
without a password; every request needs Basic authentication (constant-time compare); GET/HEAD
only; 5 s per request; bounded concurrency and results per request; search length/term
limits; all echoed text is HTML-escaped and the page has a strict CSP; downloads only serve
files that are in the index (local, online volumes; never file-list entries) and the URL is
never used as a file-system path; no directory listings.

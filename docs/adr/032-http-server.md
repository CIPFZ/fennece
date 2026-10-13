# ADR-032: HTTP server

- Status: accepted (M4, INT-04)
- Related: SPEC ch.4 §4, ch.0 §6.3

## Decision
- `fnc_http::start(config, Arc<IndexBackend>) -> HttpHandle`: binds synchronously (errors are
  reported to the caller), then runs axum on a private Tokio runtime (`fennec-http` + two
  `fennec-http-worker` threads); searches run on Tokio's blocking pool through the shared
  `IndexBackend` (ADR-030). The app needs no async code; dropping the handle stops the server
  (2 s grace for running requests).
- Security: refuses to start without a password; Basic authentication on every request
  (constant-time comparison of `user:password`); GET/HEAD only; 8 KiB URI limit; 5 s per request
  (the search's cancel token uses the same limit); at most 16 requests at once (503 beyond);
  ≤ 10,000 rows per request; searches > 1024 bytes, > 32 terms or > 128 wildcards are rejected;
  fixed `Server: Fennec`, `nosniff`, `X-Frame-Options: DENY`, `no-referrer`, `no-store`, and a
  CSP without scripts on the page. Every echoed string is HTML/JSON-escaped.
- Downloads: the URL path is percent-decoded and resolved **in the index only**; it must be a
  file of an online, non-file-list volume; the file opened is the index's full path, never the
  URL text, so traversal is impossible. Folders are never listed — page links search them.
- JSON follows Everything (`totalResults`, `results[{type,name,path,size,date_modified,…}]`,
  values as strings, only requested columns). Unknown values (folder sizes, missing dates) are
  omitted (SPEC-GAP: Everything's behaviour for unknown values is undocumented).
- `static_root` serves sanitised files below the root (canonicalised prefix check) for paths
  other than searches; `/` without a query serves its `index.html`.
- Page strings are injected (`HttpLabels`) so the app can pass its translations.
- Defaults follow Everything: 127.0.0.1:8080, 32 rows per page, JSON `count` = all (capped),
  downloads allowed.

## Alternatives
hyper directly (more code for routing/middleware); tower-http layers (another dependency for
three small middlewares written inline); serving on the app's own runtime (the app has none).

## Consequences
When disabled the crate costs nothing (no thread, no socket). Slow-loris style connection
holding is bounded only by hyper's defaults; acceptable for a loopback-default, authenticated
server.

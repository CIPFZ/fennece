# ADR-034: Servers in the app (IPC, Everything window, HTTP)

- Status: accepted (M4, INT-02/03/04/05)
- Related: SPEC ch.4 §3–4, ADR-030/031/032

## Background
`fnc-ipc` and `fnc-http` are libraries; `fennec.exe` has to run them per `-instance`, start
and stop them from the settings without blocking the UI (stopping HTTP waits up to 2 s), and
answer the `Host` questions (options, filters, bookmarks, status, admin commands) from UI
state that lives on the UI thread.

## Decision
- One `IndexBackend` per app (queries on index snapshots) shared by the native pipe server,
  the Everything window and HTTP. `fnc_app::ipc_host::AppHost` mirrors UI state behind an
  `RwLock` (pushed by the UI on every options/filter/bookmark/status change) and forwards
  requests that need the UI (exit, command lines, view refresh) over a channel plus a repaint;
  saves and `DeleteRunHistory` run on short worker threads.
- `fnc_app::servers` owns every server on a `fennec-servers` thread; the UI sends the wanted
  configuration (`Desired::from_settings`) and reads a status snapshot (address/error for the
  status bar). Only changed parts restart; failed parts are retried on the next apply.
- The window is in `fnc_platform::compat` (Win32 only there, no `fnc-ipc` dependency): a
  top-level never-shown window on its own thread with a `CompatHandler` trait; queries are
  deferred to a `fennec-compat-query` worker that sends the reply `WM_COPYDATA` itself, so the
  window thread never waits for a search. It is not created when another process owns the
  class (real Everything); `EVERYTHING_IPC_CREATED` is broadcast after creation. A client's
  `WM_CLOSE` means "exit" (es.exe `-exit` sends it and waits for the process to end), so the
  app stops its own window with a private `WM_APP` message.
- Authorization of admin commands (`Host::authorize`): pipe clients must run in this session
  with at least medium integrity (`fnc_platform::session`, Linux: same uid); Everything
  clients are allowed (window messages do not cross sessions, UIPI applies, and
  `EVERYTHING_WM_IPC` carries no sender); HTTP never runs admin commands.
- `Rebuild` is refused (`Unsupported`): sources are fixed at start-up and the indexer cannot
  restart in place; `Update` rescans folder indexes, `SaveDb`/`SaveRunHistory` save the
  database (run history is part of it).
- Settings: page `server` (`ipc_server`, `everything_ipc` on; `http_*` with HTTP off,
  loopback, port 8080, downloads on). Passwords are masked in Options and never echoed.

## Alternatives
- Starting servers on the UI thread: simpler, but a port change would freeze the UI for up to
  2 s (AGENT.md rule 2).
- Message-only window (`HWND_MESSAGE`): invisible to `FindWindow`, so es.exe and the SDK would
  not find it.
- Putting the Win32 window in `fnc-ipc`: would need `unsafe` outside `fnc-ntfs`/`fnc-platform`.

## Consequences
- An app with `-instance X` uses the service pipe of instance X; the MSI installs the service
  for the default instance only, so non-admin runs of other instances fall back to folder
  indexing.
- The HTTP password is stored in `settings.toml` in clear text, protected only by the
  profile's file permissions (portable mode: the folder's).

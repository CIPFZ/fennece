# ADR-035: IFilter content extraction in a host process

- Status: accepted (M3+, SPEC `content_ifilter_separate_process`)
- Related: SPEC ch.1 §9.2, ch.2 §9 (IFilter row), ADR-014 (PDF), ADR-016 (stores)

## Background
Text of formats without a built-in parser (`.doc`, `.xls`, `.ppt`, `.rtf`, `.odt`, `.msg`
with Outlook, …) is available through the IFilters registered on Windows (`LoadIFilter` →
`IFilter::Init`/`GetChunk`/`GetText`). Filters are third-party in-process COM servers:
a crashing, hanging or leaking filter must not take Fennec down, block an extraction or
search thread forever, or grow Fennec's memory. `fnc-props` cannot use COM.

## Decision
- **Hidden mode of the app binary**: `fennec.exe -ifilter-host` (dispatched first thing in
  `main`, no window, no logging) runs `fnc_platform::ifilter::host_main`. One binary means
  no packaging change, the host always matches the client's protocol version, and it costs
  nothing on disk. Code lives in `fnc-platform/src/ifilter.rs` (protocol, host loop, client,
  all unsafe COM/Win32 with SAFETY notes) and `fnc-app/src/ifilter.rs` (provider, flag).
- **Transport**: the child's anonymous stdin/stdout pipes, not a named pipe: nothing to
  squat or secure with ACLs, and the host sees EOF when Fennec exits. The host first writes
  an 8-byte handshake (`FNCIFH01`), so a wrong executable (an old `fennec.exe` that would
  open the GUI) is detected and killed at the deadline.
- **Protocol**: little-endian length-prefixed frames. Request `id, kind, arg, path`
  (`Extract` with a byte limit; test-only `Sleep`/`Crash`/`Echo`, refused unless the host
  is started with `test_requests`). Reply `id, status (text / no text / error), UTF-8`,
  cut at the byte limit on a char boundary. Stale replies (of a timed-out request) are
  dropped by id.
- **Isolation**: per-file deadline (10 s); on timeout the host is killed and the path is
  remembered as poisoned for the session (not retried); a crash (pipe EOF) reaps the host;
  the next request starts a fresh one; failed starts back off for 30 s. Hosts are recycled
  every 500 files. Two hosts serve parallel requests (search threads).
- **Containment**: hosts run in a job object (`KILL_ON_JOB_CLOSE`: they die with Fennec even
  if it crashes; 1 GiB per-process memory limit; `DIE_ON_UNHANDLED_EXCEPTION`: no WER
  dialog), are created with `CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS`, and enter
  `PROCESS_MODE_BACKGROUND_BEGIN` (low CPU/I/O/memory priority) with
  `SEM_NOGPFAULTERRORBOX`. COM is initialised STA in the host's single thread.
- **When it is used**: `ContentExtractor::fallback` (a `ContentProvider` trait in
  `fnc-props`) is consulted only if `content_ifilter` is on and the extension has a real
  filter (`HKCR\.ext\PersistentHandler`, directly or through the ProgID's CLSID, → the
  `{89BCB740-…}` add-in; the null handler/filter are ignored; checked in the parent, cached),
  and either the built-in parsers gave no text, or the format is a known non-text document
  (`is_foreign_document`: rtf, doc, xls, ppt, msg, odt, …) whose raw bytes the decoder would
  otherwise return. OOXML/PDF follow `content_office`/`content_pdf`; `textplain:` and the
  raw-encoding functions never use filters. Hosts start lazily: zero cost until used.

## Alternatives
- Separate small `fennec-ifilter.exe`: lighter process image, but one more file to ship
  and version (WiX, portable zip) for no functional gain; the host never loads the GUI.
- In-process IFilters: what Everything does without `content_ifilter_separate_process`;
  rejected (stability, AGENT.md rule 2 when a filter hangs).
- Named pipe per host: needs a random name, a DACL and connect handshakes; stdio gives the
  same with less code.

## Consequences
- A hung filter costs at most the deadline once per file per session; a crash costs a
  process start (~50 ms). `content:` over many unindexed legacy documents is slow (one
  filter call per file, bounded by `slow_search_max_files`), as in Everything.
- Off Windows nothing changes: no provider, `content_ifilter` has no effect.

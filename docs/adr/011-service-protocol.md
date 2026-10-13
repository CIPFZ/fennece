# ADR-011: fennec-service protocol and access model

- Status: accepted (M2, IDX-10)
- Related: SPEC ch.1 §2.4, ch.4 §8

## Background
A standard user cannot open `\\.\C:`, so MFT enumeration and USN reads need a privileged
helper. The helper runs as LocalSystem, so whatever it accepts is attack surface.

## Decision
- `fnc-ntfs::access::VolumeAccess` abstracts the four operations the source needs
  (QueryJournal, EnumMft, ReadUsn, stat by FRN). `NtfsSource::open` tries the volume handle
  first and falls back to the service only on access denied.
- The service answers exactly `Hello`, `QueryJournal`, `EnumMft`, `ReadUsn` over the local
  socket `Fennec.<instance>.svc`, framed `len u32 | payload` (≤ 16 MiB), strict decoding
  (unknown ops, trailing bytes, non-letter drives, journal sizes > 4 GiB → rejected and the
  connection is closed). Only drives that `list_volumes()` reports (fixed NTFS/ReFS) are served.
- Record data is passed through raw (USN_RECORD_V2/V3 blocks) and parsed by the client, so the
  service does no parsing of untrusted input of its own.
- Stat by FRN stays in the client: `OpenFileById` with a handle to the volume's root folder as
  hint only needs the user's own access to the file, so the service never reveals metadata the
  user could not read anyway (names are already visible through the journal, as with
  Everything's service).
- Pipe DACL `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`: SYSTEM, Administrators, interactive
  users; network and service logons cannot connect.
- The client reconnects for 10 s when the service restarts.

## Alternatives
Per-connection session-ID checks (SPEC "每连接鉴权") — the interactive-user ACE already limits
callers to logged-on users; `GetNamedPipeClientSessionId` checks are a hardening follow-up.
Forwarding parsed entries — more code running as SYSTEM for no gain.

## Consequences
Measured on Windows Server 2022 as a standard user: 304k entries ready in 3.7 s, USN change
visible in 25 ms through the service. `fennec-service.exe` must not be run from the build
directory while building (the running service locks it); it is installed from
`C:\Program Files\Fennec` on the test machine.

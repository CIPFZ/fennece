# ADR-037: Scope of `unsafe` code

- Status: accepted (0.2.0, REL-10; AGENT.md iron rule 5; replaces the list in SPEC ch.6 §5)
- Related: ADR-030 (pipes, client checks), ADR-034 (Everything window), ADR-035 (IFilter)

## Background
SPEC ch.6 §5 allowed `unsafe` only in `fnc-ntfs`/`fnc-platform`. M4 added the Everything-SDK
C ABI (`fnc-sdk`), an Everything IPC test window (`fnc-ipc` example) and the service, whose
entry point is FFI. Survey at 0.2.0 (`rg '\bunsafe\b' crates tools`, keyword in code only):
`fnc-ntfs` 18, `fnc-platform` 166, `fnc-sdk` 105 (+3 in tests), `fnc-ipc` 0 in the library
(7 in `examples/compat_window.rs`), `fnc-service` 0; none elsewhere (other hits: comments,
the CSP `'unsafe-inline'` in `fnc-http`). Vendored `third_party/` is outside this policy.

## Decision
- **Five crates only**: `fnc-ntfs`, `fnc-platform`, `fnc-ipc`, `fnc-sdk`, `fnc-service`. A new
  crate (or any crate not listed) that needs `unsafe` writes an ADR first and is added here.
  Every block needs a `// SAFETY:` argument, every `unsafe fn` a `# Safety` section.
- **Lints**: workspace `unsafe_code = "deny"` and `unsafe_op_in_unsafe_fn = "deny"`, inherited
  by every crate (`[lints] workspace = true`); pure crates add `#![forbid(unsafe_code)]`
  (core, index, query, exec, props, semantic, http, etp, fes, app, ipc library, service).
  Overrides: `fnc-ntfs` `#![cfg_attr(windows, allow(unsafe_code))]` (still denied on Linux);
  `fnc-platform` `#![allow(unsafe_code)]`; `fnc-sdk` only in `src/ffi.rs` (the Rust API in
  `lib.rs` stays denied) and `tests/sdk.rs`; `fnc-ipc` only in `examples/compat_window.rs`.
- **fnc-ntfs** (`src/win.rs`: 16 blocks, `unsafe impl Send/Sync` for the handle): drive and
  volume discovery, `CreateFileW`/`OpenFileById`, `GetFileInformationByHandle(Ex)` and one
  synchronous `ioctl` helper for `FSCTL_ENUM_USN_DATA`/`READ_USN_JOURNAL` (and query/create/
  delete journal) into an owned 8-byte-aligned buffer. Iron rule 3 has no safe API: `windows`
  exposes raw `DeviceIoControl`; USN crates are thin, unmaintained and unsafe inside. USN
  records are parsed by safe portable code (`record.rs`, byte-sample tests).
- **fnc-platform** (13 files: 159 blocks, 4 `unsafe fn`, 3 `unsafe impl`): every COM method
  and Win32 function in `windows` is `unsafe fn`; no safe wrapper covers these APIs.
  `fileops.rs` 51 (`IFileOperation`, `IContextMenu2/3` + subclass proc, clipboard,
  `DoDragDrop`), `props.rs` 23 (`IPropertyStore`, `PROPVARIANT`), `winsearch.rs` 18
  (`ISearchManager`, OLE DB `IRowset`), `ifilter.rs` 14 (job object, `LoadIFilter`),
  `compat.rs` 13 (window procedure, `WM_COPYDATA`), `thumb.rs` 8 (`IShellItemImageFactory`,
  `GetDIBits`), `shell.rs` 8 (`ShellExecuteW`, `SHGetFileInfoW`, key subclass),
  `integration.rs` 8 (registry), `session.rs` 8 (client session/integrity, used by the
  service), `instance.rs` 5 (named mutex), `locale.rs` 4, `privilege.rs` 4 (elevation,
  background priority), `process.rs` 2. All in `#[cfg(windows)]` modules behind safe
  functions, with RAII guards (COM init, `PropVariantClear`, `ILFree`, handles, job, subclass).
- **fnc-sdk** (`src/ffi.rs`): a C ABI mirroring `Everything_*` takes raw pointers by
  definition: 15 exported `unsafe extern "C" fn` with `# Safety` contracts, 4 helpers holding
  the 15 blocks (`read_wide`/`read_narrow` bounded by `MAX_STRING`, `copy_out`, `put_u64`),
  and 76 `#[unsafe(no_mangle)]` exports, kept unique by the `fnc_` prefix.
- **fnc-ipc**: the library forbids `unsafe`; pipe security descriptors come from
  `interprocess` (`SecurityDescriptor::deserialize` of SDDL). Only the unshipped manual-test
  example `compat_window` (Everything 1.4 window for `es.exe`) has a window procedure; the
  shipped window is `fnc-platform::compat` (ADR-034).
- **fnc-service**: forbids `unsafe`; `windows-service` (its `define_windows_service!` expands
  the `extern "system"` entry) does service control; client checks use `fnc_platform::session`.

## Alternatives
- Two crates (SPEC as written): the C ABI would move into `fnc-platform`, which would then
  depend on `fnc-ipc` and ship in every binary; rejected.
- Third-party safe wrappers: none cover IFileOperation, IFilter, OLE DB or USN; each would be
  a dependency outside SPEC ch.6 §6 that is unsafe inside anyway.

## Consequences
- Review of `unsafe` is confined to `fnc-ntfs/src/win.rs`, `fnc-platform`, `fnc-sdk/src/ffi.rs`;
  lifting `forbid` in the `fnc-ipc` library or `fnc-service` needs an update of this ADR.

# fnc-sdk (M4) — `fennec_sdk`

Client SDK for a running Fennec (SPEC ch.4 §3.1):

* Rust: `fennec_sdk::Fennec::connect()?.search("abc").sort(Column::Size, false).limit(10).run()?`
  (thin wrapper over `fnc_ipc::Client`).
* C ABI (`fennec_sdk.dll` / `libfennec_sdk.so`, header `include/fennec_sdk.h`, example
  `include/example.c`) mirroring the Everything SDK: `Everything_SetSearchW` →
  `fnc_SetSearchW`, `Everything_QueryW` → `fnc_QueryW`, `Everything_GetResultFullPathNameW` →
  `fnc_GetResultFullPathNameW`, same sort/request-flag/error values.

**Invariants**: the only `unsafe` code of the crate is `src/ffi.rs` (every block carries a
`// SAFETY:` note); the C state is one process-global mutex-protected struct; returned
string pointers live until the next query/reset; queries are synchronous.

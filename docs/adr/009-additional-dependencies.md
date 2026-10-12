# ADR-009: Dependencies beyond the SPEC ch.6 §6 list

- Status: accepted (M0/M1)

| Crate | Size / build | Used by | Why |
|---|---|---|---|
| `serde_json` | > 10 k lines, pure Rust | `fnc-platform` (single-instance messages), `tools/bench` (results.json) | Needed anyway for the HTTP JSON API (M4) and IPC tooling; one well-audited JSON implementation. |
| `png` | pure Rust | `fnc-app` (decode embedded icon), `tools/gen-icons` (encode) | Already a transitive dependency of the egui stack; avoids `image`. |
| `winresource` | build-time only, Windows hosts | `fnc-app/build.rs` | Embeds `fennec.ico` and version info into `fennec.exe` (AGENT M0 task 8). |
| `fluent-syntax` | dev-only | `fnc-app` tests | Enumerates message ids to check every language has every key. |
| `chrono` | see ADR-004 | | |

No dependency with a C build has been added. `cargo-deny` (`deny.toml`) enforces licences, advisories and
sources in CI.

Note: `criterion` (listed in SPEC ch.6 §6, dev-only in `tools/bench`) pulls `alloca`, which compiles a small C
file. It never ships in a binary; the Linux→Windows cross-clippy CI job excludes `bench` because that C build
needs MSVC, and the `windows-latest` job covers it natively.

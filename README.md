# Fennec

*Find anything in a blink.* — a local file search engine for Windows written in Rust and egui, modelled on
voidtools Everything 1.5, with local semantic search planned on top (see `docs/SPEC.md`).

Status: **M0 (skeleton) and M1 (MVP)** — see `docs/reports/` and `docs/CHANGELOG.md`.

![search](docs/screenshots/m1-search-highlight.png)

## Build

```sh
cargo build --release            # fennec (GUI), fes (CLI), fennec-service (stub)
cargo test --workspace
cargo run --release -p gen-fixtures -- --count 1000000      # fixtures/fixture-1m.efu
cargo run --release -p bench --bin bench-report             # bench/results.md
```

Toolchain: Rust 1.99.0 (pinned in `rust-toolchain.toml`), targets `x86_64-pc-windows-msvc` and Linux.

## Run

- Windows (administrator, or with the service from M2): indexes all local NTFS/ReFS volumes via the MFT and
  keeps them current from the USN journal.
- Any platform: `fennec -folder <dir>` indexes a folder; `fennec -filelist <list.efu>` mounts an Everything
  file list; on Linux/macOS the home folder is indexed by default.
- `fes -filelist <list.efu> -n 10 -sort size-descending *.iso` — command-line search (offline mode until M4).

The search syntax follows Everything 1.5 (`ext:jpg;png size:>1mb dm:thisyear`, `a b|c`, `!x`, `<...>`,
`regex:`, `case:`, `path:`, wildcards, macros such as `audio:`); see `docs/SPEC.md` ch.2 and its §9 table of
known differences.

## Layout

`crates/` — `fnc-core` (data model), `fnc-index` (columnar index), `fnc-query` (language), `fnc-exec`
(execution), `fnc-platform`, `fnc-ntfs` (MFT/USN), `fnc-app` (`fennec`), `fnc-fes` (`fes`), `fnc-service`,
and later-milestone crates (`fnc-props`, `fnc-semantic`, `fnc-ipc`, `fnc-http`, `fnc-sdk`).
`tools/` — `gen-fixtures`, `bench`, `gen-icons`, `sem-eval`. `docs/adr/` — architecture decisions.

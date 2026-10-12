# ADR-004: `chrono` for date arithmetic

- Status: accepted (M0)

## Background
Date search values need local-time calendar math: `dm:thismonth`, `lastweek`, `6months`, DST-aware
conversion between local wall time and FILETIME, ISO-8601 parsing in EFU files. `chrono` is not in the
SPEC ch.6 §6 dependency list and is > 10 k lines, so AGENT iron rule 8 requires an ADR.

## Decision
Use `chrono` 0.4 with `default-features = false, features = ["clock", "std"]` (no `oldtime`, no serde),
confined to `fnc-core::time`, `fnc-query::date`, EFU date parsing and display formatting.

## Alternatives
- `time` / `jiff`: comparable size; chrono's `Local` uses the OS time-zone APIs on Windows and the tz
  database on Unix, which matches Everything's local-time semantics.
- Hand-written civil-date code: easy for UTC, error-prone for local time with DST.

## Consequences
No C build; pure Rust. Locale-specific formats (date order, week start) still come from Fennec settings
(SPEC-GAP until OS locale queries land).

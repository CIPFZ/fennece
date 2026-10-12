# gen-fixtures

`cargo run --release -p gen-fixtures -- [--count 1000000] [--seed 42] [--out fixtures/fixture-1m.efu]`

Deterministic synthetic file system as an EFU file list: folder depth 2–12, weighted extension mix,
10% CJK names, 2% diacritics, camera-style names, 5% duplicate files, log-uniform sizes, dates in the ten
years before 2026-10-01. Also a library (`build_index`) used by benchmarks and UI tests.

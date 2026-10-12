# bench

- `cargo run --release -p bench --bin bench-report` — measures the baseline on 1M entries and writes
  `bench/results.md` / `bench/results.json` (index build, memory, query p50/p95, narrowing, sorting, path join).
- `cargo bench -p bench` — criterion suites. Smoke run: `FENNEC_BENCH_ENTRIES=20000 cargo bench -p bench -- --test`.

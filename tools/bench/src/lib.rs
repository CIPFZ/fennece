//! Shared helpers for the benchmark suites.

use std::path::Path;
use std::time::{Duration, Instant};

use fnc_exec::{Caches, CancelToken, ExecCtx, ResultSet, execute};
use fnc_index::IndexStore;
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

/// Number of entries for benchmarks (`FENNEC_BENCH_ENTRIES`, default 1,000,000).
pub fn bench_entries() -> usize {
    std::env::var("FENNEC_BENCH_ENTRIES").ok().and_then(|v| v.replace('_', "").parse().ok()).unwrap_or(1_000_000)
}

/// Loads `fixtures/fixture-1m.efu` when present and the size matches, else generates
/// the fixture in memory. Returns the index and the build time (parse + bulk load).
pub fn load_fixture(entries: usize) -> anyhow::Result<(IndexStore, Duration)> {
    let file = Path::new("fixtures/fixture-1m.efu");
    let t0 = Instant::now();
    let mut store = if entries == 1_000_000 && file.exists() {
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu_file(&mut s, file)?;
        if let Some(v) = s.volumes().first().map(|v| v.id) {
            s.set_volume_online(v, true);
        }
        s
    } else {
        gen_fixtures::build_index(entries, 42)?
    };
    let elapsed = t0.elapsed();
    store.rebuild_name_order();
    Ok((store, elapsed))
}

/// Plans and executes `q` once (fresh plan, shared caches).
pub fn run_query(s: &IndexStore, q: &str, caches: &Caches) -> ResultSet {
    let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    let cancel = CancelToken::new();
    execute(&p, s, &ExecCtx { cancel: &cancel, prev: None, caches }).unwrap_or_default()
}

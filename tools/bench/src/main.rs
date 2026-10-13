//! `bench-report`: measures the M0 baseline on the 1M-entry fixture and writes
//! `bench/results.md` + `bench/results.json`.
//!
//! Metrics: index build from EFU, memory, query p50/p95 (SPEC ch.2 §10 query set),
//! narrowing reuse, sorting, and path joining.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use bench::{bench_entries, load_fixture, run_query};
use fnc_core::EntryId;
use fnc_core::settings::FoldersFirst;
use fnc_exec::{Caches, CancelToken, ExecCtx, SortColumn, SortSpec, execute, sort_ids};
use fnc_query::{ParseEnv, PlanEnv, parse, plan};
use serde::Serialize;

#[derive(Serialize)]
struct Metric {
    name: String,
    unit: &'static str,
    p50: f64,
    p95: f64,
    target: Option<f64>,
    results: Option<usize>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentiles(mut v: Vec<Duration>) -> (f64, f64) {
    v.sort();
    let at = |q: f64| ms(v[((v.len() as f64 - 1.0) * q).round() as usize]);
    (at(0.5), at(0.95))
}

fn measure(runs: usize, mut f: impl FnMut() -> usize) -> (f64, f64, usize) {
    let mut times = Vec::with_capacity(runs);
    let mut n = 0;
    for _ in 0..runs {
        let t = Instant::now();
        n = f();
        times.push(t.elapsed());
    }
    let (p50, p95) = percentiles(times);
    (p50, p95, n)
}

fn main() -> anyhow::Result<()> {
    let entries = bench_entries();
    let runs: usize = std::env::var("FENNEC_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    eprintln!("building index with {entries} entries...");
    let (store, build) = load_fixture(entries)?;
    let t = Instant::now();
    let mut s2 = store.clone();
    s2.rebuild_name_order();
    let rank_build = t.elapsed();
    drop(s2);
    let mut metrics: Vec<Metric> = Vec::new();
    let mut push = |name: &str, unit: &'static str, p50: f64, p95: f64, target: Option<f64>, results: Option<usize>| {
        metrics.push(Metric { name: name.into(), unit, p50, p95, target, results });
    };
    push(
        "index build from EFU (parse + bulk load)",
        "ms",
        ms(build),
        ms(build),
        Some(60_000.0),
        Some(store.live_count()),
    );
    push("name fast-sort index build", "ms", ms(rank_build), ms(rank_build), None, None);
    let heap_mb = store.heap_bytes() as f64 / (1024.0 * 1024.0);
    push("index heap memory", "MiB", heap_mb, heap_mb, Some(150.0), None);

    let caches = Caches::new();
    // Warm caches used by path queries (built once per tree generation in practice).
    let t = Instant::now();
    let _ = caches.dir_paths(&store);
    let dp_build = t.elapsed();
    push("folder path cache build", "ms", ms(dp_build), ms(dp_build), None, None);

    let queries: &[(&str, Option<f64>)] = &[
        ("a", Some(50.0)),
        ("abc", Some(50.0)),
        ("report", Some(50.0)),
        ("*.jpg", Some(50.0)),
        ("regex:^IMG_\\d{4}", None),
        ("ext:jpg size:>1mb dm:thisyear", None),
        ("ext:jpg;png;gif", None),
        ("folder:", None),
        ("windows\\", None),
        ("c:\\users\\*", None),
        ("季度", Some(50.0)),
        ("cafe", Some(50.0)),
        ("!a", None),
        ("a b c", None),
        ("ww:report", None),
    ];
    for (q, target) in queries {
        let (p50, p95, n) = measure(runs, || run_query(&store, q, &caches).len());
        eprintln!("{q:<34} p50 {p50:7.2} ms  p95 {p95:7.2} ms  ({n} results)");
        push(&format!("query `{q}`"), "ms", p50, p95, *target, Some(n));
    }

    // Narrowing: "a" -> "ab" -> "abc" reusing the previous result set.
    let pe = ParseEnv::default();
    let env = PlanEnv::default();
    let cancel = CancelToken::new();
    let (p50, p95, n) = measure(runs, || {
        let p1 = plan(&parse("a", &pe).ast, &env, None);
        let Ok(r1) = execute(&p1, &store, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }) else { return 0 };
        let p2 = plan(&parse("ab", &pe).ast, &env, Some(&p1));
        let Ok(r2) = execute(&p2, &store, &ExecCtx { cancel: &cancel, prev: Some(&r1), caches: &caches }) else {
            return 0;
        };
        let p3 = plan(&parse("abc", &pe).ast, &env, Some(&p2));
        let r3 = execute(&p3, &store, &ExecCtx { cancel: &cancel, prev: Some(&r2), caches: &caches });
        r3.map(|r| r.len()).unwrap_or(0)
    });
    push("narrowing a→ab→abc (3 queries, reuse)", "ms", p50, p95, None, Some(n));

    // Sorting.
    let all: Vec<EntryId> = (0..store.len() as u32).map(EntryId).filter(|&i| store.is_live(i)).collect();
    let spec = |column, ascending| SortSpec::by(column, ascending, FoldersFirst::Ascending);
    for (label, sp, target) in [
        ("sort 1M by name (fast-sort ranks)", spec(SortColumn::Name, true), Some(300.0)),
        ("sort 1M by size desc", spec(SortColumn::Size, false), Some(300.0)),
        ("sort 1M by date modified desc", spec(SortColumn::DateModified, false), Some(300.0)),
        ("sort 1M by path", spec(SortColumn::Path, true), Some(300.0)),
    ] {
        let (p50, p95, n) = measure(runs.min(10), || {
            let mut ids = all.clone();
            let _ = sort_ids(&mut ids, &sp, &store, &caches, &cancel);
            ids.len()
        });
        eprintln!("{label:<34} p50 {p50:7.2} ms  p95 {p95:7.2} ms");
        push(label, "ms", p50, p95, target, Some(n));
    }
    // Natural comparator without ranks (cold sort before the fast-sort index exists).
    let (p50, p95, n) = measure(3, || {
        let mut ids = all.clone();
        ids.sort_unstable_by(|&a, &b| store.cmp_names(a, b));
        ids.len()
    });
    push("sort 1M by name (natural comparator, 1 thread)", "ms", p50, p95, None, Some(n));

    // Path joining.
    let (p50, p95, n) = measure(runs.min(10), || {
        let mut buf = String::with_capacity(512);
        let mut total = 0usize;
        for &id in &all {
            store.full_path(id, &mut buf);
            total += buf.len();
        }
        total
    });
    push("full path join, all entries (1 thread)", "ms", p50, p95, None, Some(n));
    let per = p50 * 1e6 / all.len() as f64;
    push("full path join per entry", "ns", per, p95 * 1e6 / all.len() as f64, None, None);

    let mut md = String::new();
    let _ = writeln!(md, "# Fennec benchmark baseline\n");
    let _ = writeln!(
        md,
        "Generated by `cargo run --release -p bench --bin bench-report` on {} entries \
         (`tools/gen-fixtures`, seed 42), {} runs per query, {} logical CPUs, {}.\n",
        store.live_count(),
        runs,
        std::thread::available_parallelism().map_or(0, |n| n.get()),
        std::env::consts::OS
    );
    let _ = writeln!(md, "| Metric | p50 | p95 | Unit | Target | Results |");
    let _ = writeln!(md, "|---|---:|---:|---|---:|---:|");
    for m in &metrics {
        let _ = writeln!(
            md,
            "| {} | {:.2} | {:.2} | {} | {} | {} |",
            m.name,
            m.p50,
            m.p95,
            m.unit,
            m.target.map_or("–".to_string(), |t| format!("≤ {t}")),
            m.results.map_or("–".to_string(), |r| r.to_string())
        );
    }
    std::fs::create_dir_all("bench")?;
    std::fs::write("bench/results.md", &md)?;
    std::fs::write("bench/results.json", serde_json::to_string_pretty(&metrics)?)?;
    eprintln!("wrote bench/results.md and bench/results.json");
    let failures: Vec<&Metric> = metrics.iter().filter(|m| m.target.is_some_and(|t| m.p95 > t)).collect();
    for f in &failures {
        eprintln!("BUDGET EXCEEDED: {} p95 {:.2} > {:?}", f.name, f.p95, f.target);
    }
    Ok(())
}

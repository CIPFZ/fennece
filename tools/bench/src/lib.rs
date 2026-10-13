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

/// M3: synthetic document text (`words` words from a pseudo-English vocabulary; the
/// phrase "quarterly plan" appears in about 2 % of documents, "plan" alone more often).
pub fn synthetic_doc(rng: &mut gen_fixtures::Rng, words: usize) -> String {
    const SYL: &[&str] = &[
        "ka", "lo", "mi", "ra", "te", "su", "no", "ve", "di", "pa", "re", "to", "an", "el", "is", "or", "un", "ber",
        "con", "tion", "ment", "ing", "er", "al", "ly", "pro", "sta", "gra", "port", "vis",
    ];
    let mut out = String::with_capacity(words * 7);
    let quarterly = rng.chance(0.02);
    for i in 0..words {
        if i > 0 {
            out.push(if rng.chance(0.08) { '\n' } else { ' ' });
        }
        if quarterly && i == words / 2 {
            out.push_str("Quarterly plan");
            continue;
        }
        match rng.below(40) {
            0 => out.push_str("plan"),
            1 => out.push_str("quarterly"),
            2 => out.push_str("planning"),
            3 => out.push_str("季度报告"),
            _ => {
                for _ in 0..1 + rng.below(3) {
                    out.push_str(rng.pick(SYL));
                }
            }
        }
    }
    out
}

/// M3: content store over `n` files of `s` spread over the index (300–700 words each).
pub fn content_store(s: &IndexStore, n: usize, seed: u64) -> fnc_props::content::ContentStore {
    let mut rng = gen_fixtures::Rng::new(seed);
    let files: Vec<fnc_core::EntryId> =
        (0..s.len() as u32).map(fnc_core::EntryId).filter(|&i| s.is_live(i) && !s.is_dir(i)).collect();
    let stride = (files.len() / n.max(1)).max(1);
    let mut c = fnc_props::content::ContentStore::new(s.instance());
    for &id in files.iter().step_by(stride).take(n) {
        let words = 300 + rng.below(400);
        c.insert(id, &synthetic_doc(&mut rng, words));
    }
    c
}

/// M3: property store with `width`/`height` for `fraction` of the files (100–8000 px).
pub fn width_store(s: &IndexStore, fraction: f64, seed: u64) -> fnc_props::PropertyStore {
    use fnc_props::{PropertyId, Value};
    let mut rng = gen_fixtures::Rng::new(seed);
    let mut p = fnc_props::PropertyStore::new(s.instance());
    p.set_indexed(&[PropertyId::WIDTH, PropertyId::HEIGHT]);
    for id in (0..s.len() as u32).map(fnc_core::EntryId) {
        if s.is_live(id) && !s.is_dir(id) && rng.chance(fraction) {
            let w = 100 + rng.below(7900) as i64;
            p.insert(id, vec![(PropertyId::WIDTH, Value::Int(w)), (PropertyId::HEIGHT, Value::Int(w * 3 / 4))]);
        }
    }
    p
}

/// Plans and executes `q` with property/content stores.
pub fn run_query_ext(s: &IndexStore, q: &str, caches: &Caches, ext: &fnc_exec::ExtCtx<'_>) -> ResultSet {
    let env =
        PlanEnv { indexed_props: ext.props.map(|p| p.indexed().to_vec()).unwrap_or_default(), ..PlanEnv::default() };
    let p = plan(&parse(q, &ParseEnv::default()).ast, &env, None);
    let cancel = CancelToken::new();
    fnc_exec::execute_ext(&p, s, &ExecCtx { cancel: &cancel, prev: None, caches }, ext).unwrap_or_default()
}

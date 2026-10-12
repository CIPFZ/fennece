//! Criterion suite: scans, path joining and sorting on the synthetic fixture.
//!
//! `FENNEC_BENCH_ENTRIES=20000 cargo bench -p bench -- --test` runs a quick smoke pass
//! (used by CI).

use std::sync::OnceLock;

use bench::{bench_entries, load_fixture, run_query};
use criterion::{Criterion, criterion_group, criterion_main};
use fnc_core::EntryId;
use fnc_exec::{Caches, CancelToken, SortColumn, SortSpec, sort_ids};
use fnc_index::IndexStore;

fn store() -> &'static IndexStore {
    static S: OnceLock<IndexStore> = OnceLock::new();
    S.get_or_init(|| match load_fixture(bench_entries()) {
        Ok((s, _)) => s,
        Err(e) => {
            eprintln!("fixture: {e}");
            IndexStore::default()
        }
    })
}

fn scans(c: &mut Criterion) {
    let s = store();
    let caches = Caches::new();
    let mut g = c.benchmark_group("scan");
    g.sample_size(20);
    for q in ["a", "abc", "*.jpg", "regex:^IMG_\\d{4}", "ext:jpg size:>1mb dm:thisyear", "windows\\"] {
        g.bench_function(q, |b| b.iter(|| run_query(s, q, &caches).len()));
    }
    g.finish();
}

fn paths(c: &mut Criterion) {
    let s = store();
    let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i)).take(100_000).collect();
    c.bench_function("path join 100k", |b| {
        let mut buf = String::with_capacity(512);
        b.iter(|| {
            let mut n = 0;
            for &id in &ids {
                s.full_path(id, &mut buf);
                n += buf.len();
            }
            n
        })
    });
}

fn sorting(c: &mut Criterion) {
    let s = store();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let all: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i)).collect();
    let mut g = c.benchmark_group("sort");
    g.sample_size(10);
    g.bench_function("name natural (ranks)", |b| {
        b.iter(|| {
            let mut v = all.clone();
            let _ = sort_ids(&mut v, &SortSpec::default(), s, &caches, &cancel);
            v.len()
        })
    });
    g.bench_function("size desc", |b| {
        let spec = SortSpec { column: SortColumn::Size, ascending: false, ..SortSpec::default() };
        b.iter(|| {
            let mut v = all.clone();
            let _ = sort_ids(&mut v, &spec, s, &caches, &cancel);
            v.len()
        })
    });
    g.finish();
}

criterion_group!(benches, scans, paths, sorting);
criterion_main!(benches);

//! Semantic terms through the executor and the engine, with a fake embedding index.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fnc_core::EntryId;
use fnc_index::{IndexStore, SharedIndex, efu};
use fnc_query::SemPred;

use crate::engine::{EngineEvent, SearchEngine, SearchOptions, SearchRequest};
use crate::ext::{ExecWarning, Shared};
use crate::semantic::{SemHit, SemanticSource};
use crate::sort::{SortColumn, SortSpec};

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Docs\",,,,16\n\
\"C:\\Docs\\2026Q2_budget.xlsx\",10,,,32\n\
\"C:\\Docs\\holiday.jpg\",10,,,32\n\
\"C:\\Docs\\notes.md\",10,,,32\n\
\"C:\\Docs\\budget-old.xlsx\",10,,,32\n\
\"C:\\Src\\usn.rs\",10,,,32\n";

/// Similarities by (query text, file name); counts ANN and exact calls.
struct Fake {
    scores: HashMap<(&'static str, &'static str), f32>,
    ann: AtomicUsize,
    exact: AtomicUsize,
}

impl SemanticSource for Fake {
    fn generation(&self) -> u64 {
        1
    }
    fn default_top(&self) -> usize {
        200
    }
    fn search(
        &self,
        q: &SemPred,
        s: &IndexStore,
        candidates: Option<&[EntryId]>,
        top: usize,
    ) -> Result<Vec<SemHit>, String> {
        if q.text == "fail" {
            return Err("model file not found".into());
        }
        match candidates {
            Some(_) => self.exact.fetch_add(1, Ordering::Relaxed),
            None => self.ann.fetch_add(1, Ordering::Relaxed),
        };
        let min = q.min_score_f32().unwrap_or(0.45);
        let all: Vec<EntryId> = match candidates {
            Some(c) => c.to_vec(),
            None => (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i)).collect(),
        };
        let mut hits: Vec<SemHit> = all
            .into_iter()
            .filter_map(|id| {
                let name = s.name_string(id);
                let score = *self.scores.iter().find(|((t, n), _)| *t == q.text && *n == name)?.1;
                (score >= min).then_some(SemHit { id, score, range: name.ends_with(".md").then_some((3, 9)) })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(top);
        Ok(hits)
    }
}

fn fake() -> Arc<Fake> {
    let mut scores = HashMap::new();
    scores.insert(("季度预算", "2026Q2_budget.xlsx"), 0.72);
    scores.insert(("季度预算", "budget-old.xlsx"), 0.55);
    scores.insert(("季度预算", "notes.md"), 0.47);
    scores.insert(("read usn", "usn.rs"), 0.8);
    scores.insert(("budget", "notes.md"), 0.6);
    Arc::new(Fake { scores, ann: AtomicUsize::new(0), exact: AtomicUsize::new(0) })
}

fn names(s: &IndexStore, ids: &[EntryId]) -> Vec<String> {
    ids.iter().map(|&id| s.name_string(id)).collect()
}

fn run(
    src: &Arc<Fake>,
    text: &str,
    sort: SortSpec,
    fuse: bool,
) -> (Vec<String>, Arc<crate::ResultSet>, Vec<ExecWarning>) {
    let mut store = IndexStore::default();
    efu::load_efu(&mut store, EFU.as_bytes(), "t.efu").unwrap();
    store.rebuild_name_order();
    let shared = SharedIndex::new(store);
    let (tx, rx) = crossbeam_channel::unbounded();
    let eng = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
    let source: Arc<dyn SemanticSource> = src.clone();
    let options =
        SearchOptions { semantic: Some(Shared::new(source)), semantic_fuse: fuse, ..SearchOptions::default() };
    eng.search(SearchRequest { id: 1, text: text.into(), filter: None, sort, options: Arc::new(options) });
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let snap = shared.snapshot();
    (names(&snap, &o.results.ids), o.results.clone(), o.results.stats.warnings.clone())
}

fn relevance() -> SortSpec {
    SortSpec::by(SortColumn::Relevance, false, fnc_core::settings::FoldersFirst::Never)
}

#[test]
fn semantic_only_queries_use_ann_and_sort_by_relevance() {
    let f = fake();
    let (got, rs, _) = run(&f, "sem:季度预算", relevance(), false);
    assert_eq!(got, ["2026Q2_budget.xlsx", "budget-old.xlsx", "notes.md"]);
    assert!(rs.has_relevance());
    assert!(rs.relevance(rs.ids[0]).is_some_and(|s| (s - 0.72).abs() < 1e-6));
    assert_eq!(rs.semantic_range(rs.ids[2]), Some(3..12));
    assert_eq!(rs.semantic_range(rs.ids[0]), None);
    assert_eq!((f.ann.load(Ordering::Relaxed), f.exact.load(Ordering::Relaxed)), (1, 0));
    // Thresholds.
    let (got, ..) = run(&f, "sem:>0.6 季度预算", relevance(), false);
    assert_eq!(got, ["2026Q2_budget.xlsx"]);
}

#[test]
fn filters_run_first_and_few_candidates_are_scored_exactly() {
    let f = fake();
    let (got, ..) = run(&f, "sem:季度预算 ext:xlsx", relevance(), false);
    assert_eq!(got, ["2026Q2_budget.xlsx", "budget-old.xlsx"]);
    assert_eq!((f.ann.load(Ordering::Relaxed), f.exact.load(Ordering::Relaxed)), (0, 1));
    // Default name sort still works; folders never match.
    let (got, ..) = run(&f, "sem:季度预算", SortSpec::default(), false);
    assert_eq!(got, ["2026Q2_budget.xlsx", "budget-old.xlsx", "notes.md"]);
    // Nested terms: ANN sets.
    let (got, ..) = run(&f, "<sem:季度预算 | sem:\"read usn\"> !ext:md", relevance(), false);
    assert_eq!(got, ["usn.rs", "2026Q2_budget.xlsx", "budget-old.xlsx"]);
}

#[test]
fn missing_model_is_a_warning_and_no_results() {
    let f = fake();
    let (got, _, warnings) = run(&f, "sem:fail", SortSpec::default(), false);
    assert!(got.is_empty());
    assert!(matches!(&warnings[..], [ExecWarning::Semantic(m)] if m.contains("model")));
}

#[test]
fn smart_mode_fuses_literal_and_semantic_hits() {
    let f = fake();
    // Literal "budget": the two xlsx files; semantic "budget": notes.md.
    let (got, rs, _) = run(&f, "?sem: budget", relevance(), false);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.contains(&"notes.md".to_string()));
    assert!(rs.has_relevance());
    let (got2, ..) = run(&f, "budget", relevance(), true);
    assert_eq!(got2.len(), 3);
    // Without the switch nothing semantic happens.
    let (plain, ..) = run(&f, "budget", SortSpec::default(), false);
    assert_eq!(plain, ["2026Q2_budget.xlsx", "budget-old.xlsx"]);
}

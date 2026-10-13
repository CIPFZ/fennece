//! `si:` with a fake system index source: path resolution, combination with other terms,
//! caching, limits, unavailability and cancellation.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fnc_index::{IndexStore, SharedIndex, efu};
use fnc_query::{ParseEnv, PlanEnv, parse, plan};

use crate::exec::{ExecCtx, ResultSet, execute_ext};
use crate::ext::{ExecWarning, ExtCtx};
use crate::sysindex::{SystemIndexCache, SystemIndexError, SystemIndexHits, SystemIndexSource, resolve_paths};
use crate::{
    Caches, CancelToken, Cancelled, EngineEvent, SearchEngine, SearchOptions, SearchRequest, Shared, SortSpec,
};

const EFU: &str = "Filename,Size,Date Modified,Date Created,Attributes\n\
\"C:\\Users\\bob\\Documents\\Report Q3.docx\",12000,134100000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Documents\\report notes.txt\",10,134100000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Documents\\budget.xlsx\",10,134100000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Photos\\IMG_0001.JPG\",3000000,134000000000000000,133500000000000000,32\n\
\"C:\\Users\\bob\\Photos\\café.png\",500,134000000000000000,133500000000000000,32\n\
\"D:\\Music\\track.flac\",9000000,,,32\n";

fn store() -> IndexStore {
    let mut s = IndexStore::default();
    efu::load_efu(&mut s, EFU.as_bytes(), "t.efu").unwrap();
    s.set_volume_online(fnc_core::VolumeId(0), true);
    s.rebuild_name_order();
    s
}

/// What the fake answers.
enum Answer {
    Paths(Vec<String>),
    Limit(Vec<String>, usize),
    Unavailable,
    Failed,
    /// Waits until the search is cancelled.
    Block,
}

#[derive(Default)]
struct Fake {
    answers: HashMap<String, Answer>,
    calls: AtomicUsize,
}

impl Fake {
    fn new(answers: Vec<(&str, Answer)>) -> Arc<Self> {
        Arc::new(Self {
            answers: answers.into_iter().map(|(q, a)| (q.to_string(), a)).collect(),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl SystemIndexSource for Fake {
    fn search(&self, aqs: &str, cancel: &dyn Fn() -> bool) -> Result<SystemIndexHits, SystemIndexError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match self.answers.get(aqs) {
            None => Ok(SystemIndexHits::default()),
            Some(Answer::Paths(p)) => Ok(SystemIndexHits { paths: p.clone(), limit: None }),
            Some(Answer::Limit(p, n)) => Ok(SystemIndexHits { paths: p.clone(), limit: Some(*n) }),
            Some(Answer::Unavailable) => Err(SystemIndexError::Unavailable),
            Some(Answer::Failed) => Err(SystemIndexError::Failed("bad query".into())),
            Some(Answer::Block) => {
                while !cancel() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(SystemIndexError::Cancelled)
            }
        }
    }
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn run(
    s: &IndexStore,
    q: &str,
    src: Option<&Arc<dyn SystemIndexSource>>,
    caches: &Caches,
    cancel: &CancelToken,
) -> Result<ResultSet, Cancelled> {
    let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
    assert!(p.diagnostics.is_empty(), "{q}: {:?}", p.diagnostics);
    let ext = ExtCtx { system_index: src, ..ExtCtx::default() };
    execute_ext(&p, s, &ExecCtx { cancel, prev: None, caches }, &ext)
}

fn names(s: &IndexStore, rs: &ResultSet) -> BTreeSet<String> {
    rs.ids.iter().map(|&i| s.name_string(i)).collect()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn report_fake() -> Arc<Fake> {
    Fake::new(vec![
        (
            "report",
            Answer::Paths(paths(&[
                "C:\\Users\\bob\\Documents\\Report Q3.docx",
                // Case, separators and a trailing separator differ from the index.
                "c:/users/BOB/documents/REPORT NOTES.TXT",
                "C:\\Users\\bob\\Photos\\",
                // Not in the index (another volume, a deleted file): ignored.
                "E:\\elsewhere\\report.txt",
                "C:\\Users\\bob\\Documents\\gone.docx",
            ])),
        ),
        ("kind:music", Answer::Paths(paths(&["D:\\Music\\track.flac"]))),
        ("\"a phrase\"", Answer::Paths(paths(&["C:\\Users\\bob\\Photos\\CAFÉ.png"]))),
        ("offline", Answer::Unavailable),
        ("broken", Answer::Failed),
        ("many", Answer::Limit(paths(&["C:\\Users\\bob\\Documents\\budget.xlsx"]), 1)),
        ("slow", Answer::Block),
    ])
}

#[test]
fn paths_resolve_to_entries() {
    let s = store();
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let q = |q: &str| run(&s, q, Some(&src), &caches, &cancel).unwrap();
    let rs = q("si:report");
    assert_eq!(names(&s, &rs), set(&["Report Q3.docx", "report notes.txt", "Photos"]));
    assert!(rs.stats.warnings.is_empty(), "{:?}", rs.stats.warnings);
    assert_eq!(names(&s, &q("system-index:kind:music")), set(&["track.flac"]));
    // The quotes of `si*:` reach the source; diacritics fold like the index.
    assert_eq!(names(&s, &q("si*:\"a phrase\"")), set(&["café.png"]));
    assert!(q("si:nothing").is_empty());
}

#[test]
fn combines_with_other_terms() {
    let s = store();
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let q = |q: &str| names(&s, &run(&s, q, Some(&src), &caches, &cancel).unwrap());
    assert_eq!(q("si:report ext:docx"), set(&["Report Q3.docx"]));
    assert_eq!(q("si:report file:"), set(&["Report Q3.docx", "report notes.txt"]));
    assert_eq!(q("si:report !report"), set(&["Photos"]));
    assert_eq!(q("si:report | *.flac"), set(&["Report Q3.docx", "report notes.txt", "Photos", "track.flac"]));
    assert_eq!(q("si:report | si:kind:music folder:"), set(&["Photos"]));
    assert_eq!(q("ext:txt;docx;xlsx !si:report"), set(&["budget.xlsx"]));
    assert_eq!(q("<si:report si:kind:music>"), set(&[]));
    // Each distinct query text is asked once; the rest came from the cache.
    assert_eq!(fake.calls(), 2);
}

#[test]
fn source_is_queried_once_and_only_for_si() {
    let s = store();
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    for q in ["report", "*.docx", "ext:txt report", "content:report"] {
        run(&s, q, Some(&src), &caches, &cancel).unwrap();
    }
    assert_eq!(fake.calls(), 0, "searches without si: never ask the system index");
    run(&s, "si:report", Some(&src), &caches, &cancel).unwrap();
    run(&s, "si:report ext:docx", Some(&src), &caches, &cancel).unwrap();
    run(&s, "si:report", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(fake.calls(), 1, "cached across searches");
    run(&s, "si:report", Some(&src), &Caches::new(), &cancel).unwrap();
    assert_eq!(fake.calls(), 2, "the cache belongs to the engine");
    caches.system_index().clear();
    run(&s, "si:report", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(fake.calls(), 3);
    assert!(SystemIndexCache::TTL >= SystemIndexCache::UNAVAILABLE_TTL);
}

#[test]
fn unavailable_failed_and_limited() {
    let s = store();
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    // No source at all (non-Windows builds, tests).
    let rs = run(&s, "si:report", None, &caches, &cancel).unwrap();
    assert!(rs.is_empty());
    assert_eq!(rs.stats.warnings, [ExecWarning::SystemIndexUnavailable]);
    assert!(rs.stats.errors.iter().any(|e| e.contains("Windows Search")), "{:?}", rs.stats.errors);
    // The service is not running: nothing matches, a warning says why; remembered.
    let rs = run(&s, "si:offline", Some(&src), &caches, &cancel).unwrap();
    assert!(rs.is_empty());
    assert_eq!(rs.stats.warnings, [ExecWarning::SystemIndexUnavailable]);
    let rs = run(&s, "si:offline | *.flac", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(names(&s, &rs), set(&["track.flac"]), "the other terms still work");
    assert_eq!(fake.calls(), 1);
    // A failed query is reported with its message and retried next time.
    let rs = run(&s, "si:broken", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(rs.stats.warnings, [ExecWarning::SystemIndexFailed("bad query".into())]);
    run(&s, "si:broken", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(fake.calls(), 3);
    // The source stopped at its limit: the results it gave, plus a note.
    let rs = run(&s, "si:many", Some(&src), &caches, &cancel).unwrap();
    assert_eq!(names(&s, &rs), set(&["budget.xlsx"]));
    assert_eq!(rs.stats.warnings, [ExecWarning::SystemIndexLimit(1)]);
}

#[test]
fn cancellation_stops_a_slow_query() {
    let s = store();
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        c2.cancel();
    });
    assert!(run(&s, "si:slow", Some(&src), &caches, &cancel).is_err());
    t.join().unwrap();
    assert!(caches.system_index().is_empty(), "cancelled queries are not cached");
}

#[test]
fn many_paths_use_the_name_lookup() {
    let s = store();
    let mut all = Vec::new();
    let mut p = String::new();
    for i in 0..s.len() {
        let id = fnc_core::EntryId(i as u32);
        if s.is_live(id) && s.parent(id).is_some() {
            p.clear();
            s.full_path(id, &mut p);
            all.push(p.to_uppercase());
        }
    }
    // Repeat to pass the threshold of the lookup path.
    let n = all.len();
    let many: Vec<String> = all.iter().cycle().take(n * 20).cloned().collect();
    let caches = Caches::new();
    let cancel = CancelToken::new();
    let few = resolve_paths(&s, &all, &caches, &cancel).unwrap();
    let lots = resolve_paths(&s, &many, &caches, &cancel).unwrap();
    for i in 0..s.len() {
        let id = fnc_core::EntryId(i as u32);
        let expect = s.is_live(id) && s.parent(id).is_some();
        assert_eq!(few.get(i), expect, "{}", s.name_string(id));
        assert_eq!(lots.get(i), expect, "{}", s.name_string(id));
    }
}

#[test]
fn engine_passes_the_source() {
    let shared = SharedIndex::new(store());
    let (tx, rx) = crossbeam_channel::unbounded();
    let eng = SearchEngine::spawn(shared.clone(), tx, Box::new(|| {}));
    let fake = report_fake();
    let src: Arc<dyn SystemIndexSource> = fake.clone();
    let options = Arc::new(SearchOptions { system_index: Some(Shared::new(src)), ..SearchOptions::default() });
    let req = |id: u64, text: &str, options: &Arc<SearchOptions>| SearchRequest {
        id,
        text: text.into(),
        filter: None,
        sort: SortSpec::default(),
        options: options.clone(),
    };
    eng.search(req(1, "si:report ext:docx", &options));
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let snap = shared.snapshot();
    assert_eq!(names(&snap, &o.results), set(&["Report Q3.docx"]));
    // Without a source (the default options): unavailable.
    eng.search(req(2, "si:report", &Arc::new(SearchOptions::default())));
    let EngineEvent::Done(o) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(o.results.is_empty());
    assert_eq!(o.results.stats.warnings, [ExecWarning::SystemIndexUnavailable]);
}

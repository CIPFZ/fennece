//! Background search service: owns the query thread, cancels superseded queries and
//! reuses the previous result set when a search only narrows (SPEC ch.0 §4.1, ch.1
//! §6.1). Used by the GUI, `fes` (offline mode), IPC and HTTP.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use fnc_core::settings::Precedence;
use fnc_index::SharedIndex;
use fnc_query::filter::combine;
use fnc_query::parser::{Diag, ParseEnv, UserMacro};
use fnc_query::plan::{OmitSet, PlanDiag, PlanEnv, PlannedQuery};
use fnc_query::{Expr, FilterSet, ModSet, Modifier, Term, Value, parse, plan};
use parking_lot::Mutex;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::exec::{ExecCtx, ResultSet, execute};
use crate::sort::{SortSpec, sort_ids};
use crate::types::TypeNames;

/// Search-wide options (from settings).
#[derive(Clone, Debug, PartialEq)]
pub struct SearchOptions {
    /// Operator precedence.
    pub precedence: Precedence,
    /// Default modifiers (match case / whole word / path / diacritics).
    pub defaults: ModSet,
    /// `*` matches `\`.
    pub star_all: bool,
    /// Reuse the previous result set when narrowing.
    pub reuse: bool,
    /// Treat the whole search as one regular expression (`match_regex`).
    pub regex_all: bool,
    /// User macros (e.g. from filters with a macro name, see
    /// [`fnc_query::Filter::user_macro`]).
    pub macros: Vec<UserMacro>,
    /// Also match plain terms by pinyin (as if every term had `pinyin:`).
    pub pinyin_default: bool,
    /// Results hidden with "Omit result"; excluded unless the search uses `omit:`.
    pub omit: Option<OmitSet>,
    /// Type-name provider for `type:` (Windows: registry lookup); `None` = the
    /// fallback "`EXT` File" names. Create it once and reuse it: descriptions are
    /// cached per extension until a different provider is installed.
    pub type_names: Option<TypeNames>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            precedence: Precedence::Or,
            defaults: ModSet::EMPTY,
            star_all: false,
            reuse: true,
            regex_all: false,
            macros: Vec::new(),
            pinyin_default: false,
            omit: None,
            type_names: None,
        }
    }
}

impl SearchOptions {
    /// Builds options from settings.
    pub fn from_settings(s: &fnc_core::settings::Settings) -> Self {
        let mut d = ModSet::EMPTY;
        let mut put = |m, on: bool| {
            if on {
                d.set(m, true);
            }
        };
        put(Modifier::Case, s.match_case);
        put(Modifier::WholeWord, s.match_whole_word);
        put(Modifier::Path, s.match_path);
        put(Modifier::Diacritics, s.match_diacritics);
        Self {
            precedence: s.operator_precedence,
            defaults: d,
            star_all: s.wildcards_star_all,
            reuse: s.reuse_results,
            regex_all: s.match_regex,
            pinyin_default: s.match_pinyin,
            ..Self::default()
        }
    }
}

/// One search request.
#[derive(Clone, Debug)]
pub struct SearchRequest {
    /// Caller-chosen id echoed in the outcome (monotonic per caller).
    pub id: u64,
    /// Search text.
    pub text: String,
    /// Active filters, AND-ed with `text` (`Some("pic:".into())` for a plain
    /// filter search).
    pub filter: Option<FilterSet>,
    /// Sort order.
    pub sort: SortSpec,
    /// Options.
    pub options: Arc<SearchOptions>,
}

/// Completed search.
#[derive(Clone, Debug)]
pub struct SearchOutcome {
    /// Request id.
    pub id: u64,
    /// Search text.
    pub text: String,
    /// Results in display order.
    pub results: Arc<ResultSet>,
    /// Plan (for highlighting and diagnostics).
    pub plan: Arc<PlannedQuery>,
    /// Parser diagnostics.
    pub parse_diagnostics: Vec<Diag>,
    /// Planner diagnostics.
    pub plan_diagnostics: Vec<PlanDiag>,
    /// Wall time from dequeue to result.
    pub elapsed: Duration,
}

/// Events emitted by the engine.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// A search finished.
    Done(SearchOutcome),
}

enum Cmd {
    Search(SearchRequest, CancelToken),
    Refresh(CancelToken),
    Shutdown,
}

/// Handle to the search thread.
pub struct SearchEngine {
    handle: EngineHandle,
    thread: Option<JoinHandle<()>>,
}

/// Cloneable, thread-safe handle for submitting searches and refreshes.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Cmd>,
    current: Arc<Mutex<CancelToken>>,
}

impl EngineHandle {
    /// Cancels the query in flight and enqueues `make(new_token)`. The send happens
    /// while holding the lock, so a concurrent submit cannot cancel this command's
    /// token before it is queued (which would silently drop the request).
    fn submit(&self, make: impl FnOnce(CancelToken) -> Cmd) {
        let tok = CancelToken::new();
        let mut cur = self.current.lock();
        cur.cancel();
        *cur = tok.clone();
        // Unbounded channel: never blocks while the lock is held.
        let _ = self.tx.send(make(tok));
    }

    /// Submits a search, cancelling any query in flight.
    pub fn search(&self, req: SearchRequest) {
        self.submit(|tok| Cmd::Search(req, tok));
    }

    /// Re-runs the last search against the current index (after index changes).
    pub fn refresh(&self) {
        self.submit(Cmd::Refresh);
    }
}

type Notify = Box<dyn Fn() + Send + Sync>;

struct Last {
    req: SearchRequest,
    plan: Arc<PlannedQuery>,
    results: Arc<ResultSet>,
}

/// Builds the AST for a request: filters (each with its own match options, combined by
/// OR unless `and-filter:` / [`FilterSet::and`]) AND the text; `regex_all` makes the
/// text one regex.
pub fn request_ast(req: &SearchRequest) -> (Expr, Vec<Diag>) {
    let lookup = |name: &str| std::env::var(name).ok();
    let env = ParseEnv { precedence: req.options.precedence, user_macros: &req.options.macros, env_var: Some(&lookup) };
    let mut diags = Vec::new();
    let mut and_filter = None;
    let text_ast = if req.options.regex_all && !req.text.is_empty() {
        Expr::Term(Term {
            mods: ModSet::EMPTY.with(Modifier::Regex, true),
            func: None,
            value: Value::Text(req.text.clone()),
            raw: 0..req.text.len(),
        })
    } else {
        let p = parse(&req.text, &env);
        diags = p.diagnostics;
        and_filter = p.and_filter;
        p.ast
    };
    let ast = match req.filter.as_ref().filter(|f| !f.is_empty()) {
        Some(f) => combine(f.expr(&env, and_filter), text_ast),
        None => text_ast,
    };
    (ast, diags)
}

impl SearchEngine {
    /// Starts the search thread. `events` receives outcomes; `notify` is called after
    /// each event (e.g. to wake the UI).
    pub fn spawn(index: SharedIndex, events: Sender<EngineEvent>, notify: Notify) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("fennec-search".into())
            // Generous stack: parsing and planning recurse over the (bounded) AST.
            .stack_size(8 << 20)
            .spawn(move || worker(index, rx, events, notify))
            .ok();
        if thread.is_none() {
            tracing::error!("cannot spawn search thread");
        }
        Self { handle: EngineHandle { tx, current: Arc::new(Mutex::new(CancelToken::new())) }, thread }
    }

    /// A cloneable handle (for the indexer, IPC, ...).
    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    /// Submits a search, cancelling any query in flight.
    pub fn search(&self, req: SearchRequest) {
        self.handle.search(req);
    }

    /// Re-runs the last search against the current index (after index changes).
    pub fn refresh(&self) {
        self.handle.refresh();
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        self.handle.current.lock().cancel();
        let _ = self.handle.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Sets the size of the global search thread pool (call once, before the first search).
pub fn configure_threads(threads: usize) -> Result<(), rayon::ThreadPoolBuildError> {
    rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("fennec-scan-{i}")).build_global()
}

fn worker(index: SharedIndex, rx: Receiver<Cmd>, events: Sender<EngineEvent>, notify: Notify) {
    let caches = Caches::new();
    // Last completed search (input for reuse) and the newest request seen.
    let mut last: Option<Last> = None;
    let mut latest: Option<SearchRequest> = None;
    while let Ok(first) = rx.recv() {
        // Coalesce: only the newest search matters; a refresh applies to it.
        let mut pending = Some(first);
        let mut job: Option<(CancelToken, bool)> = None;
        loop {
            match pending.take() {
                Some(Cmd::Shutdown) => return,
                Some(Cmd::Search(r, t)) => {
                    latest = Some(r);
                    job = Some((t, true));
                }
                Some(Cmd::Refresh(t)) => {
                    let reuse = job.as_ref().is_some_and(|j| j.1);
                    job = Some((t, reuse));
                }
                None => {}
            }
            match rx.try_recv() {
                Ok(c) => pending = Some(c),
                Err(_) => break,
            }
        }
        let (Some((tok, allow_reuse)), Some(req)) = (job, latest.clone()) else { continue };
        // A bug in one query must not kill the search thread (every later search would
        // be silently dropped), so panics are contained and reported as errors.
        let prev = if allow_reuse { last.as_ref() } else { None };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&index, &caches, &req, &tok, prev)));
        match outcome {
            Ok(Ok((outcome, plan))) => {
                last = Some(Last { req, plan, results: outcome.results.clone() });
                if events.send(EngineEvent::Done(outcome)).is_err() {
                    return;
                }
                notify();
            }
            Ok(Err(Cancelled)) => tracing::trace!(id = req.id, "search cancelled"),
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(id = req.id, text = %req.text, panic = %msg, "search panicked");
                caches.clear();
                last = None;
            }
        }
    }
}

fn run(
    index: &SharedIndex,
    caches: &Caches,
    req: &SearchRequest,
    cancel: &CancelToken,
    last: Option<&Last>,
) -> Result<(SearchOutcome, Arc<PlannedQuery>), Cancelled> {
    let t0 = Instant::now();
    let (ast, parse_diagnostics) = request_ast(req);
    let env = PlanEnv {
        defaults: req.options.defaults,
        star_all: req.options.star_all,
        date: fnc_query::date::DateEnv::now(),
        pinyin_default: req.options.pinyin_default,
        omit: req.options.omit.clone(),
    };
    caches.set_type_names(req.options.type_names.clone());
    let snap = index.snapshot();
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let last = last.filter(|l| l.results.is_current(&snap) && *l.req.options == *req.options);
    let same_search = last.is_some_and(|l| l.req.text == req.text && l.req.filter == req.filter);
    let (plan, mut results) = match last {
        // Only the sort (or nothing) changed: reuse the ids.
        Some(l) if same_search => {
            let mut r = (*l.results).clone();
            r.stats.reused = true;
            r.stats.examined = 0;
            r.stats.scan = Duration::ZERO;
            ((*l.plan).clone(), r)
        }
        _ => {
            let prev_plan = last.filter(|_| req.options.reuse).map(|l| &*l.plan);
            let plan = plan(&ast, &env, prev_plan);
            let ctx = ExecCtx { cancel, prev: last.map(|l| &*l.results), caches };
            let results = execute(&plan, &snap, &ctx)?;
            (plan, results)
        }
    };
    if results.sort != Some(req.sort) {
        let ts = Instant::now();
        sort_ids(&mut results.ids, &req.sort, &snap, caches, cancel)?;
        results.sort = Some(req.sort);
        results.stats.sort = ts.elapsed();
    }
    drop(snap);
    let plan = Arc::new(plan);
    let outcome = SearchOutcome {
        id: req.id,
        text: req.text.clone(),
        results: Arc::new(results),
        plan_diagnostics: plan.diagnostics.clone(),
        plan: plan.clone(),
        parse_diagnostics,
        elapsed: t0.elapsed(),
    };
    Ok((outcome, plan))
}

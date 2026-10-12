//! Background search service: owns the query thread, cancels superseded queries and
//! reuses the previous result set when a search only narrows (SPEC ch.0 §4.1, ch.1
//! §6.1). Used by the GUI, `fes` (offline mode), IPC and HTTP.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use fnc_core::settings::Precedence;
use fnc_index::SharedIndex;
use fnc_query::parser::{Diag, ParseEnv, UserMacro};
use fnc_query::plan::{PlanDiag, PlanEnv, PlannedQuery};
use fnc_query::{Expr, ModSet, Modifier, Term, Value, parse, plan};
use parking_lot::Mutex;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::exec::{ExecCtx, ResultSet, execute};
use crate::sort::{SortSpec, sort_ids};

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
    /// User macros.
    pub macros: Vec<UserMacro>,
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
            macros: Vec::new(),
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
    /// Active filter's search, AND-ed with `text`.
    pub filter: Option<String>,
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
    tx: Sender<Cmd>,
    current: Arc<Mutex<CancelToken>>,
    thread: Option<JoinHandle<()>>,
}

type Notify = Box<dyn Fn() + Send + Sync>;

struct Last {
    req: SearchRequest,
    plan: Arc<PlannedQuery>,
    results: Arc<ResultSet>,
}

/// Builds the AST for a request (filter AND text; `regex_all` makes the text one regex).
pub fn request_ast(req: &SearchRequest) -> (Expr, Vec<Diag>) {
    let lookup = |name: &str| std::env::var(name).ok();
    let env = ParseEnv { precedence: req.options.precedence, user_macros: &req.options.macros, env_var: Some(&lookup) };
    let mut diags = Vec::new();
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
        p.ast
    };
    let ast = match req.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        Some(f) => {
            let fp = parse(f, &env);
            match (fp.ast, text_ast) {
                (Expr::All, t) => t,
                (f, Expr::All) => f,
                (f, t) => Expr::And(vec![f, t]),
            }
        }
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
            .spawn(move || worker(index, rx, events, notify))
            .ok();
        if thread.is_none() {
            tracing::error!("cannot spawn search thread");
        }
        Self { tx, current: Arc::new(Mutex::new(CancelToken::new())), thread }
    }

    fn new_token(&self) -> CancelToken {
        let tok = CancelToken::new();
        let mut cur = self.current.lock();
        cur.cancel();
        *cur = tok.clone();
        tok
    }

    /// Submits a search, cancelling any query in flight.
    pub fn search(&self, req: SearchRequest) {
        let tok = self.new_token();
        let _ = self.tx.send(Cmd::Search(req, tok));
    }

    /// Re-runs the last search against the current index (after index changes).
    pub fn refresh(&self) {
        let tok = self.new_token();
        let _ = self.tx.send(Cmd::Refresh(tok));
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        self.current.lock().cancel();
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
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
        match run(&index, &caches, &req, &tok, if allow_reuse { last.as_ref() } else { None }) {
            Ok((outcome, plan)) => {
                last = Some(Last { req, plan, results: outcome.results.clone() });
                if events.send(EngineEvent::Done(outcome)).is_err() {
                    return;
                }
                notify();
            }
            Err(Cancelled) => tracing::trace!(id = req.id, "search cancelled"),
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
    };
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

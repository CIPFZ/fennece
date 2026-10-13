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

use fnc_props::content::ContentIndex;
use fnc_props::hash::HashCache;
use fnc_props::ondemand::OnDemand;
use fnc_props::store::PropertyIndex;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::exec::{ExecCtx, ResultSet, apply_post, base_ids_mut, execute_filter};
use crate::ext::{ExtCtx, Shared, SlowPolicy};
use crate::semantic::SemanticSource;
use crate::sort::{SortColumn, SortExt, SortSpec, sort_ids_with};
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
    /// Property index (`width:`, `artist:`, property sort columns); `None` = not indexed.
    pub props: Option<PropertyIndex>,
    /// Content index (`content:`); `None` = not indexed.
    pub content: Option<ContentIndex>,
    /// Hash cache shared with the app (persisted as `hashes.fnx`); `None` = an
    /// engine-private cache.
    pub hashes: Option<Shared<HashCache>>,
    /// File access for unindexed entries (slow path); `None` = no file reads.
    pub on_demand: Option<Shared<dyn OnDemand>>,
    /// Slow-path policy (file read budget per search).
    pub slow: SlowPolicy,
    /// Embedding index for `sem:` terms; `None` = semantic search off (zero cost).
    pub semantic: Option<Shared<dyn SemanticSource>>,
    /// Windows Search system index for `si:`; `None` = `si:` finds nothing (with a
    /// warning).
    pub system_index: Option<Shared<dyn crate::sysindex::SystemIndexSource>>,
    /// Smart mode: also send plain searches to the semantic channel and fuse the
    /// rankings (as if every search started with `?sem:`).
    pub semantic_fuse: bool,
    /// Formulas of the custom columns (`custom_columns` setting), by column index, for
    /// [`crate::SortKey::Custom`] sorts (`None` = the column's formula did not parse).
    /// Compared by identity: keep reusing the same `Arc`s while the setting is unchanged.
    pub formulas: Vec<Option<Shared<fnc_query::formula::Formula>>>,
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
            props: None,
            content: None,
            hashes: None,
            on_demand: None,
            slow: SlowPolicy::default(),
            semantic: None,
            system_index: None,
            semantic_fuse: false,
            formulas: Vec::new(),
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
    let (text, _) = fnc_query::semantic::take_switch(&req.text);
    let text_ast = if req.options.regex_all && !text.is_empty() {
        Expr::Term(Term {
            mods: ModSet::EMPTY.with(Modifier::Regex, true),
            func: None,
            value: Value::Text(text.clone()),
            raw: 0..text.len(),
        })
    } else {
        let p = parse(&text, &env);
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

/// Relevance order (the ids are sorted by name already, so ties stay in name order):
/// the fused rank when `?sem:` produced one, else by similarity; entries without a
/// score follow in their order. `best_first` is the descending direction.
fn relevance_order(results: &mut ResultSet, best_first: bool) {
    let Some(extra) = results.extra.clone() else { return };
    if !extra.fused_rank.is_empty() {
        let rank = &extra.fused_rank;
        results.ids.sort_by_key(|id| rank.get(&id.0).copied().unwrap_or(u32::MAX));
    } else {
        let s = &extra.sem_scores;
        results.ids.sort_by(|a, b| {
            let (x, y) = (s.get(&a.0).map(|v| v.0), s.get(&b.0).map(|v| v.0));
            y.is_some().cmp(&x.is_some()).then_with(|| y.unwrap_or(0.0).total_cmp(&x.unwrap_or(0.0)))
        });
    }
    if !best_first {
        results.ids.reverse();
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
    caches.set_type_names(req.options.type_names.clone());
    // Lock order: the index first, then the stores (the app never holds an index guard
    // while taking a store's write lock, see `fnc_props` docs).
    let snap = index.snapshot();
    let props = req.options.props.as_ref().map(PropertyIndex::snapshot);
    let content = req.options.content.as_ref().map(ContentIndex::snapshot);
    let ext = ExtCtx {
        props: props.as_deref(),
        content: content.as_deref(),
        hashes: req.options.hashes.as_ref().map(|h| &h.0),
        on_demand: req.options.on_demand.as_ref().map(|o| &o.0),
        policy: req.options.slow,
        semantic: req.options.semantic.as_ref().map(|v| &v.0),
        system_index: req.options.system_index.as_ref().map(|v| &v.0),
    };
    let env = PlanEnv {
        defaults: req.options.defaults,
        star_all: req.options.star_all,
        date: fnc_query::date::DateEnv::now(),
        pinyin_default: req.options.pinyin_default,
        omit: req.options.omit.clone(),
        indexed_props: props.as_deref().map(|p| p.indexed().to_vec()).unwrap_or_default(),
    };
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let ext_generation = ext.generation();
    let last = last.filter(|l| {
        l.results.is_current(&snap) && l.results.ext_generation == ext_generation && *l.req.options == *req.options
    });
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
            let results = execute_filter(&plan, &snap, &ctx, &ext)?;
            (plan, results)
        }
    };
    let fresh_post = !plan.post.is_empty() && results.extra.as_ref().is_none_or(|e| e.base.is_none());
    let mut resorted = false;
    if results.sort != Some(req.sort) || fresh_post {
        let ts = Instant::now();
        // With set functions, the pre-set results are sorted and the sets re-applied.
        let sx = SortExt { props: ext.props, formulas: &req.options.formulas };
        match base_ids_mut(&mut results) {
            Some(base) => sort_ids_with(base, &req.sort, &snap, caches, cancel, sx)?,
            None => sort_ids_with(&mut results.ids, &req.sort, &snap, caches, cancel, sx)?,
        }
        if req.sort.column == SortColumn::Relevance && results.has_relevance() {
            relevance_order(&mut results, !req.sort.ascending);
        }
        results.sort = Some(req.sort);
        results.stats.sort = ts.elapsed();
        resorted = true;
    }
    // `?sem:` / smart mode: the plain search also goes to the semantic channel; its
    // hits are fused with the literal results in their current order (RRF).
    let (fuse_text, switch) = fnc_query::semantic::take_switch(&req.text);
    let fuse = (switch || req.options.semantic_fuse)
        && !fuse_text.trim().is_empty()
        && !fnc_query::semantic::contains_semantic(&plan.root)
        && results.extra.as_ref().is_none_or(|e| e.fused_rank.is_empty());
    if fuse {
        let q = fnc_query::SemPred {
            text: fuse_text.trim().to_string(),
            kinds: fnc_query::semantic::kinds::ALL,
            min_score: None,
            top: None,
        };
        let hits = match ext.semantic {
            Some(src) => src.search(&q, &snap, None, src.default_top()).map_err(crate::ext::ExecWarning::Semantic),
            None => Err(crate::ext::ExecWarning::Semantic("semantic search is off".into())),
        };
        match hits {
            Ok(h) => {
                let hits: Vec<_> = h.into_iter().filter(|h| snap.is_live(h.id)).collect();
                let mut extra = results.extra.as_deref().cloned().unwrap_or_default();
                let fused = crate::semantic::fuse(&results.ids, &hits);
                extra.fused_rank = fused.iter().enumerate().map(|(r, id)| (id.0, r as u32)).collect();
                extra.sem_scores.extend(hits.iter().map(|h| (h.id.0, (h.score, h.range))));
                results.extra = Some(Arc::new(extra));
                if req.sort.column == SortColumn::Relevance {
                    results.ids = if req.sort.ascending { fused.into_iter().rev().collect() } else { fused };
                } else {
                    // Semantic-only hits join the results in the requested order.
                    let known: std::collections::HashSet<u32> = results.ids.iter().map(|id| id.0).collect();
                    results.ids.extend(hits.iter().map(|h| h.id).filter(|id| !known.contains(&id.0)));
                    let sx = SortExt { props: ext.props, formulas: &req.options.formulas };
                    sort_ids_with(&mut results.ids, &req.sort, &snap, caches, cancel, sx)?;
                }
            }
            Err(w) => {
                results.stats.errors.push(w.to_string());
                results.stats.warnings.push(w);
            }
        }
    }
    if resorted && !plan.post.is_empty() {
        apply_post(&plan, &mut results, &snap, caches, &ext, cancel)?;
    }
    drop(content);
    drop(props);
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

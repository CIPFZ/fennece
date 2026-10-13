//! Property, content and hash evaluation (SPEC ch.1 §8–9, ch.2 §5.3–5.9) and the
//! context the search engine passes for them.
//!
//! Indexed data is evaluated once per query at compile time (a pass over the store's
//! rows, or an inverted-index search for `content:`) into per-entry bit sets, so the
//! per-entry cost in the scan is a bit test. Entries the stores do not cover are
//! *unknown*: they match only `prop:unknown`, unless a slow path is enabled — then the
//! file is read through the [`OnDemand`] source (bounded by [`SlowPolicy::max_files`],
//! cancellable).

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use fnc_core::EntryId;
use fnc_index::IndexStore;
use fnc_index::ext::ext_of;
use fnc_props::content::{ContentMatcher, ContentStore, MatchScratch, TextEncoding, TextStats};
use fnc_props::hash::{HashAlgo, HashCache, HashKey, hex_prefix_matches};
use fnc_props::ondemand::OnDemand;
use fnc_props::store::{ColumnView, PropertyStore};
use fnc_props::value::float_key;
use fnc_props::{PropType, PropertyId, Value};
use fnc_query::plan::{ContentEnc, ContentPred, ContentSource, HashPred, NumPred, PatternKind, PropPred, PropTest};
use parking_lot::Mutex;

use crate::cache::{Caches, CancelToken};
use crate::eval::{Bits, Compiled, Scratch, TextLeaf};

/// Shared handle compared by identity (for `SearchOptions` equality).
pub struct Shared<T: ?Sized>(pub Arc<T>);

impl<T: ?Sized> Shared<T> {
    /// Wraps an `Arc`.
    pub fn new(a: Arc<T>) -> Self {
        Self(a)
    }
}

impl<T: ?Sized> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: ?Sized> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T: ?Sized> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Shared(..)")
    }
}

impl<T: ?Sized> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// When searches may read files for entries the indexes do not cover (SPEC ch.2 §8.6).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SlowPolicy {
    /// Read properties of unindexed entries without `notindexed:` (Everything gathers
    /// unindexed properties from disk). Default `true`; `notindexed:` always reads.
    pub props: bool,
    /// Read content of entries without indexed content (`content:`). Default `true`.
    pub content: bool,
    /// Compute hashes (`sha256:`, `dupe:size;sha256`). Default `true`.
    pub hashes: bool,
    /// At most this many file reads per search (the rest stay unknown and a
    /// [`ExecWarning::SlowLimit`] is reported). Default 10,000.
    pub max_files: usize,
}

impl Default for SlowPolicy {
    fn default() -> Self {
        Self { props: true, content: true, hashes: true, max_files: 10_000 }
    }
}

/// Non-fatal notes about a search (shown in the status bar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecWarning {
    /// The function's data is not indexed and cannot be read (no on-demand source or
    /// slow reads disabled): entries are unknown.
    NotIndexed(&'static str),
    /// The slow path stopped after this many files.
    SlowLimit(usize),
}

impl fmt::Display for ExecWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecWarning::NotIndexed(name) => {
                write!(f, "{name}: not indexed (enable the index or use notindexed:{name}:)")
            }
            ExecWarning::SlowLimit(n) => {
                write!(f, "slow search stopped after reading {n} files; add filters to narrow it")
            }
        }
    }
}

/// Extension data for one execution (all optional: absent = not indexed).
#[derive(Clone, Copy, Default)]
pub struct ExtCtx<'a> {
    /// Property store snapshot.
    pub props: Option<&'a PropertyStore>,
    /// Content store snapshot.
    pub content: Option<&'a ContentStore>,
    /// Hash cache.
    pub hashes: Option<&'a Arc<HashCache>>,
    /// File access for the slow paths.
    pub on_demand: Option<&'a Arc<dyn OnDemand>>,
    /// Slow-path policy.
    pub policy: SlowPolicy,
}

impl ExtCtx<'_> {
    /// Combined generation of the attached stores (result sets record it).
    pub fn generation(&self) -> u64 {
        let p = self.props.map_or(0, |p| p.generation().wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ p.instance());
        let c = self.content.map_or(0, |c| c.generation().wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ c.instance());
        p ^ c.rotate_left(17)
    }
}

/// Properties read from one file.
type Extracted = Arc<Vec<(PropertyId, Value)>>;

/// Per-query slow-path state shared by all extension leaves.
pub(crate) struct SlowState {
    cancel: CancelToken,
    budget: usize,
    used: AtomicUsize,
    exceeded: AtomicBool,
    on_demand: Option<Arc<dyn OnDemand>>,
    pub(crate) hashes: Arc<HashCache>,
    /// Properties wanted by the query's property leaves (one extraction per entry).
    wanted: Vec<PropertyId>,
    /// Per-query extraction results (entry → values).
    extracted: Mutex<HashMap<u32, Extracted>>,
    /// Content first-hit offsets and counts (entry → (offset, hits)).
    pub(crate) content_hits: Mutex<HashMap<u32, (u32, u32)>>,
    /// Long-lived cache of on-demand properties (across queries).
    cross: Arc<OnDemandCache>,
    pub(crate) warnings: Mutex<Vec<ExecWarning>>,
}

impl SlowState {
    pub(crate) fn new(ext: &ExtCtx<'_>, cancel: &CancelToken, caches: &Caches) -> Self {
        Self {
            cancel: cancel.clone(),
            budget: ext.policy.max_files,
            used: AtomicUsize::new(0),
            exceeded: AtomicBool::new(false),
            on_demand: ext.on_demand.cloned(),
            hashes: ext.hashes.cloned().unwrap_or_else(|| caches.query_hashes()),
            wanted: Vec::new(),
            extracted: Mutex::new(HashMap::new()),
            content_hits: Mutex::new(HashMap::new()),
            cross: caches.on_demand_props(),
            warnings: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn warn(&self, w: ExecWarning) {
        let mut v = self.warnings.lock();
        if !v.contains(&w) {
            v.push(w);
        }
    }

    /// Reserves one file read; `false` when cancelled or over budget.
    fn take_file(&self) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        if self.used.fetch_add(1, Ordering::Relaxed) >= self.budget {
            self.exceeded.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Final warnings (adds the budget note).
    pub(crate) fn finish(&self) -> Vec<ExecWarning> {
        if self.exceeded.load(Ordering::Relaxed) {
            self.warn(ExecWarning::SlowLimit(self.budget));
        }
        self.warnings.lock().clone()
    }

    pub(crate) fn want(&mut self, p: PropertyId) {
        for s in fnc_props::config::sources(p) {
            if !self.wanted.contains(s) {
                self.wanted.push(*s);
            }
        }
    }

    /// Extracted text of `id` read from the file (count properties of plain files).
    fn content_text(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> Option<String> {
        let src = self.on_demand.as_ref()?;
        if !self.take_file() {
            return None;
        }
        s.full_path(id, &mut sc.t);
        let ext = String::from_utf8_lossy(ext_of(s.fold(id))).into_owned();
        src.content(Path::new(&sc.t), &ext)
    }

    /// Properties of `id` read from the file (cached per query and across queries).
    fn properties(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> Option<Extracted> {
        if let Some(v) = self.extracted.lock().get(&id.0) {
            return Some(v.clone());
        }
        let key = HashKey::of(s, id);
        if let Some(k) = &key
            && let Some(v) = self.cross.get(k, &self.wanted)
        {
            self.extracted.lock().insert(id.0, v.clone());
            return Some(v);
        }
        let src = self.on_demand.as_ref()?;
        if !self.take_file() {
            return None;
        }
        s.full_path(id, &mut sc.t);
        let ext = String::from_utf8_lossy(ext_of(s.fold(id))).into_owned();
        let v = Arc::new(src.properties(Path::new(&sc.t), &ext, &self.wanted));
        if let Some(k) = key {
            self.cross.insert(k, &self.wanted, v.clone());
        }
        self.extracted.lock().insert(id.0, v.clone());
        Some(v)
    }
}

/// Cross-query cache of on-demand property extractions, keyed by file identity
/// (volume, FRN/path, size, date modified) and the requested property set.
#[derive(Default)]
pub struct OnDemandCache {
    map: Mutex<HashMap<(HashKey, Vec<PropertyId>), Extracted>>,
}

impl OnDemandCache {
    const MAX: usize = 200_000;

    fn get(&self, k: &HashKey, wanted: &[PropertyId]) -> Option<Extracted> {
        self.map.lock().get(&(*k, wanted.to_vec())).cloned()
    }

    fn insert(&self, k: HashKey, wanted: &[PropertyId], v: Extracted) {
        let mut m = self.map.lock();
        if m.len() >= Self::MAX {
            m.clear();
        }
        m.insert((k, wanted.to_vec()), v);
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ------------------------------------------------------------------ properties

/// A compiled property test.
pub(crate) struct PropMatch {
    id: PropertyId,
    test: PropTest,
    text: Option<Box<TextLeaf>>,
}

impl PropMatch {
    fn new(c: &mut Compiled, p: &PropPred, s: &IndexStore) -> Option<Self> {
        let text = match &p.test {
            PropTest::Text { pred, .. } => Some(Box::new(c.text_leaf(pred, s)?)),
            _ => None,
        };
        Some(Self { id: p.id, test: p.test.clone(), text })
    }

    /// Result for an entry known to have no value.
    fn absent(&self) -> bool {
        match &self.test {
            PropTest::Unknown => true,
            PropTest::Num(n) | PropTest::Dims(n, _) => n.unknown,
            _ => false,
        }
    }

    fn one_text(&self, t: &str, sc: &mut Scratch) -> bool {
        match &self.test {
            PropTest::Text { .. } => self.text.as_deref().is_some_and(|l| Compiled::match_str(l, t, sc)),
            PropTest::Len(n) => n.matches(t.encode_utf16().count() as i64),
            PropTest::Atoi(n) => leading_int(t).is_some_and(|v| n.matches(v)),
            PropTest::Atof(n) => leading_float(t).is_some_and(|v| n.matches(float_key(v))),
            PropTest::Known => true,
            PropTest::Unknown => false,
            PropTest::Num(n) => {
                // Numeric test on a text property: compare its leading number.
                leading_int(t).is_some_and(|v| n.matches(v))
            }
            PropTest::Count(_) | PropTest::Dims(..) => false,
        }
    }

    fn num(&self, v: i64) -> bool {
        match &self.test {
            PropTest::Num(n) => n.matches(v),
            PropTest::Known => true,
            PropTest::Unknown => false,
            _ => false,
        }
    }

    fn dims(&self, d: (i64, i64)) -> bool {
        match &self.test {
            PropTest::Dims(w, h) => w.matches(d.0) && h.matches(d.1),
            PropTest::Num(n) => n.matches(if self.id == PropertyId::ASPECT_RATIO && d.1 > 0 {
                float_key(d.0 as f64 / d.1 as f64)
            } else {
                d.0.saturating_mul(d.1)
            }),
            PropTest::Known => true,
            _ => false,
        }
    }

    /// Evaluates an owned extraction result.
    fn values(&self, vals: &[(PropertyId, Value)], sc: &mut Scratch) -> bool {
        let get = |p: PropertyId| vals.iter().find(|(q, _)| *q == p).map(|(_, v)| v);
        if self.id.is_derived() {
            let int = |p| match get(p) {
                Some(Value::Int(v)) => Some(*v),
                _ => None,
            };
            let d = match (int(PropertyId::WIDTH), int(PropertyId::HEIGHT)) {
                (Some(w), Some(h)) => Some((w, h)),
                _ => int(PropertyId::FRAME_WIDTH).zip(int(PropertyId::FRAME_HEIGHT)),
            };
            return d.map_or_else(|| self.absent(), |d| self.dims(d));
        }
        match get(self.id) {
            None => self.absent(),
            Some(Value::Text(t)) => match &self.test {
                PropTest::Count(c) => c.matches(1),
                _ => self.one_text(t, sc),
            },
            Some(Value::MultiText(items)) => match &self.test {
                PropTest::Count(c) => c.matches(items.len() as i64),
                PropTest::Text { all: true, .. } => items.iter().all(|t| self.one_text(t, sc)),
                _ => items.iter().any(|t| self.one_text(t, sc)),
            },
            Some(Value::Float(f)) => self.num(float_key(*f)),
            Some(v) => v.as_i64().is_some_and(|x| self.num(x)),
        }
    }
}

fn leading_int(t: &str) -> Option<i64> {
    let t = t.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().ok()
}

fn leading_float(t: &str) -> Option<f64> {
    let t = t.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().ok()
}

/// Compiled property leaf.
pub struct PropLeaf {
    hits: Bits,
    covered: Bits,
    absent: bool,
    slow: Option<(Arc<SlowState>, PropMatch)>,
    /// `line-count:` / `word-count:` / `char-count:` fall back to the text's statistics.
    stat: Option<fn(&TextStats) -> u32>,
}

/// Text statistic behind a count property (documents report them in their metadata;
/// other files are counted from their content).
fn text_stat(p: PropertyId) -> Option<fn(&TextStats) -> u32> {
    match p {
        PropertyId::LINE_COUNT => Some(|s| s.lines),
        PropertyId::WORD_COUNT => Some(|s| s.words),
        PropertyId::CHAR_COUNT => Some(|s| s.chars),
        _ => None,
    }
}

impl PropLeaf {
    /// Evaluates entry `id`.
    #[inline]
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        if self.covered.get(id.idx()) {
            return self.hits.get(id.idx());
        }
        match &self.slow {
            Some((st, m)) if !s.is_dir(id) => {
                let vals = st.properties(s, id, sc);
                if let Some(f) = self.stat
                    && vals.as_ref().is_none_or(|v| !v.iter().any(|(q, _)| *q == m.id))
                    && let Some(text) = st.content_text(s, id, sc)
                {
                    return m.num(i64::from(f(&fnc_props::content::text_stats(&text))));
                }
                match vals {
                    Some(vals) => m.values(&vals, sc),
                    None => self.absent,
                }
            }
            _ => self.absent,
        }
    }
}

/// Compiles a property predicate against the store (one pass over its rows).
pub(crate) fn compile_prop(
    c: &mut Compiled,
    p: &PropPred,
    s: &IndexStore,
    ext: &ExtCtx<'_>,
    slow: &Arc<SlowState>,
) -> Option<PropLeaf> {
    let m = PropMatch::new(c, p, s)?;
    let n = s.len();
    let mut hits = Bits::new(n);
    let mut covered = Bits::new(n);
    let mut sc = Scratch::default();
    let store = ext.props.filter(|st| st.instance() == s.instance());
    let sources = fnc_props::config::sources(p.id);
    if let Some(st) = store {
        let indexed = sources.iter().all(|q| st.is_indexed(*q))
            || (p.id.is_derived() && [PropertyId::WIDTH, PropertyId::HEIGHT].iter().all(|q| st.is_indexed(*q)));
        let ids = st.rows().ids();
        let absent = m.absent();
        let mark = |hits: &mut Bits, covered: &mut Bits, id: u32, has: bool, ok: bool| {
            if (id as usize) < n && (has || indexed) {
                covered.set(id as usize);
                if if has { ok } else { absent } {
                    hits.set(id as usize);
                }
            }
        };
        if p.id.is_derived() {
            for &id in ids.iter().filter(|&&i| i != fnc_props::rows::NONE) {
                let d = st.dimensions(EntryId(id));
                mark(&mut hits, &mut covered, id, d.is_some(), d.is_some_and(|d| m.dims(d)));
            }
        } else {
            match st.column(p.id) {
                ColumnView::Empty => {
                    for &id in ids.iter().filter(|&&i| i != fnc_props::rows::NONE) {
                        mark(&mut hits, &mut covered, id, false, false);
                    }
                }
                ColumnView::Int(v) => {
                    for (r, &id) in ids.iter().enumerate().filter(|(_, i)| **i != fnc_props::rows::NONE) {
                        let val = v.get(r).copied().filter(|&x| x != i64::MIN);
                        let ok = val.is_some_and(|x| match p.id.ty() {
                            PropType::Text | PropType::MultiText => false,
                            _ => m.num(x),
                        });
                        mark(&mut hits, &mut covered, id, val.is_some(), ok);
                    }
                }
                ColumnView::Float(v) => {
                    for (r, &id) in ids.iter().enumerate().filter(|(_, i)| **i != fnc_props::rows::NONE) {
                        let val = v.get(r).copied().filter(|x| !x.is_nan());
                        mark(&mut hits, &mut covered, id, val.is_some(), val.is_some_and(|f| m.num(float_key(f))));
                    }
                }
                ColumnView::Text(v, dict) => {
                    let dm: Vec<bool> = dict.iter().map(|t| m.one_text(t, &mut sc)).collect();
                    let count_one = matches!(&m.test, PropTest::Count(cn) if cn.matches(1));
                    for (r, &id) in ids.iter().enumerate().filter(|(_, i)| **i != fnc_props::rows::NONE) {
                        let val = v.get(r).copied().filter(|&x| x != fnc_props::rows::NONE);
                        let ok = val.is_some_and(|d| {
                            if matches!(m.test, PropTest::Count(_)) {
                                count_one
                            } else {
                                dm.get(d as usize).copied().unwrap_or(false)
                            }
                        });
                        mark(&mut hits, &mut covered, id, val.is_some(), ok);
                    }
                }
                ColumnView::Multi(v, lists, dict) => {
                    let dm: Vec<bool> = dict.iter().map(|t| m.one_text(t, &mut sc)).collect();
                    let all = matches!(m.test, PropTest::Text { all: true, .. });
                    for (r, &id) in ids.iter().enumerate().filter(|(_, i)| **i != fnc_props::rows::NONE) {
                        let list = v
                            .get(r)
                            .copied()
                            .filter(|&x| x != fnc_props::rows::NONE)
                            .and_then(|l| lists.get(l as usize));
                        let ok = list.is_some_and(|l| {
                            let hit = |d: &u32| dm.get(*d as usize).copied().unwrap_or(false);
                            match &m.test {
                                PropTest::Count(cn) => cn.matches(l.len() as i64),
                                _ if all => l.iter().all(hit),
                                _ => l.iter().any(hit),
                            }
                        });
                        mark(&mut hits, &mut covered, id, list.is_some(), ok);
                    }
                }
            }
        }
    }
    let stat = text_stat(p.id);
    if let (Some(f), Some(cs)) = (stat, ext.content.filter(|c| c.instance() == s.instance())) {
        for &id in st_ids(cs) {
            let e = EntryId(id);
            let has_prop = store.is_some_and(|st| st.get_ref(e, p.id).is_some());
            if id as usize >= n || has_prop {
                continue;
            }
            if let Some(ts) = cs.stats(e) {
                covered.set(id as usize);
                if m.num(i64::from(f(&ts))) {
                    hits.set(id as usize);
                }
            }
        }
    }
    let read = p.not_indexed || ext.policy.props;
    let slow_ok = read && slow.on_demand.is_some();
    if !slow_ok && store.is_none_or(|st| !sources.iter().all(|q| st.is_indexed(*q))) {
        slow.warn(ExecWarning::NotIndexed(p.id.name()));
    }
    let absent = m.absent();
    Some(PropLeaf { hits, covered, absent, slow: slow_ok.then(|| (slow.clone(), m)), stat })
}

// ------------------------------------------------------------------ content

fn encoding(e: ContentEnc) -> TextEncoding {
    match e {
        ContentEnc::Auto => TextEncoding::Auto,
        ContentEnc::Utf8 => TextEncoding::Utf8,
        ContentEnc::Ansi => TextEncoding::Ansi,
        ContentEnc::Utf16Le => TextEncoding::Utf16Le,
        ContentEnc::Utf16Be => TextEncoding::Utf16Be,
    }
}

/// Content matcher for a content predicate (`Err` = invalid regex).
pub(crate) fn content_matcher(c: &ContentPred) -> Result<ContentMatcher, String> {
    let t = &c.text;
    let r = match t.kind {
        PatternKind::Regex => ContentMatcher::regex(&t.pattern, t.case, t.regex_flags.0, t.regex_flags.1),
        PatternKind::Wildcard => ContentMatcher::wildcard(&t.pattern, t.case),
        // SPEC-GAP: `exact:`/`startwith:`/`endwith:` have no defined meaning for file
        // contents; they search for the text like a plain `content:` term.
        _ if t.whole_word => Ok(ContentMatcher::word(&t.pattern, t.case, t.diacritics)),
        _ => Ok(ContentMatcher::substring(&t.pattern, t.case, t.diacritics)),
    };
    r.map_err(|e| format!("invalid pattern `{}`: {e}", t.pattern))
}

/// Compiled content leaf.
pub struct ContentLeaf {
    hits: Bits,
    covered: Bits,
    counts: HashMap<u32, u32>,
    matcher: ContentMatcher,
    source: ContentSource,
    bytes: Vec<u8>,
    slow: Option<Arc<SlowState>>,
}

thread_local! {
    static MATCH_SCRATCH: std::cell::RefCell<MatchScratch> = std::cell::RefCell::new(MatchScratch::default());
}

impl ContentLeaf {
    /// `(first offset, hits)` of entry `id`, `None` if no match or unknown.
    fn find(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> Option<(u32, u32)> {
        if self.covered.get(id.idx()) {
            return self.hits.get(id.idx()).then(|| (0, self.counts.get(&id.0).copied().unwrap_or(1)));
        }
        let st = self.slow.as_ref()?;
        if s.is_dir(id) {
            return None;
        }
        let src = st.on_demand.as_ref()?;
        if !st.take_file() {
            return None;
        }
        s.full_path(id, &mut sc.t);
        let path = Path::new(&sc.t);
        let found = match self.source {
            ContentSource::Binary => {
                let b = src.bytes(path)?;
                let finder = memchr::memmem::Finder::new(&self.bytes);
                let mut it = finder.find_iter(&b);
                let first = it.next()?;
                Some((first as u32, 1 + it.count() as u32))
            }
            ContentSource::Plain(e) => {
                let text = src.text_as(path, encoding(e))?;
                MATCH_SCRATCH.with(|m| self.matcher.find(&text, &mut m.borrow_mut())).map(|(o, n)| (o as u32, n))
            }
            ContentSource::Indexed => {
                let ext = String::from_utf8_lossy(ext_of(s.fold(id))).into_owned();
                let text = src.content(path, &ext)?;
                MATCH_SCRATCH.with(|m| self.matcher.find(&text, &mut m.borrow_mut())).map(|(o, n)| (o as u32, n))
            }
        };
        if let Some(f) = found {
            st.content_hits.lock().insert(id.0, f);
        }
        found
    }

    /// Evaluates entry `id`.
    #[inline]
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        if self.covered.get(id.idx()) {
            return self.hits.get(id.idx());
        }
        self.slow.is_some() && self.find(s, id, sc).is_some()
    }
}

/// Compiles a content predicate (searches the content index once).
pub(crate) fn compile_content(
    c: &mut Compiled,
    p: &ContentPred,
    s: &IndexStore,
    ext: &ExtCtx<'_>,
    slow: &Arc<SlowState>,
    cancel: &CancelToken,
) -> Option<ContentLeaf> {
    let matcher = match content_matcher(p) {
        Ok(m) => m,
        Err(e) => {
            c.errors.push(e);
            return None;
        }
    };
    let n = s.len();
    let mut hits = Bits::new(n);
    let mut covered = Bits::new(n);
    let mut counts = HashMap::new();
    let store = ext.content.filter(|st| st.instance() == s.instance());
    if p.source == ContentSource::Indexed
        && let Some(st) = store
    {
        for &id in st_ids(st) {
            if (id as usize) < n {
                covered.set(id as usize);
            }
        }
        let found = st.search(&matcher, &|| cancel.is_cancelled())?;
        let mut first = slow.content_hits.lock();
        for h in found {
            if h.id.idx() < n {
                hits.set(h.id.idx());
                counts.insert(h.id.0, h.hits);
                first.insert(h.id.0, (h.offset, h.hits));
            }
        }
    }
    let read = ext.policy.content && slow.on_demand.is_some();
    if !read && (p.source != ContentSource::Indexed || store.is_none()) {
        slow.warn(ExecWarning::NotIndexed(match p.source {
            ContentSource::Indexed => "content",
            ContentSource::Plain(_) => "textplain",
            ContentSource::Binary => "binary",
        }));
    }
    Some(ContentLeaf {
        hits,
        covered,
        counts,
        matcher,
        source: p.source,
        bytes: p.bytes.clone(),
        slow: read.then(|| slow.clone()),
    })
}

fn st_ids(st: &ContentStore) -> impl Iterator<Item = &u32> {
    st.rows().ids().iter().filter(|&&i| i != fnc_props::rows::NONE)
}

/// `contenthits:` — total matches of the query's content terms.
pub struct ContentHitsLeaf {
    parts: Vec<ContentLeaf>,
    pred: NumPred,
}

impl ContentHitsLeaf {
    /// Evaluates entry `id`.
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        let total: i64 = self.parts.iter().filter_map(|p| p.find(s, id, sc)).map(|(_, n)| i64::from(n)).sum();
        self.pred.matches(total)
    }
}

/// Compiles `contenthits:`.
pub(crate) fn compile_content_hits(
    c: &mut Compiled,
    pred: &NumPred,
    parts: &[ContentPred],
    s: &IndexStore,
    ext: &ExtCtx<'_>,
    slow: &Arc<SlowState>,
    cancel: &CancelToken,
) -> Option<ContentHitsLeaf> {
    let parts = parts.iter().filter_map(|p| compile_content(c, p, s, ext, slow, cancel)).collect();
    Some(ContentHitsLeaf { parts, pred: pred.clone() })
}

// ------------------------------------------------------------------ hashes

/// Compiled hash leaf.
pub struct HashLeaf {
    algo: HashAlgo,
    prefixes: Vec<String>,
    unknown: bool,
    also: Vec<HashAlgo>,
    slow: Arc<SlowState>,
    compute: bool,
}

impl HashLeaf {
    /// Evaluates entry `id`.
    pub(crate) fn eval(&self, s: &IndexStore, id: EntryId, sc: &mut Scratch) -> bool {
        let Some(key) = HashKey::of_with(s, id, &mut sc.a) else { return false };
        let cache = &self.slow.hashes;
        if self.unknown {
            return cache.get(&key, self.algo).is_none();
        }
        let digest = match cache.get(&key, self.algo) {
            Some(d) => d,
            None => {
                if !self.compute || !self.slow.take_file() {
                    return false;
                }
                s.full_path(id, &mut sc.t);
                let cancel = &self.slow.cancel;
                match cache.get_or_compute(key, Path::new(&sc.t), self.algo, &self.also, &|| cancel.is_cancelled()) {
                    Ok(Some(d)) => d,
                    _ => return false,
                }
            }
        };
        self.prefixes.is_empty() || self.prefixes.iter().any(|p| hex_prefix_matches(&digest, p))
    }
}

/// Compiles a hash predicate.
pub(crate) fn compile_hash(p: &HashPred, all: &[HashAlgo], ext: &ExtCtx<'_>, slow: &Arc<SlowState>) -> HashLeaf {
    HashLeaf {
        algo: p.algo,
        prefixes: p.prefixes.clone(),
        unknown: p.unknown,
        also: all.iter().copied().filter(|a| *a != p.algo).collect(),
        slow: slow.clone(),
        compute: ext.policy.hashes,
    }
}

/// Highlight ranges (byte offsets in `text`) of the plan's content terms, at most `max`
/// (preview panel). Regex and substring semantics match the search.
pub fn content_highlights(plan: &fnc_query::PlannedQuery, text: &str, max: usize) -> Vec<std::ops::Range<usize>> {
    let mut preds = Vec::new();
    fnc_query::plan::collect_content(&plan.root, &mut preds);
    let mut sc = MatchScratch::default();
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for p in preds.iter().filter(|p| p.source != ContentSource::Binary) {
        if let Ok(m) = content_matcher(p) {
            out.extend(m.ranges(text, &mut sc, max));
        }
    }
    out.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(out.len());
    for r in out {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged.truncate(max);
    merged
}

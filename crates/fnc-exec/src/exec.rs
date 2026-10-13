//! Query execution: parallel filtered scan producing a [`ResultSet`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fnc_core::{EntryFlags, EntryId};
use fnc_index::IndexStore;
use fnc_query::plan::{InputSet, Node, PlannedQuery};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::eval::{CHUNK, Compiled, Scratch};
use crate::ext::{ExecWarning, ExtCtx};
use crate::sets::apply_sets;
use crate::sort::SortSpec;

/// Execution statistics.
#[derive(Clone, Debug, Default)]
pub struct ExecStats {
    /// Wall time of the scan (excluding sort).
    pub scan: Duration,
    /// Wall time of the sort.
    pub sort: Duration,
    /// Entries examined.
    pub examined: usize,
    /// The previous result set was reused as input.
    pub reused: bool,
    /// Invalid patterns, followed by the rendered [`ExecStats::warnings`] (front ends that
    /// show plain text need nothing else; localising ones use `warnings`).
    pub errors: Vec<String>,
    /// Non-fatal notes (unindexed functions, slow-path limits).
    pub warnings: Vec<ExecWarning>,
}

/// First content match of a result (preview panel, "hit" column).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContentHitInfo {
    /// Byte offset of the first match in the text (indexed text for indexed entries,
    /// else the extracted/decoded text).
    pub offset: u32,
    /// Number of matches.
    pub hits: u32,
}

/// Per-result data of the extension functions (absent for plain searches).
#[derive(Clone, Debug, Default)]
pub struct ResultExtra {
    /// Duplicate group number (1-based) per entry for `dupe:`-style searches.
    pub dupe_groups: HashMap<u32, u32>,
    /// First content hit per entry for `content:` searches.
    pub content_hits: HashMap<u32, (u32, u32)>,
    /// Results before the set functions (re-sorted and re-filtered when the sort changes).
    pub base: Option<Vec<EntryId>>,
}

/// Ordered result of a query.
#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    /// Matching entries in display order.
    pub ids: Vec<EntryId>,
    /// Index generation the ids are valid for.
    pub generation: u64,
    /// Index instance (id space).
    pub instance: u64,
    /// Current order of `ids` (`None` = id order).
    pub sort: Option<SortSpec>,
    /// Statistics.
    pub stats: ExecStats,
    /// [`ExtCtx::generation`] of the stores the results were computed with.
    pub ext_generation: u64,
    /// Extension data (duplicate groups, content hits).
    pub extra: Option<Arc<ResultExtra>>,
}

impl ResultSet {
    /// Number of results.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// `true` if the ids are valid for `s` without re-running the query.
    pub fn is_current(&self, s: &IndexStore) -> bool {
        self.generation == s.generation() && self.instance == s.instance()
    }

    /// Duplicate group number (1-based, by first appearance) of `id` in a `dupe:` search.
    pub fn dupe_group(&self, id: EntryId) -> Option<u32> {
        self.extra.as_ref()?.dupe_groups.get(&id.0).copied()
    }

    /// First content match of `id` in a `content:` search.
    pub fn content_hit(&self, id: EntryId) -> Option<ContentHitInfo> {
        self.extra.as_ref()?.content_hits.get(&id.0).map(|&(offset, hits)| ContentHitInfo { offset, hits })
    }
}

/// Inputs to [`execute`].
pub struct ExecCtx<'a> {
    /// Cancellation.
    pub cancel: &'a CancelToken,
    /// Previous result set (used when the plan's input is `Previous`).
    pub prev: Option<&'a ResultSet>,
    /// Shared caches.
    pub caches: &'a Caches,
}

#[inline]
fn visible(s: &IndexStore, id: EntryId) -> bool {
    !s.flags(id).contains(EntryFlags::DELETED)
}

/// Runs `plan` against snapshot `s` without property/content stores (their functions
/// see every entry as not indexed). Output preserves the input order: id order for a
/// full scan, the previous display order when filtering the previous result set; set
/// functions (`dupe:`) are applied in that order.
pub fn execute(plan: &PlannedQuery, s: &IndexStore, ctx: &ExecCtx<'_>) -> Result<ResultSet, Cancelled> {
    execute_ext(plan, s, ctx, &ExtCtx::default())
}

/// [`execute`] with property/content stores, hash cache and slow-path source.
pub fn execute_ext(
    plan: &PlannedQuery,
    s: &IndexStore,
    ctx: &ExecCtx<'_>,
    ext: &ExtCtx<'_>,
) -> Result<ResultSet, Cancelled> {
    let mut rs = execute_filter(plan, s, ctx, ext)?;
    apply_post(plan, &mut rs, s, ctx.caches, ext, ctx.cancel)?;
    Ok(rs)
}

/// The filter part of [`execute_ext`]: everything except the set functions. Callers
/// that sort call [`apply_post`] after sorting (Everything 1.5 semantics).
pub fn execute_filter(
    plan: &PlannedQuery,
    s: &IndexStore,
    ctx: &ExecCtx<'_>,
    ext: &ExtCtx<'_>,
) -> Result<ResultSet, Cancelled> {
    let t0 = Instant::now();
    let ext_generation = ext.generation();
    let prev = match (plan.input, ctx.prev) {
        (InputSet::Previous, Some(p))
            if p.is_current(s)
                && p.ext_generation == ext_generation
                && p.extra.as_ref().is_none_or(|e| e.base.is_none()) =>
        {
            Some(p)
        }
        _ => None,
    };
    let compiled = Compiled::with_ext(&plan.root, s, ctx.caches, ctx.cancel, ext)?;
    let cancel = ctx.cancel;
    let (ids, examined) = match prev {
        Some(p) => {
            let ids = filter_ids(&p.ids, s, &compiled, cancel)?;
            (ids, p.ids.len())
        }
        None => (scan_all(&plan.root, s, &compiled, cancel)?, s.len()),
    };
    let content_hits = compiled.content_hits();
    let warnings = compiled.warnings();
    let extra = (!content_hits.is_empty()).then(|| Arc::new(ResultExtra { content_hits, ..ResultExtra::default() }));
    Ok(ResultSet {
        ids,
        generation: s.generation(),
        instance: s.instance(),
        sort: prev.and_then(|p| p.sort),
        stats: ExecStats {
            scan: t0.elapsed(),
            sort: Duration::ZERO,
            examined,
            reused: prev.is_some(),
            errors: compiled.errors.iter().cloned().chain(warnings.iter().map(ToString::to_string)).collect(),
            warnings,
        },
        ext_generation,
        extra,
    })
}

/// Applies the plan's set functions (`dupe:`, `distinct:`, ...) to `rs` in its current
/// order. Idempotent: re-applying after a re-sort starts from the pre-set results.
pub fn apply_post(
    plan: &PlannedQuery,
    rs: &mut ResultSet,
    s: &IndexStore,
    caches: &Caches,
    ext: &ExtCtx<'_>,
    cancel: &CancelToken,
) -> Result<(), Cancelled> {
    if plan.post.is_empty() {
        return Ok(());
    }
    let mut extra = rs.extra.as_deref().cloned().unwrap_or_default();
    let base = extra.base.take().unwrap_or_else(|| rs.ids.clone());
    let out = apply_sets(&plan.post, &base, s, ext, &caches.query_hashes(), cancel)?;
    rs.ids = out.ids;
    for w in out.warnings {
        if !rs.stats.warnings.contains(&w) {
            rs.stats.errors.push(w.to_string());
            rs.stats.warnings.push(w);
        }
    }
    extra.dupe_groups = out.groups;
    extra.base = Some(base);
    rs.extra = Some(Arc::new(extra));
    Ok(())
}

/// Sorts the pre-set results of `rs` (when set functions were applied) so that
/// [`apply_post`] can re-run in a new order.
pub fn base_ids_mut(rs: &mut ResultSet) -> Option<&mut Vec<EntryId>> {
    rs.extra.as_mut().map(Arc::make_mut).and_then(|e| e.base.as_mut())
}

fn scan_all(root: &Node, s: &IndexStore, c: &Compiled, cancel: &CancelToken) -> Result<Vec<EntryId>, Cancelled> {
    let n = s.len();
    let chunks = n.div_ceil(CHUNK);
    let match_all = *root == Node::All;
    let parts: Vec<Option<Vec<EntryId>>> = (0..chunks)
        .into_par_iter()
        .map_init(Scratch::default, |sc, k| {
            if cancel.is_cancelled() {
                return None;
            }
            let lo = k * CHUNK;
            let hi = (lo + CHUNK).min(n);
            let mut out = Vec::new();
            for i in lo..hi {
                let id = EntryId(i as u32);
                if visible(s, id) && (match_all || c.matches(s, id, sc)) {
                    out.push(id);
                }
            }
            Some(out)
        })
        .collect();
    concat(parts, cancel)
}

fn filter_ids(
    input: &[EntryId],
    s: &IndexStore,
    c: &Compiled,
    cancel: &CancelToken,
) -> Result<Vec<EntryId>, Cancelled> {
    let parts: Vec<Option<Vec<EntryId>>> = input
        .par_chunks(CHUNK)
        .map_init(Scratch::default, |sc, chunk| {
            if cancel.is_cancelled() {
                return None;
            }
            Some(chunk.iter().copied().filter(|&id| s.is_live(id) && c.matches(s, id, sc)).collect())
        })
        .collect();
    concat(parts, cancel)
}

fn concat(parts: Vec<Option<Vec<EntryId>>>, cancel: &CancelToken) -> Result<Vec<EntryId>, Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let total = parts.iter().map(|p| p.as_ref().map_or(0, Vec::len)).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        out.extend(p.ok_or(Cancelled)?);
    }
    Ok(out)
}

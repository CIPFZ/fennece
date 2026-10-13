//! Set functions on the sorted result set (`dupe:`, `distinct:`, `unique:`,
//! `dupe-count:`, `dupe-max:`; SPEC ch.1 §11, ch.2 §5.2) — Everything 1.5 semantics:
//! applied to the current results after sorting, not to the whole index.
//!
//! Each key maps an entry to an exact class id (equal values ↔ equal ids); entries whose
//! key is unknown (folders for `size`, missing dates or properties, files that could not
//! be hashed) are never duplicates. Hash keys are computed only for entries that share
//! their size and earlier keys with another result.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fnc_core::EntryId;
use fnc_index::IndexStore;
use fnc_index::ext::{ext_of, stem_of};
use fnc_props::hash::{HashAlgo, HashCache, HashKey};
use fnc_props::store::{PropertyStore, ValueRef};
use fnc_props::value::float_key;
use fnc_query::plan::{DupeKey, SetKind, SetOp};
use rayon::prelude::*;

use crate::cache::{CancelToken, Cancelled};
use crate::ext::{ExecWarning, ExtCtx};

/// Result of [`apply_sets`].
#[derive(Clone, Debug, Default)]
pub struct SetOutput {
    /// Kept entries in input order.
    pub ids: Vec<EntryId>,
    /// Duplicate group (1-based, numbered by first appearance) of kept entries that
    /// belong to a group of two or more.
    pub groups: HashMap<u32, u32>,
    /// Warnings (hash budget, unindexed properties).
    pub warnings: Vec<ExecWarning>,
}

const UNKNOWN: u64 = u64::MAX;

/// Maps a value onto a class id distinct from [`UNKNOWN`].
fn cls(v: u64) -> u64 {
    if v == UNKNOWN { UNKNOWN - 1 } else { v }
}

/// Interns byte strings into dense class ids.
#[derive(Default)]
struct Classes<'a> {
    map: HashMap<&'a [u8], u64>,
}

impl<'a> Classes<'a> {
    fn id(&mut self, b: &'a [u8]) -> u64 {
        let n = self.map.len() as u64;
        *self.map.entry(b).or_insert(n)
    }
}

/// Class ids of `key` for `ids` (`UNKNOWN` = never a duplicate). `alive` marks entries
/// that still share all previous keys with another entry (hashes are computed only for
/// those).
#[allow(clippy::too_many_arguments)]
fn classes(
    key: DupeKey,
    ids: &[EntryId],
    alive: &[bool],
    s: &IndexStore,
    props: Option<&PropertyStore>,
    hashes: &HashCache,
    ext: &ExtCtx<'_>,
    cancel: &CancelToken,
    warnings: &mut Vec<ExecWarning>,
) -> Result<Vec<u64>, Cancelled> {
    let size = |id: EntryId| if s.is_dir(id) { None } else { s.size(id) };
    let known = |t: fnc_core::FileTime| if t.is_known() { cls(t.0 as u64) } else { UNKNOWN };
    Ok(match key {
        DupeKey::Name | DupeKey::NamePart | DupeKey::Ext => {
            let mut c = Classes::default();
            ids.iter()
                .map(|&id| {
                    let f = s.fold(id);
                    let b = match key {
                        DupeKey::Name => f,
                        DupeKey::NamePart => {
                            if s.is_dir(id) {
                                f
                            } else {
                                stem_of(f)
                            }
                        }
                        _ => {
                            if s.is_dir(id) {
                                return UNKNOWN;
                            }
                            ext_of(f)
                        }
                    };
                    c.id(b)
                })
                .collect()
        }
        DupeKey::Size => ids.iter().map(|&id| size(id).unwrap_or(UNKNOWN)).collect(),
        DupeKey::Attrib => {
            ids.iter().map(|&id| u64::from(s.attributes(id) | if s.is_dir(id) { 0x10 } else { 0 })).collect()
        }
        DupeKey::DateAccessed => ids.iter().map(|&id| known(s.date_accessed(id))).collect(),
        DupeKey::DateCreated => ids.iter().map(|&id| known(s.date_created(id))).collect(),
        DupeKey::DateModified => ids.iter().map(|&id| known(s.date_modified(id))).collect(),
        DupeKey::Path => ids.iter().map(|&id| u64::from(s.parent(id).0)).collect(),
        DupeKey::Prop(p) => {
            let Some(st) = props.filter(|st| st.instance() == s.instance()) else {
                warnings.push(ExecWarning::NotIndexed(p.name()));
                return Ok(vec![UNKNOWN; ids.len()]);
            };
            let mut text: HashMap<&str, u64> = HashMap::new();
            ids.iter()
                .map(|&id| {
                    if p.is_derived() {
                        return st.dimensions(id).map_or(UNKNOWN, |(w, h)| cls(((w as u64) << 32) ^ (h as u64)));
                    }
                    match st.get_ref(id, p) {
                        None => UNKNOWN,
                        Some(ValueRef::Int(v)) => cls(v as u64),
                        Some(ValueRef::Float(f)) => cls(float_key(f) as u64),
                        Some(ValueRef::Text(t)) => {
                            let n = text.len() as u64;
                            *text.entry(t).or_insert(n)
                        }
                        Some(ValueRef::Multi(l, d)) => {
                            let joined: String = l
                                .iter()
                                .filter_map(|&i| d.get(i as usize))
                                .map(|s| &**s)
                                .collect::<Vec<_>>()
                                .join("\u{1}");
                            cls(xxhash_rust::xxh3::xxh3_64(joined.as_bytes()))
                        }
                    }
                })
                .collect()
        }
        DupeKey::Hash(algo) => hash_classes(algo, ids, alive, s, hashes, ext, cancel, warnings)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn hash_classes(
    algo: HashAlgo,
    ids: &[EntryId],
    alive: &[bool],
    s: &IndexStore,
    hashes: &HashCache,
    ext: &ExtCtx<'_>,
    cancel: &CancelToken,
    warnings: &mut Vec<ExecWarning>,
) -> Result<Vec<u64>, Cancelled> {
    // Equal hashes imply equal sizes: only entries sharing a size with another alive
    // entry are hashed.
    let mut by_size: HashMap<u64, u32> = HashMap::new();
    for (k, &id) in ids.iter().enumerate() {
        if alive[k]
            && !s.is_dir(id)
            && let Some(sz) = s.size(id)
        {
            *by_size.entry(sz).or_default() += 1;
        }
    }
    let budget = ext.policy.max_files;
    let used = AtomicUsize::new(0);
    let compute = ext.policy.hashes;
    let digests: Vec<Option<Box<[u8]>>> = ids
        .par_iter()
        .enumerate()
        .map_init(String::new, |path, (k, &id)| {
            if cancel.is_cancelled() || !alive[k] {
                return None;
            }
            let key = HashKey::of(s, id)?;
            if by_size.get(&key.size).copied().unwrap_or(0) < 2 {
                return None;
            }
            if let Some(d) = hashes.get(&key, algo) {
                return Some(d);
            }
            if !compute || used.fetch_add(1, Ordering::Relaxed) >= budget {
                return None;
            }
            s.full_path(id, path);
            hashes.get_or_compute(key, Path::new(path.as_str()), algo, &[], &|| cancel.is_cancelled()).ok().flatten()
        })
        .collect();
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    if used.load(Ordering::Relaxed) > budget {
        warnings.push(ExecWarning::SlowLimit(budget));
    }
    let mut map: HashMap<&[u8], u64> = HashMap::new();
    Ok(digests
        .iter()
        .map(|d| match d {
            Some(d) => {
                let n = map.len() as u64;
                *map.entry(d).or_insert(n)
            }
            None => UNKNOWN,
        })
        .collect())
}

/// Group index per entry (`u32::MAX` = no group / unknown key) and group sizes.
fn group(ids: &[EntryId], keys: &[Vec<u64>]) -> (Vec<u32>, Vec<u32>) {
    let mut map: HashMap<Vec<u64>, u32> = HashMap::new();
    let mut sizes = Vec::new();
    let mut out = Vec::with_capacity(ids.len());
    let mut buf = Vec::with_capacity(keys.len());
    for k in 0..ids.len() {
        buf.clear();
        buf.extend(keys.iter().map(|c| c[k]));
        if buf.contains(&UNKNOWN) {
            out.push(u32::MAX);
            continue;
        }
        let g = match map.get(&buf) {
            Some(&g) => g,
            None => {
                let g = sizes.len() as u32;
                map.insert(buf.clone(), g);
                sizes.push(0);
                g
            }
        };
        sizes[g as usize] += 1;
        out.push(g);
    }
    (out, sizes)
}

/// Applies `ops` in order to `ids` (already in display order).
pub fn apply_sets(
    ops: &[SetOp],
    ids: &[EntryId],
    s: &IndexStore,
    ext: &ExtCtx<'_>,
    fallback_hashes: &Arc<HashCache>,
    cancel: &CancelToken,
) -> Result<SetOutput, Cancelled> {
    let mut cur: Vec<EntryId> = ids.to_vec();
    let mut groups = HashMap::new();
    let mut warnings = Vec::new();
    let hashes: &HashCache = ext.hashes.map_or(&**fallback_hashes, |h| &**h);
    for op in ops {
        if let SetKind::Limit(n) = op.kind {
            cur.truncate(usize::try_from(n).unwrap_or(usize::MAX));
            continue;
        }
        let keys = op.keys.clone().unwrap_or_else(|| vec![DupeKey::Name]);
        let mut cls: Vec<Vec<u64>> = Vec::with_capacity(keys.len());
        for &k in &keys {
            // Entries still sharing every earlier key with another entry.
            let alive: Vec<bool> = if cls.is_empty() {
                vec![true; cur.len()]
            } else {
                let (g, sizes) = group(&cur, &cls);
                g.iter().map(|&g| g != u32::MAX && sizes[g as usize] > 1).collect()
            };
            cls.push(classes(k, &cur, &alive, s, ext.props, hashes, ext, cancel, &mut warnings)?);
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
        }
        let (g, sizes) = group(&cur, &cls);
        let size_of = |k: usize| if g[k] == u32::MAX { 1 } else { sizes[g[k] as usize] };
        let mut seen = vec![0u32; sizes.len()];
        let mut keep = Vec::with_capacity(cur.len());
        for k in 0..cur.len() {
            let rank = if g[k] == u32::MAX {
                0
            } else {
                let r = seen[g[k] as usize];
                seen[g[k] as usize] += 1;
                r
            };
            let n = size_of(k);
            let mut ok = match &op.kind {
                SetKind::Dupe => n > 1,
                SetKind::Distinct => rank == 0,
                SetKind::Unique => n == 1,
                SetKind::DupeCount(p) => p.matches(i64::from(n)),
                SetKind::DupeMax(m) => rank < *m,
                SetKind::Limit(_) => true,
            };
            if op.negate {
                ok = !ok;
            }
            keep.push(ok);
        }
        // Group numbers by first appearance among kept duplicates.
        groups.clear();
        let mut number: HashMap<u32, u32> = HashMap::new();
        let mut next = Vec::with_capacity(cur.len());
        for (k, &id) in cur.iter().enumerate() {
            if !keep[k] {
                continue;
            }
            if g[k] != u32::MAX && size_of(k) > 1 {
                let n = number.len() as u32 + 1;
                let gn = *number.entry(g[k]).or_insert(n);
                groups.insert(id.0, gn);
            }
            next.push(id);
        }
        cur = next;
    }
    warnings.dedup();
    Ok(SetOutput { ids: cur, groups, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnc_query::{ParseEnv, PlanEnv, parse, plan};

    fn store() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\a\\x.txt\",10,1,,32\n\
                   \"C:\\b\\x.txt\",10,2,,33\n\
                   \"C:\\c\\x.txt\",20,2,,32\n\
                   \"C:\\a\\y.txt\",20,3,,32\n\
                   \"C:\\a\\x.doc\",30,3,,32\n\
                   \"C:\\b\\z.md\",30,,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s
    }

    fn run(s: &IndexStore, q: &str) -> (Vec<String>, HashMap<u32, u32>) {
        let p = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let ids: Vec<EntryId> = (0..s.len() as u32)
            .map(EntryId)
            .filter(|&i| s.is_live(i) && !s.is_dir(i) && s.parent(i).is_some())
            .collect();
        let out =
            apply_sets(&p.post, &ids, s, &ExtCtx::default(), &Arc::new(HashCache::new()), &CancelToken::new()).unwrap();
        let names = out
            .ids
            .iter()
            .map(|&i| {
                let mut p = String::new();
                s.full_path(i, &mut p);
                p
            })
            .collect();
        (names, out.groups)
    }

    #[test]
    fn dupe_variants() {
        let s = store();
        assert_eq!(run(&s, "dupe:").0, ["C:\\a\\x.txt", "C:\\b\\x.txt", "C:\\c\\x.txt"]);
        assert_eq!(run(&s, "dupe:name;size").0, ["C:\\a\\x.txt", "C:\\b\\x.txt"]);
        assert_eq!(run(&s, "sizedupe:").0.len(), 6);
        assert_eq!(run(&s, "namepartdupe:").0, ["C:\\a\\x.txt", "C:\\b\\x.txt", "C:\\c\\x.txt", "C:\\a\\x.doc"]);
        assert_eq!(run(&s, "attribdupe:").0.len(), 5);
        assert_eq!(run(&s, "dmdupe:").0.len(), 4, "unknown date is never a duplicate");
        assert_eq!(run(&s, "dupe:ext").0.len(), 4);
        assert_eq!(run(&s, "dupe:path").0.len(), 5);
        assert_eq!(run(&s, "distinct:").0, ["C:\\a\\x.txt", "C:\\a\\y.txt", "C:\\a\\x.doc", "C:\\b\\z.md"]);
        assert_eq!(run(&s, "unique:").0, ["C:\\a\\y.txt", "C:\\a\\x.doc", "C:\\b\\z.md"]);
        assert_eq!(run(&s, "!dupe:").0, ["C:\\a\\y.txt", "C:\\a\\x.doc", "C:\\b\\z.md"]);
        assert_eq!(run(&s, "dupe-count:3").0.len(), 3);
        assert_eq!(run(&s, "dupe-count:2").0.len(), 0);
        assert_eq!(run(&s, "dupe-count:1").0.len(), 3);
        assert_eq!(run(&s, "dupe:size dupe-max:1").0, ["C:\\a\\x.txt", "C:\\c\\x.txt", "C:\\a\\x.doc"]);
        assert_eq!(run(&s, "count:2").0, ["C:\\a\\x.txt", "C:\\b\\x.txt"]);
        assert_eq!(run(&s, "count:2 dupe:").0, ["C:\\a\\x.txt", "C:\\b\\x.txt"], "limit after the set");
        assert_eq!(run(&s, "count:0").0.len(), 0);
        assert_eq!(run(&s, "count:99").0.len(), 6);
        let (names, groups) = run(&s, "sizedupe:");
        assert_eq!(names.len(), 6);
        let mut g: Vec<u32> = groups.values().copied().collect();
        g.sort();
        g.dedup();
        assert_eq!(g, [1, 2, 3]);
    }

    #[test]
    fn hash_dupes_only_hash_same_size_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.bin"), b"same").unwrap();
        std::fs::write(root.join("b.bin"), b"same").unwrap();
        std::fs::write(root.join("c.bin"), b"diff").unwrap();
        std::fs::write(root.join("d.bin"), b"unique size").unwrap();
        let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
        for n in ["a", "b", "c", "d"] {
            let p = root.join(format!("{n}.bin"));
            let len = std::fs::metadata(&p).unwrap().len();
            efu.push_str(&format!("\"{}\",{len},5,,32\n", p.display()));
        }
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        let p = plan(&parse("dupe:size;sha256", &ParseEnv::default()).ast, &PlanEnv::default(), None);
        let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i) && !s.is_dir(i)).collect();
        let cache = Arc::new(HashCache::new());
        let out = apply_sets(&p.post, &ids, &s, &ExtCtx::default(), &cache, &CancelToken::new()).unwrap();
        let names: Vec<String> = out.ids.iter().map(|&i| s.name_string(i)).collect();
        assert_eq!(names, ["a.bin", "b.bin"]);
        assert_eq!(cache.len(), 3, "d.bin (unique size) was never hashed");
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(apply_sets(&p.post, &ids, &s, &ExtCtx::default(), &cache, &cancel).is_err());
    }
}

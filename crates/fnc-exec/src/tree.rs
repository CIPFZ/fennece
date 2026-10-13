//! Per-generation aggregates over the folder tree and name lookup.
//!
//! * [`TreeAggregates`] — descendant file/folder counts and aggregate folder sizes,
//!   accumulated bottom-up in one pass (`descendantcount:`, `foldersize:`).
//! * [`LinkCounts`] — hard link count per file (`hardlinkcount:`).
//! * [`NameLookup`] — `(parent, folded name) → entry` for resolving paths
//!   (`fileexists:`, `filelist:`), sorted by `(parent, hash)` and binary-searched.
//!
//! All are built on first use and cached in [`crate::Caches`] until the index
//! generation changes.

use fnc_core::EntryId;
use fnc_index::IndexStore;
use rayon::prelude::*;

use crate::cache::ChildCounts;
use crate::eval::CHUNK;

/// Folders in breadth-first order from the volume roots (parents before children).
pub fn folders_breadth_first(s: &IndexStore) -> Vec<EntryId> {
    let mut order: Vec<EntryId> = Vec::with_capacity(s.stats().folders + 16);
    for v in s.volumes() {
        order.extend(v.roots.iter().copied().filter(|&r| s.is_live(r)));
    }
    let mut k = 0;
    // Bounded by the entry count even if the tree were corrupt (cyclic).
    while k < order.len() && order.len() <= s.len() {
        let d = order[k];
        k += 1;
        order.extend(s.children(d).filter(|&c| s.is_dir(c)));
    }
    order
}

/// Descendant counts and aggregate sizes per folder.
pub struct TreeAggregates {
    /// `(instance, generation)`.
    pub key: (u64, u64),
    /// Files at any depth below each folder.
    pub files: Vec<u32>,
    /// Folders at any depth below each folder.
    pub folders: Vec<u32>,
    /// Total size of the files below each folder (unknown sizes count as 0).
    pub bytes: Vec<u64>,
}

impl TreeAggregates {
    /// Builds from the direct child counts of the same generation.
    pub fn build(s: &IndexStore, direct: &ChildCounts) -> Self {
        let n = s.len();
        let mut files = direct.files.clone();
        let mut folders = direct.folders.clone();
        let mut bytes = vec![0u64; n];
        for i in 0..n {
            let id = EntryId(i as u32);
            if !s.is_live(id) || s.is_dir(id) {
                continue;
            }
            let p = s.parent(id);
            if p.is_some()
                && let Some(b) = bytes.get_mut(p.idx())
            {
                *b = b.saturating_add(s.size(id).unwrap_or(0));
            }
        }
        // Children before parents: each folder's totals are final when added upward.
        for &d in folders_breadth_first(s).iter().rev() {
            let p = s.parent(d);
            if !p.is_some() || p.idx() >= n {
                continue;
            }
            let (f, fo, b) = (files[d.idx()], folders[d.idx()], bytes[d.idx()]);
            files[p.idx()] = files[p.idx()].saturating_add(f);
            folders[p.idx()] = folders[p.idx()].saturating_add(fo);
            bytes[p.idx()] = bytes[p.idx()].saturating_add(b);
        }
        Self { key: (s.instance(), s.generation()), files, folders, bytes }
    }
}

/// Hard link counts per entry (folders: 1).
pub struct LinkCounts {
    /// `(instance, generation)`.
    pub key: (u64, u64),
    /// Count per row, saturated at `u16::MAX`.
    pub counts: Vec<u16>,
}

impl LinkCounts {
    /// Builds the column (parallel).
    pub fn build(s: &IndexStore) -> Self {
        let counts = (0..s.len() as u32)
            .into_par_iter()
            .map(|i| {
                let id = EntryId(i);
                if !s.is_live(id) || s.is_dir(id) { 1 } else { u16::try_from(s.hardlink_count(id)).unwrap_or(u16::MAX) }
            })
            .collect();
        Self { key: (s.instance(), s.generation()), counts }
    }
}

/// FNV-1a over a folded name.
#[inline]
fn name_hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3))
}

/// `(parent, name hash) → entry` index of all live non-root entries.
pub struct NameLookup {
    /// `(instance, generation)`.
    pub key: (u64, u64),
    rows: Vec<(u32, u64, u32)>,
}

impl NameLookup {
    /// Builds the index (parallel hash + sort).
    pub fn build(s: &IndexStore) -> Self {
        let n = s.len();
        let mut rows: Vec<(u32, u64, u32)> = (0..n.div_ceil(CHUNK))
            .into_par_iter()
            .flat_map_iter(|k| {
                (k * CHUNK..((k + 1) * CHUNK).min(n)).filter_map(|i| {
                    let id = EntryId(i as u32);
                    let p = s.parent(id);
                    (s.is_live(id) && p.is_some()).then(|| (p.0, name_hash(s.fold(id)), id.0))
                })
            })
            .collect();
        rows.par_sort_unstable();
        Self { key: (s.instance(), s.generation()), rows }
    }

    /// Live child of `parent` whose fold key is `fold`.
    pub fn child(&self, s: &IndexStore, parent: EntryId, fold: &[u8]) -> Option<EntryId> {
        let h = name_hash(fold);
        let start = self.rows.partition_point(|r| (r.0, r.1) < (parent.0, h));
        self.rows[start..]
            .iter()
            .take_while(|r| r.0 == parent.0 && r.1 == h)
            .map(|r| EntryId(r.2))
            .find(|&c| s.is_live(c) && s.parent(c) == parent && s.fold(c) == fold)
    }

    /// Heap bytes.
    pub fn heap_bytes(&self) -> usize {
        self.rows.capacity() * std::mem::size_of::<(u32, u64, u32)>()
    }
}

/// Folded root paths (`\` separators, no trailing separator), longest first.
pub fn root_paths(s: &IndexStore) -> Vec<(Vec<u8>, EntryId)> {
    let mut out: Vec<(Vec<u8>, EntryId)> = Vec::new();
    for v in s.volumes() {
        for &r in &v.roots {
            if !s.is_live(r) {
                continue;
            }
            let mut p: Vec<u8> = s.fold(r).iter().map(|&b| if b == b'/' { b'\\' } else { b }).collect();
            while p.last() == Some(&b'\\') {
                p.pop();
            }
            if !p.is_empty() {
                out.push((p, r));
            }
        }
    }
    out.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    out
}

/// Resolves a folded path (`\` or `/` separators). Absolute paths start with a root
/// path; anything else is relative to `base` (`.` and `..` allowed).
pub fn resolve_path(
    s: &IndexStore,
    path: &[u8],
    base: Option<EntryId>,
    roots: &[(Vec<u8>, EntryId)],
    child: impl Fn(EntryId, &[u8]) -> Option<EntryId>,
) -> Option<EntryId> {
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    let root = roots.iter().find(|(rp, _)| {
        path.len() >= rp.len()
            && path[..rp.len()].iter().zip(rp).all(|(&a, &b)| a == b || (is_sep(a) && b == b'\\'))
            && path.get(rp.len()).is_none_or(|&b| is_sep(b))
    });
    let (mut cur, rest) = match root {
        Some((rp, r)) => (*r, &path[rp.len()..]),
        None => (base?, path),
    };
    for comp in rest.split(|&b| is_sep(b)) {
        match comp {
            b"" | b"." => {}
            b".." => {
                cur = s.parent(cur);
                if !cur.is_some() {
                    return None;
                }
            }
            name => cur = child(cur, name)?,
        }
    }
    Some(cur)
}

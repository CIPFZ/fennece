//! Result sorting (SPEC ch.3 §4.5).
//!
//! Name order uses the index's name fast-sort ranks where valid (a `u32` compare) and
//! falls back to the natural comparator for entries renamed since the ranks were built;
//! the mix is still a consistent total order because clean ranks agree with the
//! comparator. Other columns break ties by name.
//!
//! Keys computed per result run before the comparison sort, in parallel: property
//! columns (store values), custom formula columns ([`SortKey::Custom`], the `eval:`
//! language) and the type description ([`SortColumn::Type`], one lookup per extension).

use std::cmp::Ordering;
use std::collections::HashMap;

use fnc_core::EntryId;
use fnc_core::settings::FoldersFirst;
use fnc_index::IndexStore;
use fnc_index::ext::{OVERFLOW_EXT, ext_of};
use fnc_props::{PropertyId, PropertyStore};
use fnc_query::formula::{Formula, Val};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled};
use crate::ext::Shared;

/// Sortable column.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SortColumn {
    /// Name (natural order).
    Name,
    /// Folder path, then name.
    Path,
    /// Size (folders/unknown first).
    Size,
    /// Extension, then name.
    Extension,
    /// Type description (`SearchOptions::type_names`, "File folder", "File", "`EXT`
    /// File"), then name.
    Type,
    /// Date modified.
    DateModified,
    /// Date created.
    DateCreated,
    /// Date accessed.
    DateAccessed,
    /// Attributes value.
    Attributes,
    /// Run count.
    RunCount,
    /// Date run.
    DateRun,
    /// Date recently changed.
    DateRecentlyChanged,
    /// Full path (folder path, then name; same order as [`SortColumn::Path`]).
    FullPath,
    /// Volume (root path), then name.
    Volume,
    /// Index source kind (`ntfs`, `folder`, `filelist`), then name.
    IndexSource,
    /// Depth (root = 0).
    Depth,
    /// Name length.
    NameLength,
    /// File reference number.
    Frn,
    /// Semantic relevance (best first). Only searches with semantic terms have it; the
    /// engine orders by the stored similarities, plain sorts treat it as a tie.
    Relevance,
}

impl SortColumn {
    /// Parses `es.exe`/HTTP style names (`name`, `path`, `size`, `date-modified`, ...).
    pub fn parse(s: &str) -> Option<Self> {
        let n: String = s.chars().filter(|c| *c != '-' && *c != '_').collect::<String>().to_ascii_lowercase();
        Some(match n.as_str() {
            "name" | "filename" => Self::Name,
            "path" => Self::Path,
            "fullpath" | "pathandname" => Self::FullPath,
            "size" => Self::Size,
            "ext" | "extension" => Self::Extension,
            "type" | "typename" => Self::Type,
            "dm" | "datemodified" => Self::DateModified,
            "dc" | "datecreated" => Self::DateCreated,
            "da" | "dateaccessed" => Self::DateAccessed,
            "attrib" | "attributes" => Self::Attributes,
            "runcount" => Self::RunCount,
            "dr" | "daterun" => Self::DateRun,
            "rc" | "daterecentlychanged" => Self::DateRecentlyChanged,
            "volume" | "drive" => Self::Volume,
            "indexsource" | "volumekind" => Self::IndexSource,
            "depth" | "parentscount" => Self::Depth,
            "namelength" | "len" => Self::NameLength,
            "frn" => Self::Frn,
            "relevance" | "score" => Self::Relevance,
            _ => return None,
        })
    }
}

/// Sort order.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SortSpec {
    /// Column.
    pub column: SortColumn,
    /// Direction.
    pub ascending: bool,
    /// Folder placement.
    pub folders_first: FoldersFirst,
    /// Secondary and tertiary keys (`Ctrl`+click on a column header), each with its
    /// own direction; ties after all keys are broken by name.
    pub then: [Option<(SortColumn, bool)>; 2],
    /// Property columns: when `props[k]` is set, key `k` (0 = `column`, 1–2 = `then`)
    /// sorts by that property (unknown values first) and its `SortColumn` is ignored.
    pub props: [Option<PropertyId>; 3],
    /// Custom formula columns: when `custom[k]` is set, key `k` sorts by the value of
    /// formula `SearchOptions::formulas[custom[k]]` (numbers numerically, dates by time,
    /// text naturally; errors and empty values last in both directions) and its
    /// `SortColumn` is ignored.
    pub custom: [Option<u8>; 3],
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            ascending: true,
            folders_first: FoldersFirst::Ascending,
            then: [None; 2],
            props: [None; 3],
            custom: [None; 3],
        }
    }
}

/// One sort key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SortKey {
    /// A built-in column.
    Column(SortColumn),
    /// A property column.
    Property(PropertyId),
    /// Custom formula column `i` (0-based index into `SearchOptions::formulas`).
    Custom(u8),
}

impl SortSpec {
    /// Single-key sort.
    pub fn by(column: SortColumn, ascending: bool, folders_first: FoldersFirst) -> Self {
        Self { column, ascending, folders_first, then: [None; 2], props: [None; 3], custom: [None; 3] }
    }

    /// Single-key sort by a property column.
    pub fn by_property(p: PropertyId, ascending: bool, folders_first: FoldersFirst) -> Self {
        let mut s = Self::by(SortColumn::Name, ascending, folders_first);
        s.props[0] = Some(p);
        s
    }

    /// Single-key sort by custom formula column `i`.
    pub fn by_custom(i: u8, ascending: bool, folders_first: FoldersFirst) -> Self {
        let mut s = Self::by(SortColumn::Name, ascending, folders_first);
        s.custom[0] = Some(i);
        s
    }

    /// Single-key sort by any key.
    pub fn by_key(key: SortKey, ascending: bool, folders_first: FoldersFirst) -> Self {
        match key {
            SortKey::Column(c) => Self::by(c, ascending, folders_first),
            SortKey::Property(p) => Self::by_property(p, ascending, folders_first),
            SortKey::Custom(i) => Self::by_custom(i, ascending, folders_first),
        }
    }

    /// Key `k` (0 = primary) as set in the spec.
    fn key_at(&self, k: usize, col: SortColumn) -> SortKey {
        match (self.props[k], self.custom[k]) {
            (Some(p), _) => SortKey::Property(p),
            (None, Some(i)) => SortKey::Custom(i),
            (None, None) => SortKey::Column(col),
        }
    }

    /// All keys in priority order with their directions, property and custom columns
    /// included.
    pub fn sort_keys(&self) -> impl Iterator<Item = (SortKey, bool)> + '_ {
        let cols = std::iter::once(Some((self.column, self.ascending))).chain(self.then.iter().copied());
        cols.enumerate().filter_map(|(k, c)| {
            let (col, asc) = c?;
            Some((self.key_at(k, col), asc))
        })
    }

    /// Adds (or flips, if present) a secondary key of any kind; keeps at most three keys.
    pub fn push_sort_key(&mut self, key: SortKey, ascending: bool) {
        if self.key_at(0, self.column) == key {
            self.ascending = !self.ascending;
            return;
        }
        for k in 0..2 {
            if let Some((col, _)) = self.then[k]
                && self.key_at(k + 1, col) == key
                && let Some(slot) = self.then[k].as_mut()
            {
                slot.1 = !slot.1;
                return;
            }
        }
        let k = self.then.iter().position(Option::is_none).unwrap_or(1);
        let (col, prop, custom) = match key {
            SortKey::Column(c) => (c, None, None),
            SortKey::Property(p) => (SortColumn::Name, Some(p), None),
            SortKey::Custom(i) => (SortColumn::Name, None, Some(i)),
        };
        self.then[k] = Some((col, ascending));
        self.props[k + 1] = prop;
        self.custom[k + 1] = custom;
    }

    /// Adds (or flips, if present) a secondary property key; keeps at most three keys.
    pub fn push_property_key(&mut self, p: PropertyId, ascending: bool) {
        self.push_sort_key(SortKey::Property(p), ascending);
    }

    /// Adds (or flips, if present) a secondary custom formula key.
    pub fn push_custom_key(&mut self, i: u8, ascending: bool) {
        self.push_sort_key(SortKey::Custom(i), ascending);
    }

    /// Adds (or flips, if present) a secondary key; keeps at most three keys.
    pub fn push_key(&mut self, column: SortColumn, ascending: bool) {
        self.push_sort_key(SortKey::Column(column), ascending);
    }

    /// All keys in priority order with their directions.
    pub fn keys(&self) -> impl Iterator<Item = (SortColumn, bool)> + '_ {
        std::iter::once((self.column, self.ascending)).chain(self.then.iter().flatten().copied())
    }

    /// `true` if key `k` is a plain column (`col`) rather than a property/custom key.
    fn is_column(&self, k: usize) -> bool {
        self.props[k].is_none() && self.custom[k].is_none()
    }
}

/// Parallel sort with folder grouping and direction applied around `key_cmp`.
fn par_sort(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    key_cmp: impl Fn(EntryId, EntryId) -> Ordering + Sync,
) {
    let group = match spec.folders_first {
        FoldersFirst::Always | FoldersFirst::Ascending => true,
        FoldersFirst::Never => false,
    };
    let dirs_first_always = spec.folders_first == FoldersFirst::Always;
    ids.par_sort_unstable_by(|&a, &b| {
        let g = if group { s.is_dir(b).cmp(&s.is_dir(a)) } else { Ordering::Equal };
        if dirs_first_always {
            g.then_with(|| if spec.ascending { key_cmp(a, b) } else { key_cmp(b, a) })
        } else {
            let o = g.then_with(|| key_cmp(a, b));
            if spec.ascending { o } else { o.reverse() }
        }
    });
}

/// Sorts `ids` in place (property columns sort as unknown).
pub fn sort_ids(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
) -> Result<(), Cancelled> {
    sort_ids_ext(ids, spec, s, caches, cancel, None)
}

/// Sorts `ids` in place; property columns read `props` (custom columns sort as empty).
pub fn sort_ids_ext(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
    props: Option<&PropertyStore>,
) -> Result<(), Cancelled> {
    sort_ids_with(ids, spec, s, caches, cancel, SortExt { props, formulas: &[] })
}

/// Data for the computed sort keys.
#[derive(Clone, Copy, Default)]
pub struct SortExt<'a> {
    /// Property store (property columns; `None` = all unknown).
    pub props: Option<&'a PropertyStore>,
    /// Custom column formulas by index (`None` / missing = empty values).
    pub formulas: &'a [Option<Shared<Formula>>],
}

/// Value of a custom column as a sort key: numbers < dates < text, then errors and empty.
#[derive(Debug)]
enum FormulaKey {
    Num(f64),
    Date(i64),
    Text(Vec<u8>),
    Error,
    Empty,
}

impl FormulaKey {
    fn of(v: Val) -> Self {
        match v {
            Val::Num(n) => FormulaKey::Num(n),
            Val::Bool(b) => FormulaKey::Num(f64::from(u8::from(b))),
            Val::Date(t) => FormulaKey::Date(t),
            Val::Text(t) if t.is_empty() => FormulaKey::Empty,
            Val::Text(t) => FormulaKey::Text(fnc_core::fold::fold_str(&t).into_bytes()),
            Val::Error(_) => FormulaKey::Error,
            Val::Empty => FormulaKey::Empty,
        }
    }

    fn class(&self) -> u8 {
        match self {
            FormulaKey::Num(_) => 0,
            FormulaKey::Date(_) => 1,
            FormulaKey::Text(_) => 2,
            FormulaKey::Error => 3,
            FormulaKey::Empty => 4,
        }
    }

    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (FormulaKey::Num(a), FormulaKey::Num(b)) => a.total_cmp(b),
            (FormulaKey::Date(a), FormulaKey::Date(b)) => a.cmp(b),
            (FormulaKey::Text(a), FormulaKey::Text(b)) => fnc_core::natsort::natural_cmp(a, b),
            _ => self.class().cmp(&other.class()),
        }
    }
}

/// Dense ranks (by entry index) of custom formula `f` over `ids`; equal values share a
/// rank. Errors and empty values rank last in the final direction `ascending`.
fn formula_ranks(
    ids: &[EntryId],
    s: &IndexStore,
    props: Option<&PropertyStore>,
    f: Option<&Formula>,
    ascending: bool,
) -> Vec<u32> {
    let keys: Vec<FormulaKey> = ids
        .par_iter()
        .map(|&id| match f {
            Some(f) => FormulaKey::of(f.eval(&crate::formula::StoreVars { s, id, props })),
            None => FormulaKey::Empty,
        })
        .collect();
    let mut order: Vec<u32> = (0..keys.len() as u32).collect();
    order.par_sort_unstable_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
    let mut rank = vec![0u32; s.len()];
    let mut r = 2u32;
    for (n, &i) in order.iter().enumerate() {
        let key = &keys[i as usize];
        if n > 0 && key.cmp(&keys[order[n - 1] as usize]) != Ordering::Equal {
            r += 1;
        }
        let v = match (key, ascending) {
            (FormulaKey::Error, true) => u32::MAX - 1,
            (FormulaKey::Empty, true) => u32::MAX,
            (FormulaKey::Error, false) => 1,
            (FormulaKey::Empty, false) => 0,
            _ => r,
        };
        if let Some(slot) = rank.get_mut(ids[i as usize].idx()) {
            *slot = v;
        }
    }
    rank
}

/// Ranks of the type descriptions of `ids` (natural order of the folded descriptions;
/// one provider lookup per extension).
struct TypeRanks {
    by_ext: Vec<u32>,
    folder: u32,
    overflow: HashMap<Vec<u8>, u32>,
}

impl TypeRanks {
    fn build(ids: &[EntryId], s: &IndexStore, caches: &Caches) -> Self {
        let d = s.ext_dict();
        let mut used = vec![false; 1 << 16];
        let mut folder = false;
        let mut overflow: HashMap<Vec<u8>, u32> = HashMap::new();
        for &id in ids {
            if s.is_dir(id) {
                folder = true;
            } else {
                match s.ext_id(id) {
                    OVERFLOW_EXT => {
                        overflow.entry(ext_of(s.fold(id)).to_vec()).or_insert(0);
                    }
                    e => used[usize::from(e)] = true,
                }
            }
        }
        enum Slot {
            Ext(u16),
            Folder,
            Overflow(Vec<u8>),
        }
        let fold = |t: &str| fnc_core::fold::fold_str(t).into_bytes();
        let mut descs: Vec<(Vec<u8>, Slot)> = used
            .iter()
            .enumerate()
            .filter(|(_, u)| **u)
            .map(|(e, _)| (fold(&caches.type_description(d.name(e as u16))), Slot::Ext(e as u16)))
            .collect();
        if folder {
            descs.push((fold(crate::types::FOLDER_TYPE), Slot::Folder));
        }
        for ext in overflow.keys() {
            descs.push((fold(&caches.type_description(ext)), Slot::Overflow(ext.clone())));
        }
        descs.sort_by(|a, b| fnc_core::natsort::natural_cmp(&a.0, &b.0));
        let mut out = TypeRanks { by_ext: vec![0; 1 << 16], folder: 0, overflow };
        let mut r = 0u32;
        for (n, (desc, slot)) in descs.iter().enumerate() {
            if n > 0 && descs[n - 1].0 != *desc {
                r += 1;
            }
            match slot {
                Slot::Ext(e) => out.by_ext[usize::from(*e)] = r,
                Slot::Folder => out.folder = r,
                Slot::Overflow(x) => {
                    out.overflow.insert(x.clone(), r);
                }
            }
        }
        out
    }

    fn rank(&self, s: &IndexStore, id: EntryId) -> u32 {
        if s.is_dir(id) {
            return self.folder;
        }
        match s.ext_id(id) {
            OVERFLOW_EXT => self.overflow.get(ext_of(s.fold(id))).copied().unwrap_or(u32::MAX),
            e => self.by_ext[usize::from(e)],
        }
    }
}

/// Sorts `ids` in place; property columns read `ext.props`, custom columns evaluate
/// `ext.formulas`.
pub fn sort_ids_with(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
    ext: SortExt<'_>,
) -> Result<(), Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let props = ext.props;
    // Property keys: dense key columns for the results (unknown = i64::MIN, first).
    let prop_keys: Vec<Option<Vec<i64>>> = if spec.props.iter().any(Option::is_some) {
        let st = props.filter(|p| p.instance() == s.instance());
        spec.props
            .iter()
            .map(|p| {
                p.map(|p| {
                    let ranks = st.map(|st| st.text_ranks(p)).unwrap_or_default();
                    let mut v = vec![i64::MIN; s.len()];
                    if let Some(st) = st {
                        for &id in ids.iter() {
                            if let Some(slot) = v.get_mut(id.idx()) {
                                *slot = st.sort_key(id, p, &ranks);
                            }
                        }
                    }
                    v
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    // Custom formula keys: ranks of the values computed in parallel.
    let custom_keys: Vec<Option<Vec<u32>>> = if spec.custom.iter().any(Option::is_some) {
        let st = props.filter(|p| p.instance() == s.instance());
        let directions = [Some(spec.ascending), spec.then[0].map(|t| t.1), spec.then[1].map(|t| t.1)];
        spec.custom
            .iter()
            .zip(directions)
            .enumerate()
            .map(|(k, (c, asc))| {
                let i = (*c)?;
                if spec.props[k].is_some() {
                    return None;
                }
                let f = ext.formulas.get(usize::from(i)).and_then(|f| f.as_ref()).map(|f| &*f.0);
                Some(formula_ranks(ids, s, st, f, asc.unwrap_or(true)))
            })
            .collect()
    } else {
        Vec::new()
    };
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let order = s.name_order();
    let by_name = |a: EntryId, b: EntryId| -> Ordering {
        if order.is_clean(a) && order.is_clean(b) { order.rank(a).cmp(&order.rank(b)) } else { s.cmp_names(a, b) }
    };
    let needs_dir_rank = spec.keys().any(|(c, _)| matches!(c, SortColumn::Path | SortColumn::FullPath));
    let dir_rank = needs_dir_rank.then(|| caches.dir_rank(s));
    let needs_types = spec.keys().enumerate().any(|(k, (c, _))| c == SortColumn::Type && spec.is_column(k));
    let type_ranks = needs_types.then(|| TypeRanks::build(ids, s, caches));
    let size_key = |id: EntryId| if s.is_dir(id) { -1 } else { s.size(id).map_or(-1, |v| v as i128) };
    let volume_root = |id: EntryId| s.volume_info(s.volume_of(id)).map_or("", |v| v.info.root_path.as_str());
    let volume_kind = |id: EntryId| s.volume_info(s.volume_of(id)).map_or("", |v| v.info.kind.as_str());
    let column_cmp = |col: SortColumn, a: EntryId, b: EntryId| -> Ordering {
        match col {
            SortColumn::Name => by_name(a, b),
            SortColumn::Path | SortColumn::FullPath => {
                let r = dir_rank.as_deref().map(|d| &d.rank);
                let pr = |id: EntryId| {
                    let p = s.parent(id);
                    if p.is_some() { r.and_then(|r| r.get(p.idx()).copied()).unwrap_or(u32::MAX) } else { 0 }
                };
                pr(a).cmp(&pr(b))
            }
            SortColumn::Size => size_key(a).cmp(&size_key(b)),
            SortColumn::Extension => {
                let d = s.ext_dict();
                d.name(s.ext_id(a)).cmp(d.name(s.ext_id(b)))
            }
            SortColumn::Type => match type_ranks.as_ref() {
                Some(t) => t.rank(s, a).cmp(&t.rank(s, b)),
                None => Ordering::Equal,
            },
            SortColumn::DateModified => s.date_modified(a).cmp(&s.date_modified(b)),
            SortColumn::DateCreated => s.date_created(a).cmp(&s.date_created(b)),
            SortColumn::DateAccessed => s.date_accessed(a).cmp(&s.date_accessed(b)),
            SortColumn::Attributes => s.attributes(a).cmp(&s.attributes(b)),
            SortColumn::RunCount => s.run_info(a).count.cmp(&s.run_info(b).count),
            SortColumn::DateRun => s.run_info(a).last.cmp(&s.run_info(b).last),
            SortColumn::DateRecentlyChanged => s.date_recently_changed(a).cmp(&s.date_recently_changed(b)),
            SortColumn::Volume => volume_root(a).cmp(volume_root(b)),
            SortColumn::IndexSource => volume_kind(a).cmp(volume_kind(b)),
            SortColumn::Depth => s.depth(a).cmp(&s.depth(b)),
            SortColumn::NameLength => s.name(a).len().cmp(&s.name(b).len()),
            SortColumn::Frn => s.frn(a).cmp(&s.frn(b)),
            SortColumn::Relevance => Ordering::Equal,
        }
    };
    let pkey = |k: usize, a: EntryId, b: EntryId| -> Option<Ordering> {
        if let Some(v) = prop_keys.get(k).and_then(Option::as_ref) {
            return Some(v.get(a.idx()).cmp(&v.get(b.idx())));
        }
        let v = custom_keys.get(k)?.as_ref()?;
        Some(v.get(a.idx()).cmp(&v.get(b.idx())))
    };
    if !prop_keys.is_empty() || !custom_keys.is_empty() {
        par_sort(ids, spec, s, |a, b| {
            let mut o = pkey(0, a, b).unwrap_or_else(|| column_cmp(spec.column, a, b));
            for (k, &(col, asc)) in spec.then.iter().enumerate().filter_map(|(k, t)| t.as_ref().map(|t| (k, t))) {
                if o != Ordering::Equal {
                    break;
                }
                let c = pkey(k + 1, a, b).unwrap_or_else(|| column_cmp(col, a, b));
                o = if asc == spec.ascending { c } else { c.reverse() };
            }
            o.then_with(|| by_name(a, b))
        });
        return if cancel.is_cancelled() { Err(Cancelled) } else { Ok(()) };
    }
    // The common single-key sorts get their own monomorphised comparator (a runtime
    // column switch per comparison costs ~35% on 1M entries).
    if spec.then.iter().all(Option::is_none) {
        match spec.column {
            SortColumn::Name => par_sort(ids, spec, s, by_name),
            SortColumn::Size => {
                par_sort(ids, spec, s, |a, b| size_key(a).cmp(&size_key(b)).then_with(|| by_name(a, b)))
            }
            SortColumn::DateModified => {
                par_sort(ids, spec, s, |a, b| s.date_modified(a).cmp(&s.date_modified(b)).then_with(|| by_name(a, b)))
            }
            SortColumn::DateCreated => {
                par_sort(ids, spec, s, |a, b| s.date_created(a).cmp(&s.date_created(b)).then_with(|| by_name(a, b)))
            }
            SortColumn::Path | SortColumn::FullPath => {
                let r = dir_rank.as_deref().map(|d| &d.rank[..]).unwrap_or(&[]);
                let pr = |id: EntryId| {
                    let p = s.parent(id);
                    if p.is_some() { r.get(p.idx()).copied().unwrap_or(u32::MAX) } else { 0 }
                };
                par_sort(ids, spec, s, |a, b| pr(a).cmp(&pr(b)).then_with(|| by_name(a, b)))
            }
            col => par_sort(ids, spec, s, |a, b| column_cmp(col, a, b).then_with(|| by_name(a, b))),
        }
    } else {
        // Primary key in the caller's direction (applied by `par_sort`); later keys carry
        // their own.
        par_sort(ids, spec, s, |a, b| {
            let mut o = column_cmp(spec.column, a, b);
            for &(col, asc) in spec.then.iter().flatten() {
                if o != Ordering::Equal {
                    break;
                }
                let k = column_cmp(col, a, b);
                o = if asc == spec.ascending { k } else { k.reverse() };
            }
            o.then_with(|| by_name(a, b))
        });
    }
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn store() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\a\\x.txt\",10,3,,32\n\
                   \"C:\\a\\y.txt\",10,1,,32\n\
                   \"C:\\b\\z.txt\",10,2,,32\n\
                   \"C:\\b\\w.txt\",5,9,,32\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s.rebuild_name_order();
        s
    }

    fn names(s: &IndexStore, spec: &SortSpec) -> Vec<String> {
        let mut ids: Vec<EntryId> =
            (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i) && !s.is_dir(i)).collect();
        sort_ids(&mut ids, spec, s, &Caches::default(), &CancelToken::default()).unwrap();
        ids.iter().map(|&i| s.name_string(i)).collect()
    }

    #[test]
    fn secondary_keys_break_ties_in_their_own_direction() {
        let s = store();
        let mut spec = SortSpec::by(SortColumn::Size, false, FoldersFirst::Never);
        // Ties follow the primary direction (names descending too).
        assert_eq!(names(&s, &spec), ["z.txt", "y.txt", "x.txt", "w.txt"]);
        // Size descending, then date modified ascending.
        spec.push_key(SortColumn::DateModified, true);
        assert_eq!(names(&s, &spec), ["y.txt", "z.txt", "x.txt", "w.txt"]);
        // Clicking the secondary again flips it.
        spec.push_key(SortColumn::DateModified, true);
        assert_eq!(names(&s, &spec), ["x.txt", "z.txt", "y.txt", "w.txt"]);
        // Clicking the primary flips the primary.
        spec.push_key(SortColumn::Size, true);
        assert_eq!(names(&s, &spec)[0], "w.txt");
        assert_eq!(spec.keys().count(), 2);
        spec.push_key(SortColumn::Path, true);
        spec.push_key(SortColumn::Extension, true);
        assert_eq!(spec.keys().count(), 3, "at most three keys");
    }

    fn typed_store() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\d\",,,,16\n\
                   \"C:\\d\\b.zip\",30,1,,32\n\
                   \"C:\\d\\a.txt\",5,2,,32\n\
                   \"C:\\d\\c.log\",100,3,,32\n\
                   \"C:\\d\\photo.jpg\",7,4,,32\n\
                   \"C:\\d\\README\",1,5,,32\n\
                   \"C:\\d\\sub\",,,,16\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s.rebuild_name_order();
        s
    }

    fn sorted_with(
        s: &IndexStore,
        spec: &SortSpec,
        caches: &Caches,
        formulas: &[Option<Shared<Formula>>],
    ) -> Vec<String> {
        let mut ids: Vec<EntryId> =
            (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i) && s.parent(i).is_some()).collect();
        sort_ids_with(&mut ids, spec, s, caches, &CancelToken::default(), SortExt { props: None, formulas }).unwrap();
        ids.iter().map(|&i| s.name_string(i)).collect()
    }

    #[test]
    fn type_column_sorts_by_description() {
        let s = typed_store();
        let caches = Caches::new();
        caches.set_type_names(Some(crate::TypeNames::new(|e| {
            Some(
                match e {
                    "zip" => "Compressed (zipped) Folder",
                    "txt" | "log" => "Text Document",
                    "jpg" => "JPEG image",
                    _ => return None,
                }
                .to_string(),
            )
        })));
        let spec = SortSpec::by(SortColumn::Type, true, FoldersFirst::Ascending);
        // Folders first ("File folder"), then by description, ties by name.
        assert_eq!(
            sorted_with(&s, &spec, &caches, &[]),
            ["d", "sub", "b.zip", "README", "photo.jpg", "a.txt", "c.log"]
        );
        let by_ext = SortSpec::by(SortColumn::Extension, true, FoldersFirst::Ascending);
        assert_ne!(sorted_with(&s, &by_ext, &caches, &[]), sorted_with(&s, &spec, &caches, &[]));
        // Without folder grouping "File folder" sorts among the descriptions.
        let mixed = SortSpec::by(SortColumn::Type, true, FoldersFirst::Never);
        assert_eq!(
            sorted_with(&s, &mixed, &caches, &[]),
            ["b.zip", "README", "d", "sub", "photo.jpg", "a.txt", "c.log"]
        );
        // Descending reverses the folder grouping too (`FoldersFirst::Ascending`).
        let desc = SortSpec::by(SortColumn::Type, false, FoldersFirst::Ascending);
        assert_eq!(
            sorted_with(&s, &desc, &caches, &[]),
            ["c.log", "a.txt", "photo.jpg", "README", "b.zip", "sub", "d"]
        );
        // Fallback names without a provider ("JPG File", "LOG File", ...).
        let plain = Caches::new();
        assert_eq!(sorted_with(&s, &spec, &plain, &[]), ["d", "sub", "README", "photo.jpg", "c.log", "a.txt", "b.zip"]);
        // As a secondary key.
        let mut two = SortSpec::by(SortColumn::Size, false, FoldersFirst::Never);
        two.push_key(SortColumn::Type, true);
        assert_eq!(two.sort_keys().nth(1), Some((SortKey::Column(SortColumn::Type), true)));
        assert_eq!(SortColumn::parse("type"), Some(SortColumn::Type));
        assert_eq!(SortColumn::parse("extension"), Some(SortColumn::Extension));
    }

    #[test]
    fn custom_formula_columns() {
        let s = typed_store();
        let caches = Caches::new();
        let f = |src: &str| Some(Shared::new(Arc::new(Formula::parse(src).unwrap())));
        let formulas =
            vec![f("$size:"), f("IF($size:>10, $dm:, \"\")"), f("$stem:"), f("IF($isfolder:, 1/0, $size:)"), None];
        let never = FoldersFirst::Never;
        // Numbers numerically; folders have no size (empty) and sort last both ways.
        let asc = SortSpec::by_custom(0, true, never);
        assert_eq!(
            sorted_with(&s, &asc, &caches, &formulas),
            ["README", "a.txt", "photo.jpg", "b.zip", "c.log", "d", "sub"]
        );
        let desc = SortSpec::by_custom(0, false, never);
        assert_eq!(
            sorted_with(&s, &desc, &caches, &formulas),
            ["c.log", "b.zip", "photo.jpg", "a.txt", "README", "sub", "d"]
        );
        // Dates by time, "" is empty.
        let dates = SortSpec::by_custom(1, false, never);
        assert_eq!(sorted_with(&s, &dates, &caches, &formulas)[..2], ["c.log", "b.zip"]);
        // Text naturally and case-insensitively.
        let text = SortSpec::by_custom(2, true, never);
        assert_eq!(
            sorted_with(&s, &text, &caches, &formulas),
            ["a", "b", "c", "d", "photo", "README", "sub"].map(|n| {
                match n {
                    "a" => "a.txt",
                    "b" => "b.zip",
                    "c" => "c.log",
                    "photo" => "photo.jpg",
                    other => other,
                }
            })
        );
        // Errors (1/0 on folders) before empty, both after the values in either direction.
        let err = SortSpec::by_custom(3, false, never);
        let v = sorted_with(&s, &err, &caches, &formulas);
        assert_eq!(v[0], "c.log");
        assert_eq!(v[5..], ["sub", "d"]);
        // An unparsable / missing formula sorts everything as empty: name order.
        for i in [4, 9] {
            let spec = SortSpec::by_custom(i, true, never);
            assert_eq!(
                sorted_with(&s, &spec, &caches, &formulas),
                sorted_with(&s, &SortSpec::by(SortColumn::Name, true, never), &caches, &[])
            );
        }
        // Secondary custom key; folders grouped first; pushing it again flips it.
        let mut spec = SortSpec::by(SortColumn::Extension, true, FoldersFirst::Ascending);
        spec.push_custom_key(0, false);
        assert_eq!(
            spec.sort_keys().collect::<Vec<_>>(),
            [(SortKey::Column(SortColumn::Extension), true), (SortKey::Custom(0), false)]
        );
        spec.push_custom_key(0, false);
        assert_eq!(spec.sort_keys().nth(1), Some((SortKey::Custom(0), true)));
        spec.push_custom_key(2, true);
        spec.push_key(SortColumn::Size, true);
        assert_eq!(spec.sort_keys().count(), 3);
        assert_eq!(spec.custom, [None, Some(0), None], "replaced key clears its custom slot");
        let by_key = SortSpec::by_key(SortKey::Custom(1), true, never);
        assert_eq!(by_key, SortSpec::by_custom(1, true, never));
        let mut flip = by_key;
        flip.push_sort_key(SortKey::Custom(1), true);
        assert!(!flip.ascending, "pushing the primary key flips it");
    }
}

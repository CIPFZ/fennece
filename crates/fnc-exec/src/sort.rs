//! Result sorting (SPEC ch.3 §4.5).
//!
//! Name order uses the index's name fast-sort ranks where valid (a `u32` compare) and
//! falls back to the natural comparator for entries renamed since the ranks were built;
//! the mix is still a consistent total order because clean ranks agree with the
//! comparator. Other columns break ties by name.

use std::cmp::Ordering;

use fnc_core::EntryId;
use fnc_core::settings::FoldersFirst;
use fnc_index::IndexStore;
use fnc_props::{PropertyId, PropertyStore};
use rayon::prelude::*;

use crate::cache::{Caches, CancelToken, Cancelled};

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
            "ext" | "extension" | "type" => Self::Extension,
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
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            ascending: true,
            folders_first: FoldersFirst::Ascending,
            then: [None; 2],
            props: [None; 3],
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
}

impl SortSpec {
    /// Single-key sort.
    pub fn by(column: SortColumn, ascending: bool, folders_first: FoldersFirst) -> Self {
        Self { column, ascending, folders_first, then: [None; 2], props: [None; 3] }
    }

    /// Single-key sort by a property column.
    pub fn by_property(p: PropertyId, ascending: bool, folders_first: FoldersFirst) -> Self {
        let mut s = Self::by(SortColumn::Name, ascending, folders_first);
        s.props[0] = Some(p);
        s
    }

    /// All keys in priority order with their directions, property columns included.
    pub fn sort_keys(&self) -> impl Iterator<Item = (SortKey, bool)> + '_ {
        let cols = std::iter::once(Some((self.column, self.ascending))).chain(self.then.iter().copied());
        cols.zip(self.props.iter()).filter_map(|(c, p)| {
            let (col, asc) = c?;
            Some((p.map_or(SortKey::Column(col), SortKey::Property), asc))
        })
    }

    /// Adds (or flips, if present) a secondary property key; keeps at most three keys.
    pub fn push_property_key(&mut self, p: PropertyId, ascending: bool) {
        if self.props[0] == Some(p) {
            self.ascending = !self.ascending;
            return;
        }
        for (k, slot) in self.then.iter_mut().enumerate() {
            if let Some(key) = slot
                && self.props[k + 1] == Some(p)
            {
                key.1 = !key.1;
                return;
            }
        }
        let k = self.then.iter().position(Option::is_none).unwrap_or(1);
        self.then[k] = Some((SortColumn::Name, ascending));
        self.props[k + 1] = Some(p);
    }

    /// Adds (or flips, if present) a secondary key; keeps at most three keys.
    pub fn push_key(&mut self, column: SortColumn, ascending: bool) {
        if self.column == column && self.props[0].is_none() {
            self.ascending = !self.ascending;
            return;
        }
        for (k, slot) in self.then.iter_mut().enumerate() {
            if let Some(key) = slot
                && key.0 == column
                && self.props[k + 1].is_none()
            {
                key.1 = !key.1;
                return;
            }
        }
        let k = self.then.iter().position(Option::is_none).unwrap_or(1);
        self.then[k] = Some((column, ascending));
        self.props[k + 1] = None;
    }

    /// All keys in priority order with their directions.
    pub fn keys(&self) -> impl Iterator<Item = (SortColumn, bool)> + '_ {
        std::iter::once((self.column, self.ascending)).chain(self.then.iter().flatten().copied())
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

/// Sorts `ids` in place; property columns read `props`.
pub fn sort_ids_ext(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
    props: Option<&PropertyStore>,
) -> Result<(), Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
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
    let order = s.name_order();
    let by_name = |a: EntryId, b: EntryId| -> Ordering {
        if order.is_clean(a) && order.is_clean(b) { order.rank(a).cmp(&order.rank(b)) } else { s.cmp_names(a, b) }
    };
    let needs_dir_rank = spec.keys().any(|(c, _)| matches!(c, SortColumn::Path | SortColumn::FullPath));
    let dir_rank = needs_dir_rank.then(|| caches.dir_rank(s));
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
        }
    };
    let pkey = |k: usize, a: EntryId, b: EntryId| -> Option<Ordering> {
        let v = prop_keys.get(k)?.as_ref()?;
        Some(v.get(a.idx()).cmp(&v.get(b.idx())))
    };
    if !prop_keys.is_empty() {
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
}

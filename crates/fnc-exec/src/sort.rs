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
}

impl SortColumn {
    /// Parses `es.exe`/HTTP style names (`name`, `path`, `size`, `date-modified`, ...).
    pub fn parse(s: &str) -> Option<Self> {
        let n: String = s.chars().filter(|c| *c != '-' && *c != '_').collect::<String>().to_ascii_lowercase();
        Some(match n.as_str() {
            "name" | "filename" => Self::Name,
            "path" | "fullpath" | "pathandname" => Self::Path,
            "size" => Self::Size,
            "ext" | "extension" | "type" => Self::Extension,
            "dm" | "datemodified" => Self::DateModified,
            "dc" | "datecreated" => Self::DateCreated,
            "da" | "dateaccessed" => Self::DateAccessed,
            "attrib" | "attributes" => Self::Attributes,
            "runcount" => Self::RunCount,
            "dr" | "daterun" => Self::DateRun,
            "rc" | "daterecentlychanged" => Self::DateRecentlyChanged,
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
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { column: SortColumn::Name, ascending: true, folders_first: FoldersFirst::Ascending }
    }
}

/// Sorts `ids` in place.
pub fn sort_ids(
    ids: &mut [EntryId],
    spec: &SortSpec,
    s: &IndexStore,
    caches: &Caches,
    cancel: &CancelToken,
) -> Result<(), Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    let order = s.name_order();
    let by_name = |a: EntryId, b: EntryId| -> Ordering {
        if order.is_clean(a) && order.is_clean(b) { order.rank(a).cmp(&order.rank(b)) } else { s.cmp_names(a, b) }
    };
    let dir_rank = (spec.column == SortColumn::Path).then(|| caches.dir_rank(s));
    let size_key = |id: EntryId| if s.is_dir(id) { -1 } else { s.size(id).map_or(-1, |v| v as i128) };
    let key_cmp = |a: EntryId, b: EntryId| -> Ordering {
        let primary = match spec.column {
            SortColumn::Name => Ordering::Equal,
            SortColumn::Path => {
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
        };
        primary.then_with(|| by_name(a, b))
    };
    let group = match spec.folders_first {
        FoldersFirst::Always => true,
        FoldersFirst::Ascending => true,
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
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    Ok(())
}

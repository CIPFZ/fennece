//! Result rows: per-entry values for the requested columns, shared by the IPC backend, the
//! compatibility layer, exports and the HTTP server.

use fnc_core::{EntryFlags, EntryId, FileTime};
use fnc_exec::{Caches, Highlighter};
use fnc_index::{IndexStore, VolumeKind};

use crate::format::highlight_markup;
use crate::proto::{Column, FLAG_FOLDER, FLAG_ROOT, ResultColumns, Results, UNKNOWN};

/// A set of [`Column`]s.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ColumnSet(u32);

impl ColumnSet {
    /// Empty set.
    pub const EMPTY: ColumnSet = ColumnSet(0);

    fn bit(c: Column) -> u32 {
        1 << (c as u32)
    }

    /// Set of `cols`.
    pub fn of(cols: &[Column]) -> Self {
        let mut s = Self::EMPTY;
        for &c in cols {
            s.insert(c);
        }
        s
    }

    /// Adds `c`.
    pub fn insert(&mut self, c: Column) {
        self.0 |= Self::bit(c);
    }

    /// `true` if `c` is in the set.
    pub fn contains(self, c: Column) -> bool {
        self.0 & Self::bit(c) != 0
    }
}

/// Values of one result. Text fields are empty and numeric fields [`UNKNOWN`] when not
/// requested or not known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// [`FLAG_FOLDER`] / [`FLAG_ROOT`].
    pub flags: u8,
    /// Name.
    pub name: String,
    /// Parent path.
    pub path: String,
    /// Full path.
    pub full_path: String,
    /// Extension (empty for folders).
    pub extension: String,
    /// Type description.
    pub type_name: String,
    /// Size.
    pub size: u64,
    /// Date modified.
    pub date_modified: u64,
    /// Date created.
    pub date_created: u64,
    /// Date accessed.
    pub date_accessed: u64,
    /// Attributes (`u32::MAX` = unknown).
    pub attributes: u32,
    /// Run count.
    pub run_count: u32,
    /// Date run.
    pub date_run: u64,
    /// Date recently changed.
    pub date_recently_changed: u64,
    /// File list name.
    pub file_list_name: String,
    /// `*`-highlighted name.
    pub highlighted_name: Option<String>,
    /// `*`-highlighted parent path.
    pub highlighted_path: Option<String>,
    /// `*`-highlighted full path.
    pub highlighted_full_path: Option<String>,
}

impl Default for Row {
    fn default() -> Self {
        Self {
            flags: 0,
            name: String::new(),
            path: String::new(),
            full_path: String::new(),
            extension: String::new(),
            type_name: String::new(),
            size: UNKNOWN,
            date_modified: UNKNOWN,
            date_created: UNKNOWN,
            date_accessed: UNKNOWN,
            attributes: u32::MAX,
            run_count: 0,
            date_run: UNKNOWN,
            date_recently_changed: UNKNOWN,
            file_list_name: String::new(),
            highlighted_name: None,
            highlighted_path: None,
            highlighted_full_path: None,
        }
    }
}

impl Row {
    /// `true` for folders.
    pub fn is_folder(&self) -> bool {
        self.flags & FLAG_FOLDER != 0
    }
}

/// FILETIME as the wire value ([`UNKNOWN`] when unknown).
pub fn wire_time(t: FileTime) -> u64 {
    if t.is_known() && t.0 >= 0 { t.0 as u64 } else { UNKNOWN }
}

/// Extension of a displayed name (`""` without a dot or for a trailing dot).
pub fn extension_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(p) if p + 1 < name.len() => &name[p + 1..],
        _ => "",
    }
}

/// Builds the values of `id` needed for `cols` (plus highlighting when `hl` is given and a
/// text column is requested).
pub fn entry_row(s: &IndexStore, id: EntryId, cols: ColumnSet, caches: &Caches, hl: Option<&Highlighter>) -> Row {
    let dir = s.is_dir(id);
    let mut r = Row {
        flags: if dir { FLAG_FOLDER } else { 0 } | if s.flags(id).contains(EntryFlags::ROOT) { FLAG_ROOT } else { 0 },
        ..Row::default()
    };
    let need_name = cols.contains(Column::Name) || cols.contains(Column::Extension) || cols.contains(Column::FullPath);
    if need_name {
        s.name_into(id, &mut r.name);
    }
    if cols.contains(Column::Path) || cols.contains(Column::FullPath) {
        s.parent_path(id, &mut r.path);
    }
    if cols.contains(Column::FullPath) {
        s.full_path(id, &mut r.full_path);
    }
    if cols.contains(Column::Extension) && !dir {
        r.extension = extension_of(&r.name).to_string();
    }
    if cols.contains(Column::Type) {
        r.type_name = if dir {
            fnc_exec::types::FOLDER_TYPE.to_string()
        } else {
            caches.type_description(s.ext_dict().name(s.ext_id(id))).to_string()
        };
    }
    if cols.contains(Column::Size) && !dir {
        r.size = s.size(id).unwrap_or(UNKNOWN);
    }
    if cols.contains(Column::DateModified) {
        r.date_modified = wire_time(s.date_modified(id));
    }
    if cols.contains(Column::DateCreated) {
        r.date_created = wire_time(s.date_created(id));
    }
    if cols.contains(Column::DateAccessed) {
        r.date_accessed = wire_time(s.date_accessed(id));
    }
    if cols.contains(Column::Attributes) {
        let mut a = s.attributes(id);
        if dir {
            a |= fnc_core::flags::attr::DIRECTORY;
        }
        r.attributes = a;
    }
    if cols.contains(Column::RunCount) || cols.contains(Column::DateRun) {
        let run = s.run_info(id);
        r.run_count = run.count;
        // A never-run entry has the default (zero) date.
        r.date_run = if run.count == 0 && run.last.0 == 0 { UNKNOWN } else { wire_time(run.last) };
    }
    if cols.contains(Column::DateRecentlyChanged) {
        r.date_recently_changed = wire_time(s.date_recently_changed(id));
    }
    if cols.contains(Column::FileListName)
        && let Some(v) = s.volume_info(s.volume_of(id))
        && v.info.kind == VolumeKind::FileList
    {
        r.file_list_name = v.info.root_path.clone();
    }
    if let Some(h) = hl {
        let mut ranges = Vec::new();
        if cols.contains(Column::Name) {
            h.name_ranges(&r.name, &mut ranges);
            r.highlighted_name = Some(highlight_markup(&r.name, &ranges));
        }
        if cols.contains(Column::Path) {
            h.path_ranges(&r.path, &mut ranges);
            r.highlighted_path = Some(highlight_markup(&r.path, &ranges));
        }
        if cols.contains(Column::FullPath) {
            h.path_ranges(&r.full_path, &mut ranges);
            let mut name_ranges = Vec::new();
            h.name_ranges(&r.name, &mut name_ranges);
            let off = r.full_path.len().saturating_sub(r.name.len());
            if r.full_path.ends_with(r.name.as_str()) {
                ranges.extend(name_ranges.into_iter().map(|x| x.start + off..x.end + off));
            }
            ranges.sort_by_key(|x| (x.start, x.end));
            let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
            for x in ranges.drain(..) {
                match merged.last_mut() {
                    Some(l) if x.start <= l.end => l.end = l.end.max(x.end),
                    _ => merged.push(x),
                }
            }
            r.highlighted_full_path = Some(highlight_markup(&r.full_path, &merged));
        }
    }
    r
}

impl ResultColumns {
    /// Empty columns with a vector for every column in `cols` (and highlighted vectors
    /// when `highlight`).
    pub fn with_columns(cols: ColumnSet, highlight: bool, capacity: usize) -> Self {
        fn v<T>(cols: ColumnSet, c: Column, cap: usize) -> Option<Vec<T>> {
            cols.contains(c).then(|| Vec::with_capacity(cap))
        }
        let hv = |c: Column| (highlight && cols.contains(c)).then(|| Vec::with_capacity(capacity));
        Self {
            flags: Vec::with_capacity(capacity),
            name: v(cols, Column::Name, capacity),
            path: v(cols, Column::Path, capacity),
            full_path: v(cols, Column::FullPath, capacity),
            extension: v(cols, Column::Extension, capacity),
            type_name: v(cols, Column::Type, capacity),
            size: v(cols, Column::Size, capacity),
            date_modified: v(cols, Column::DateModified, capacity),
            date_created: v(cols, Column::DateCreated, capacity),
            date_accessed: v(cols, Column::DateAccessed, capacity),
            attributes: v(cols, Column::Attributes, capacity),
            run_count: v(cols, Column::RunCount, capacity),
            date_run: v(cols, Column::DateRun, capacity),
            date_recently_changed: v(cols, Column::DateRecentlyChanged, capacity),
            file_list_name: v(cols, Column::FileListName, capacity),
            highlighted_name: hv(Column::Name),
            highlighted_path: hv(Column::Path),
            highlighted_full_path: hv(Column::FullPath),
        }
    }

    /// Appends a row (fields of absent columns are dropped).
    pub fn push(&mut self, r: Row) {
        fn put<T>(v: &mut Option<Vec<T>>, x: T) {
            if let Some(v) = v {
                v.push(x);
            }
        }
        self.flags.push(r.flags);
        put(&mut self.size, r.size);
        put(&mut self.date_modified, r.date_modified);
        put(&mut self.date_created, r.date_created);
        put(&mut self.date_accessed, r.date_accessed);
        put(&mut self.attributes, r.attributes);
        put(&mut self.run_count, r.run_count);
        put(&mut self.date_run, r.date_run);
        put(&mut self.date_recently_changed, r.date_recently_changed);
        put(&mut self.extension, r.extension);
        put(&mut self.type_name, r.type_name);
        put(&mut self.file_list_name, r.file_list_name);
        put(&mut self.highlighted_name, r.highlighted_name.unwrap_or_default());
        put(&mut self.highlighted_path, r.highlighted_path.unwrap_or_default());
        put(&mut self.highlighted_full_path, r.highlighted_full_path.unwrap_or_default());
        put(&mut self.name, r.name);
        put(&mut self.path, r.path);
        put(&mut self.full_path, r.full_path);
    }

    /// Columns present in this page.
    pub fn present(&self) -> ColumnSet {
        let mut s = ColumnSet::EMPTY;
        let mut add = |c: Column, p: bool| {
            if p {
                s.insert(c);
            }
        };
        add(Column::Name, self.name.is_some());
        add(Column::Path, self.path.is_some());
        add(Column::FullPath, self.full_path.is_some());
        add(Column::Extension, self.extension.is_some());
        add(Column::Type, self.type_name.is_some());
        add(Column::Size, self.size.is_some());
        add(Column::DateModified, self.date_modified.is_some());
        add(Column::DateCreated, self.date_created.is_some());
        add(Column::DateAccessed, self.date_accessed.is_some());
        add(Column::Attributes, self.attributes.is_some());
        add(Column::RunCount, self.run_count.is_some());
        add(Column::DateRun, self.date_run.is_some());
        add(Column::DateRecentlyChanged, self.date_recently_changed.is_some());
        add(Column::FileListName, self.file_list_name.is_some());
        s
    }

    /// Row `i` (absent columns default; a missing full path is rebuilt from path + name).
    pub fn row(&self, i: usize) -> Row {
        fn s(v: &Option<Vec<String>>, i: usize) -> String {
            v.as_ref().and_then(|v| v.get(i)).cloned().unwrap_or_default()
        }
        fn n<T: Copy>(v: &Option<Vec<T>>, i: usize, d: T) -> T {
            v.as_ref().and_then(|v| v.get(i)).copied().unwrap_or(d)
        }
        fn h(v: &Option<Vec<String>>, i: usize) -> Option<String> {
            v.as_ref().and_then(|v| v.get(i)).cloned()
        }
        let flags = self.flags.get(i).copied().unwrap_or(0);
        let attributes = n(&self.attributes, i, u32::MAX);
        let mut r = Row {
            flags,
            name: s(&self.name, i),
            path: s(&self.path, i),
            full_path: s(&self.full_path, i),
            extension: s(&self.extension, i),
            type_name: s(&self.type_name, i),
            size: n(&self.size, i, UNKNOWN),
            date_modified: n(&self.date_modified, i, UNKNOWN),
            date_created: n(&self.date_created, i, UNKNOWN),
            date_accessed: n(&self.date_accessed, i, UNKNOWN),
            attributes,
            run_count: n(&self.run_count, i, 0),
            date_run: n(&self.date_run, i, UNKNOWN),
            date_recently_changed: n(&self.date_recently_changed, i, UNKNOWN),
            file_list_name: s(&self.file_list_name, i),
            highlighted_name: h(&self.highlighted_name, i),
            highlighted_path: h(&self.highlighted_path, i),
            highlighted_full_path: h(&self.highlighted_full_path, i),
        };
        if self.full_path.is_none() && self.name.is_some() && self.path.is_some() {
            r.full_path = join_path(&r.path, &r.name);
        }
        r
    }
}

impl Results {
    /// Row `i` of this page.
    pub fn row(&self, i: usize) -> Row {
        self.columns.row(i)
    }
}

/// Joins a parent path and a name with the separator the path uses.
pub fn join_path(path: &str, name: &str) -> String {
    if path.is_empty() {
        return name.to_string();
    }
    let sep = if path.starts_with('/') { '/' } else { '\\' };
    if path.ends_with(sep) { format!("{path}{name}") } else { format!("{path}{sep}{name}") }
}

/// Resolves a full path (either separator, any case, optional trailing separator) to a
/// live entry of `s`. Never touches the file system.
pub fn resolve_path(s: &IndexStore, path: &str) -> Option<EntryId> {
    let is_sep = |c: char| c == '\\' || c == '/';
    let trimmed = path.trim_end_matches(is_sep);
    let path = if trimmed.is_empty() { path } else { trimmed };
    let fold = |t: &str| {
        let units: Vec<u16> = t.encode_utf16().collect();
        let mut out = Vec::new();
        fnc_core::fold::fold_utf16_into(&units, &mut out);
        out
    };
    let mut best: Option<(usize, EntryId)> = None;
    let mut buf = String::new();
    for v in s.volumes() {
        for &root in &v.roots {
            if !s.is_live(root) {
                continue;
            }
            s.full_path(root, &mut buf);
            let rp = buf.trim_end_matches(is_sep);
            let rp = if rp.is_empty() { buf.as_str() } else { rp };
            let Some(head) = path.get(..rp.len()) else { continue };
            let boundary = path[rp.len()..].chars().next().is_none_or(is_sep) || rp.ends_with(is_sep);
            let same =
                head.chars().zip(rp.chars()).all(|(a, b)| a == b || (is_sep(a) && is_sep(b))) || fold(head) == fold(rp);
            if same && boundary && best.is_none_or(|(l, _)| rp.len() > l) {
                best = Some((rp.len(), root));
            }
        }
    }
    let (len, mut cur) = best?;
    for comp in path[len..].split(is_sep).filter(|c| !c.is_empty()) {
        cur = s.find_child(cur, &fold(comp))?;
    }
    s.is_live(cur).then_some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> IndexStore {
        let efu = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Docs\",,,,16\n\
                   \"C:\\Docs\\Report.PDF\",1234,133000000000000000,,32\n\
                   \"C:\\Docs\\a*b.txt\",5,,,33\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s.rebuild_name_order();
        s
    }

    #[test]
    fn resolves_paths_in_the_index_only() {
        let s = store();
        let id = resolve_path(&s, "c:\\docs\\report.pdf").unwrap();
        assert_eq!(s.name_string(id), "Report.PDF");
        assert_eq!(resolve_path(&s, "C:/Docs/Report.PDF"), Some(id));
        assert!(resolve_path(&s, "C:\\Docs\\").is_some());
        assert!(resolve_path(&s, "C:").is_some());
        assert_eq!(resolve_path(&s, "C:\\Docs\\..\\Docs\\Report.PDF"), None);
        assert_eq!(resolve_path(&s, "C:\\Docs\\missing"), None);
        assert_eq!(resolve_path(&s, "D:\\Docs"), None);
        assert_eq!(resolve_path(&s, "C:\\DocsX"), None);
    }

    #[test]
    fn rows_and_columns() {
        let s = store();
        let caches = Caches::new();
        let id = resolve_path(&s, "C:\\Docs\\Report.PDF").unwrap();
        let cols = ColumnSet::of(&Column::ALL);
        let r = entry_row(&s, id, cols, &caches, None);
        assert_eq!(r.name, "Report.PDF");
        assert_eq!(r.path, "C:\\Docs");
        assert_eq!(r.full_path, "C:\\Docs\\Report.PDF");
        assert_eq!(r.extension, "PDF");
        assert_eq!(r.type_name, "PDF File");
        assert_eq!(r.size, 1234);
        assert_eq!(r.date_modified, 133000000000000000);
        assert_eq!(r.date_created, UNKNOWN);
        assert_eq!(r.attributes, 32);
        assert_eq!(r.file_list_name, "t.efu");
        let d = entry_row(&s, resolve_path(&s, "C:\\Docs").unwrap(), cols, &caches, None);
        assert!(d.is_folder());
        assert_eq!(d.size, UNKNOWN);
        assert_eq!(d.extension, "");
        assert_eq!(d.type_name, "File folder");
        assert_eq!(d.attributes & 16, 16);
        let mut rc = ResultColumns::with_columns(ColumnSet::of(&[Column::Name, Column::Path]), false, 2);
        rc.push(r.clone());
        assert_eq!(rc.row(0).full_path, "C:\\Docs\\Report.PDF");
        assert_eq!(rc.present(), ColumnSet::of(&[Column::Name, Column::Path]));
    }

    #[test]
    fn highlighting_rows() {
        let s = store();
        let caches = Caches::new();
        let p = fnc_query::plan(
            &fnc_query::parse("rep", &fnc_query::ParseEnv::default()).ast,
            &fnc_query::PlanEnv::default(),
            None,
        );
        let h = Highlighter::new(&p);
        let id = resolve_path(&s, "C:\\Docs\\Report.PDF").unwrap();
        let r = entry_row(&s, id, ColumnSet::of(&[Column::Name, Column::FullPath]), &caches, Some(&h));
        assert_eq!(r.highlighted_name.as_deref(), Some("*Rep*ort.PDF"));
        assert_eq!(r.highlighted_full_path.as_deref(), Some("C:\\Docs\\*Rep*ort.PDF"));
        let star = resolve_path(&s, "C:\\Docs\\a*b.txt").unwrap();
        let r = entry_row(&s, star, ColumnSet::of(&[Column::Name]), &caches, Some(&h));
        assert_eq!(r.highlighted_name.as_deref(), Some("a**b.txt"));
    }
}

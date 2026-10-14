//! Result list columns (SPEC ch.3 §4.3): ids, titles, cell text, sort mapping and the
//! `columns` setting (`id:width;...`). Property columns use the property's canonical
//! name as id (`width:90`), sort through `SortSpec::props` and read the property store;
//! the duplicate group column shows `ResultSet::dupe_group`, the relevance column the
//! semantic similarity, and custom columns 1–9 evaluate the formulas of the
//! `custom_columns` setting (`Title=formula`, the `eval:` language).

use fnc_core::settings::{DateFormat, SizeFormat};
use fnc_core::{EntryFlags, EntryId, FileTime};
use fnc_exec::{ResultSet, Shared, SortColumn, SortKey};
use fnc_index::IndexSnapshot;
use fnc_props::filecache::FileField;
use fnc_props::{PropType, PropertyId, PropertyStore};

use crate::cells::{LazyCol, LazyVal};
use crate::format;
use crate::i18n::t;

/// A result list column.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColumnId {
    /// File name.
    Name,
    /// Folder containing the entry.
    Path,
    /// Size.
    Size,
    /// Extension.
    Extension,
    /// Type description.
    Type,
    /// Date modified.
    DateModified,
    /// Date created.
    DateCreated,
    /// Date accessed.
    DateAccessed,
    /// Attribute letters.
    Attributes,
    /// Times opened through Fennec.
    RunCount,
    /// Last opened through Fennec.
    DateRun,
    /// Last changed while indexed (index journal).
    DateRecentlyChanged,
    /// Full path.
    FullPath,
    /// File list the entry comes from.
    FileListFilename,
    /// Index source (`NTFS`, `Folder`, `File list`).
    IndexSource,
    /// Volume root.
    Volume,
    /// Number of parent folders.
    Depth,
    /// Name length.
    NameLength,
    /// File reference number.
    Frn,
    /// Duplicate group number of `dupe:` searches.
    DupeGroup,
    /// A property (`width`, `artist`, …).
    Property(PropertyId),
    /// Semantic relevance (0–100) of searches with semantic terms.
    Relevance,
    /// Target of shortcuts, internet shortcuts, symbolic links and junctions (read).
    LinkTarget,
    /// File signature (read).
    FileSignature,
    /// Original location of recycle bin items (read).
    RecycleOrigin,
    /// Custom column `n` (0-based) of the `custom_columns` setting.
    Custom(u8),
}

/// Custom columns supported.
pub const MAX_CUSTOM: usize = 9;

const CUSTOM_KEYS: [&str; MAX_CUSTOM] =
    ["custom1", "custom2", "custom3", "custom4", "custom5", "custom6", "custom7", "custom8", "custom9"];

/// Title and parsed formula of one custom column.
type CustomColumn = (String, Option<std::sync::Arc<fnc_query::formula::Formula>>);

// The UI thread formats every cell; tests run one app per thread.
thread_local! {
    static CUSTOM: std::cell::RefCell<Vec<CustomColumn>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Installs the `custom_columns` setting (`Title=formula`, or just a formula).
pub fn set_custom_columns(spec: &[String]) {
    let parsed: Vec<CustomColumn> = spec
        .iter()
        .take(MAX_CUSTOM)
        .enumerate()
        .map(|(i, s)| {
            let (title, formula) = match s.split_once('=') {
                // `=` also starts formulas and is an operator: a title has no `$`, `(` or quote.
                Some((a, b)) if !a.is_empty() && !a.contains(['$', '(', '"', '<', '>', '!']) => {
                    (a.trim().to_string(), b)
                }
                _ => (String::new(), s.as_str()),
            };
            let title =
                if title.is_empty() { crate::i18n::t_args("col-custom", &[("n", (i + 1).into())]) } else { title };
            (title, fnc_query::formula::Formula::parse(formula).ok().map(std::sync::Arc::new))
        })
        .collect();
    CUSTOM.with(|c| *c.borrow_mut() = parsed);
}

fn custom(i: u8) -> Option<CustomColumn> {
    CUSTOM.with(|c| c.borrow().get(usize::from(i)).cloned())
}

/// The custom columns' formulas for `SearchOptions::formulas` (the same `Arc`s until
/// [`set_custom_columns`] runs again, so options compare equal).
pub fn custom_formulas() -> Vec<Option<Shared<fnc_query::formula::Formula>>> {
    CUSTOM.with(|c| c.borrow().iter().map(|(_, f)| f.clone().map(Shared::new)).collect())
}

impl ColumnId {
    /// Every built-in column, in the column chooser's order (property columns are listed
    /// separately, see [`ColumnId::properties`]).
    pub const ALL: [ColumnId; 24] = [
        ColumnId::Name,
        ColumnId::Path,
        ColumnId::Size,
        ColumnId::Extension,
        ColumnId::Type,
        ColumnId::DateModified,
        ColumnId::DateCreated,
        ColumnId::DateAccessed,
        ColumnId::Attributes,
        ColumnId::RunCount,
        ColumnId::DateRun,
        ColumnId::DateRecentlyChanged,
        ColumnId::FullPath,
        ColumnId::FileListFilename,
        ColumnId::IndexSource,
        ColumnId::Volume,
        ColumnId::Depth,
        ColumnId::NameLength,
        ColumnId::Frn,
        ColumnId::DupeGroup,
        ColumnId::Relevance,
        ColumnId::LinkTarget,
        ColumnId::FileSignature,
        ColumnId::RecycleOrigin,
    ];

    /// The custom columns defined in the settings.
    pub fn customs() -> Vec<ColumnId> {
        let n = CUSTOM.with(|c| c.borrow().len());
        (0..n as u8).map(ColumnId::Custom).collect()
    }

    /// Every property column, sorted by localised title.
    pub fn properties() -> Vec<ColumnId> {
        let mut v: Vec<(String, ColumnId)> =
            PropertyId::all().map(|p| (ColumnId::Property(p).title(), ColumnId::Property(p))).collect();
        v.sort_by_key(|a| a.0.to_lowercase());
        v.into_iter().map(|(_, c)| c).collect()
    }

    /// Key used in the `columns` setting.
    pub fn key(self) -> &'static str {
        match self {
            ColumnId::Name => "name",
            ColumnId::Path => "path",
            ColumnId::Size => "size",
            ColumnId::Extension => "ext",
            ColumnId::Type => "type",
            ColumnId::DateModified => "dm",
            ColumnId::DateCreated => "dc",
            ColumnId::DateAccessed => "da",
            ColumnId::Attributes => "attributes",
            ColumnId::RunCount => "runcount",
            ColumnId::DateRun => "dr",
            ColumnId::DateRecentlyChanged => "rc",
            ColumnId::FullPath => "fullpath",
            ColumnId::FileListFilename => "filelist",
            ColumnId::IndexSource => "indexsource",
            ColumnId::Volume => "volume",
            ColumnId::Depth => "depth",
            ColumnId::NameLength => "namelength",
            ColumnId::Frn => "frn",
            ColumnId::DupeGroup => "dupegroup",
            ColumnId::Relevance => "relevance",
            ColumnId::LinkTarget => "linktarget",
            ColumnId::FileSignature => "filesignature",
            ColumnId::RecycleOrigin => "recyclebinoriginalpath",
            ColumnId::Custom(i) => CUSTOM_KEYS.get(usize::from(i)).copied().unwrap_or("custom1"),
            ColumnId::Property(p) => p.name(),
        }
    }

    /// Parses a setting key (built-in id or property name).
    pub fn from_key(k: &str) -> Option<Self> {
        let k = k.trim();
        if let Some(i) = CUSTOM_KEYS.iter().position(|c| *c == k) {
            return Some(ColumnId::Custom(i as u8));
        }
        Self::ALL.iter().copied().find(|c| c.key() == k).or_else(|| PropertyId::lookup(k).map(ColumnId::Property))
    }

    /// Localised header title.
    pub fn title(self) -> String {
        match self {
            ColumnId::Property(p) => {
                let key = format!("prop-{}", p.name());
                let s = t(&key);
                if s == key { p.title().to_string() } else { s }
            }
            ColumnId::Custom(i) => {
                custom(i).map(|c| c.0).unwrap_or_else(|| crate::i18n::t_args("col-custom", &[("n", (i + 1).into())]))
            }
            c => t(&format!("col-{}", c.key())),
        }
    }

    /// Default width in points.
    pub fn default_width(self) -> f32 {
        match self {
            ColumnId::Name => 300.0,
            ColumnId::Path | ColumnId::FullPath => 420.0,
            ColumnId::Type => 160.0,
            ColumnId::DateModified | ColumnId::DateCreated | ColumnId::DateAccessed | ColumnId::DateRun => 140.0,
            ColumnId::DateRecentlyChanged => 140.0,
            ColumnId::FileListFilename => 200.0,
            ColumnId::LinkTarget | ColumnId::RecycleOrigin => 300.0,
            ColumnId::Relevance => 80.0,
            ColumnId::Custom(_) => 140.0,
            ColumnId::Property(p) => match p.ty() {
                PropType::Text | PropType::MultiText => 160.0,
                PropType::Date => 140.0,
                _ => 90.0,
            },
            _ => 90.0,
        }
    }

    /// Right-aligned (numbers).
    pub fn numeric(self) -> bool {
        match self {
            ColumnId::Property(p) => matches!(p.ty(), PropType::Int | PropType::Float | PropType::Duration),
            c => matches!(
                c,
                ColumnId::Size
                    | ColumnId::RunCount
                    | ColumnId::Depth
                    | ColumnId::NameLength
                    | ColumnId::Frn
                    | ColumnId::DupeGroup
                    | ColumnId::Relevance
            ),
        }
    }

    /// Sort key of this column (`None`: not sortable). Custom columns sort by their
    /// formula's value (computed by the engine), `Type` by the type description, the
    /// file-reading columns by the values cached in `files.fnx` (the window reads the
    /// missing ones first, after asking).
    pub fn sort(self) -> Option<SortKey> {
        match self {
            ColumnId::Property(p) => Some(SortKey::Property(p)),
            ColumnId::Custom(i) => Some(SortKey::Custom(i)),
            ColumnId::LinkTarget => Some(SortKey::File(FileField::LinkTarget)),
            ColumnId::FileSignature => Some(SortKey::File(FileField::Signature)),
            ColumnId::RecycleOrigin => Some(SortKey::File(FileField::RecycleOrigin)),
            // Only known for the results of a `dupe:` search.
            ColumnId::DupeGroup => None,
            c => Some(SortKey::Column(c.sort_column())),
        }
    }

    fn sort_column(self) -> SortColumn {
        match self {
            ColumnId::Name
            | ColumnId::DupeGroup
            | ColumnId::Property(_)
            | ColumnId::Custom(_)
            | ColumnId::LinkTarget
            | ColumnId::FileSignature
            | ColumnId::RecycleOrigin => SortColumn::Name,
            ColumnId::Relevance => SortColumn::Relevance,
            ColumnId::Path => SortColumn::Path,
            ColumnId::Size => SortColumn::Size,
            ColumnId::Extension => SortColumn::Extension,
            ColumnId::Type => SortColumn::Type,
            ColumnId::DateModified => SortColumn::DateModified,
            ColumnId::DateCreated => SortColumn::DateCreated,
            ColumnId::DateAccessed => SortColumn::DateAccessed,
            ColumnId::Attributes => SortColumn::Attributes,
            ColumnId::RunCount => SortColumn::RunCount,
            ColumnId::DateRun => SortColumn::DateRun,
            ColumnId::DateRecentlyChanged => SortColumn::DateRecentlyChanged,
            ColumnId::FullPath => SortColumn::FullPath,
            ColumnId::FileListFilename | ColumnId::Volume => SortColumn::Volume,
            ColumnId::IndexSource => SortColumn::IndexSource,
            ColumnId::Depth => SortColumn::Depth,
            ColumnId::NameLength => SortColumn::NameLength,
            ColumnId::Frn => SortColumn::Frn,
        }
    }

    /// Whether sorting starts descending (sizes, dates, counts: largest/newest first).
    pub fn sort_descending_first(self) -> bool {
        if let ColumnId::Property(p) = self {
            return !matches!(p.ty(), PropType::Text | PropType::MultiText);
        }
        !matches!(
            self,
            ColumnId::Name
                | ColumnId::Path
                | ColumnId::FullPath
                | ColumnId::Extension
                | ColumnId::Type
                | ColumnId::FileListFilename
                | ColumnId::IndexSource
                | ColumnId::Volume
                | ColumnId::Custom(_)
                | ColumnId::LinkTarget
                | ColumnId::FileSignature
                | ColumnId::RecycleOrigin
        )
    }
}

/// A visible column.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ColumnState {
    /// Which column.
    pub id: ColumnId,
    /// Width in points.
    pub width: f32,
}

/// Property columns whose localised title or any search name contains `filter`
/// (case-insensitive; dashes, underscores and spaces ignored in names); all for an empty filter.
pub fn filter_properties(cols: Vec<ColumnId>, filter: &str) -> Vec<ColumnId> {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return cols;
    }
    let compact = fnc_props::schema::normalize(&needle.replace(' ', ""));
    cols.into_iter()
        .filter(|c| match c {
            ColumnId::Property(p) => {
                c.title().to_lowercase().contains(&needle)
                    || p.def().is_some_and(|d| {
                        std::iter::once(&d.canonical)
                            .chain(d.aliases)
                            .any(|n| !compact.is_empty() && fnc_props::schema::normalize(n).contains(&compact))
                    })
            }
            _ => false,
        })
        .collect()
}

/// Parses the `columns` setting; unknown entries are skipped, `name` is always kept.
pub fn parse_columns(spec: &[String]) -> Vec<ColumnState> {
    let mut out: Vec<ColumnState> = Vec::new();
    for item in spec {
        let (key, width) = item.split_once(':').unwrap_or((item, ""));
        let Some(id) = ColumnId::from_key(key) else { continue };
        if out.iter().any(|c| c.id == id) {
            continue;
        }
        let width = width.trim().parse::<f32>().ok().filter(|w| (20.0..4000.0).contains(w));
        out.push(ColumnState { id, width: width.unwrap_or_else(|| id.default_width()) });
    }
    if !out.iter().any(|c| c.id == ColumnId::Name) {
        out.insert(0, ColumnState { id: ColumnId::Name, width: ColumnId::Name.default_width() });
    }
    out
}

/// Formats columns back into the setting.
pub fn columns_setting(cols: &[ColumnState]) -> Vec<String> {
    cols.iter().map(|c| format!("{}:{}", c.id.key(), c.width.round() as u32)).collect()
}

/// Parses the `sort` setting (`size-descending;width-ascending`).
pub fn parse_sort(s: &str) -> Vec<(SortKey, bool)> {
    s.split(';')
        .filter_map(|k| {
            let k = k.trim();
            let (col, dir) = k.rsplit_once('-').map_or((k, "ascending"), |(c, d)| (c, d));
            let (col, asc) = match dir {
                "ascending" | "asc" => (col, true),
                "descending" | "desc" => (col, false),
                _ => (k, true),
            };
            SortColumn::parse(col)
                .map(SortKey::Column)
                .or_else(|| CUSTOM_KEYS.iter().position(|c| *c == col).map(|i| SortKey::Custom(i as u8)))
                .or_else(|| FileField::lookup(col).map(SortKey::File))
                .or_else(|| PropertyId::lookup(col).map(SortKey::Property))
                .map(|c| (c, asc))
        })
        .take(3)
        .collect()
}

/// Sort spec of a `sort` setting text (`None` if it names no known key).
pub fn sort_spec(s: &str, folders_first: fnc_core::settings::FoldersFirst) -> Option<fnc_exec::SortSpec> {
    let keys = parse_sort(s);
    let (&(first, asc), rest) = keys.split_first()?;
    let mut spec = fnc_exec::SortSpec::by_key(first, asc, folders_first);
    for &(k, asc) in rest {
        spec.push_sort_key(k, asc);
    }
    Some(spec)
}

/// Formats a sort spec back into the setting.
pub fn sort_setting(spec: &fnc_exec::SortSpec) -> String {
    spec.sort_keys()
        .map(|(k, asc)| {
            let name = match k {
                SortKey::Column(c) => sort_key(c),
                SortKey::Property(p) => p.name(),
                SortKey::Custom(i) => ColumnId::Custom(i).key(),
                SortKey::File(f) => f.name(),
            };
            format!("{name}-{}", if asc { "ascending" } else { "descending" })
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn sort_key(c: SortColumn) -> &'static str {
    match c {
        SortColumn::Name => "name",
        SortColumn::Path => "path",
        SortColumn::Size => "size",
        SortColumn::Extension => "extension",
        SortColumn::Type => "type",
        SortColumn::DateModified => "date-modified",
        SortColumn::DateCreated => "date-created",
        SortColumn::DateAccessed => "date-accessed",
        SortColumn::Attributes => "attributes",
        SortColumn::RunCount => "run-count",
        SortColumn::DateRun => "date-run",
        SortColumn::DateRecentlyChanged => "date-recently-changed",
        SortColumn::FullPath => "full-path",
        SortColumn::Volume => "volume",
        SortColumn::IndexSource => "index-source",
        SortColumn::Depth => "depth",
        SortColumn::NameLength => "name-length",
        SortColumn::Frn => "frn",
        SortColumn::Relevance => "relevance",
    }
}

/// Attribute letters in Everything's order (`RHSDA...`).
pub fn attribute_letters(attr: u32) -> String {
    use fnc_core::flags::attr as a;
    const LETTERS: &[(u32, char)] = &[
        (a::READONLY, 'R'),
        (a::HIDDEN, 'H'),
        (a::SYSTEM, 'S'),
        (a::DIRECTORY, 'D'),
        (a::ARCHIVE, 'A'),
        (a::TEMPORARY, 'T'),
        (a::SPARSE_FILE, 'P'),
        (a::REPARSE_POINT, 'L'),
        (a::COMPRESSED, 'C'),
        (a::OFFLINE, 'O'),
        (a::NOT_CONTENT_INDEXED, 'I'),
        (a::ENCRYPTED, 'E'),
    ];
    LETTERS.iter().filter(|(bit, _)| attr & bit != 0).map(|(_, c)| *c).collect()
}

/// Cell formatting context.
pub struct CellFormat<'a> {
    /// Date style.
    pub date: DateFormat,
    /// Size style.
    pub size: SizeFormat,
    /// Type names by extension (`""` = no extension), filled lazily.
    pub type_name: &'a mut dyn FnMut(&str, bool) -> String,
    /// "Now" for relative dates.
    pub now: FileTime,
    /// Property store (property columns; `None` = cells empty).
    pub props: Option<&'a PropertyStore>,
    /// Results (duplicate groups).
    pub results: Option<&'a ResultSet>,
    /// Loader of cells that read files (`None`: such cells stay empty).
    pub lazy: Option<&'a crate::cells::LazyCells>,
}

/// Text of `col` for entry `id` (`name`/`path`-like columns are produced by the caller
/// with highlighting).
pub fn cell(s: &IndexSnapshot, id: EntryId, col: ColumnId, f: &mut CellFormat<'_>) -> String {
    let date = |t: FileTime| format::date_with(t, f.date, f.now);
    match col {
        ColumnId::Name => s.name_string(id),
        ColumnId::Path => {
            let mut p = String::new();
            s.parent_path(id, &mut p);
            p
        }
        ColumnId::FullPath => {
            let mut p = String::new();
            s.full_path(id, &mut p);
            p
        }
        ColumnId::Size => {
            if s.is_dir(id) {
                String::new()
            } else {
                s.size(id).map(|v| format::size_with(v, f.size)).unwrap_or_default()
            }
        }
        ColumnId::Extension => {
            if s.is_dir(id) {
                String::new()
            } else {
                String::from_utf8_lossy(s.ext_dict().name(s.ext_id(id))).into_owned()
            }
        }
        ColumnId::Type => {
            let ext = if s.is_dir(id) {
                String::new()
            } else {
                String::from_utf8_lossy(s.ext_dict().name(s.ext_id(id))).into_owned()
            };
            (f.type_name)(&ext, s.is_dir(id))
        }
        ColumnId::DateModified => date(s.date_modified(id)),
        ColumnId::DateCreated => date(s.date_created(id)),
        ColumnId::DateAccessed => date(s.date_accessed(id)),
        ColumnId::Attributes => attribute_letters(s.attributes(id)),
        ColumnId::RunCount => {
            let n = s.run_info(id).count;
            if n == 0 { String::new() } else { n.to_string() }
        }
        ColumnId::DateRun => date(s.run_info(id).last),
        ColumnId::DateRecentlyChanged => date(s.date_recently_changed(id)),
        ColumnId::FileListFilename => s
            .volume_info(s.volume_of(id))
            .filter(|v| v.info.kind == fnc_index::VolumeKind::FileList)
            .map(|v| v.info.root_path.clone())
            .unwrap_or_default(),
        ColumnId::IndexSource => s
            .volume_info(s.volume_of(id))
            .map(|v| match v.info.kind {
                fnc_index::VolumeKind::Ntfs => "NTFS".to_string(),
                fnc_index::VolumeKind::Refs => "ReFS".to_string(),
                fnc_index::VolumeKind::Folder => t("source-folder"),
                fnc_index::VolumeKind::FileList => t("source-filelist"),
            })
            .unwrap_or_default(),
        ColumnId::Volume => s.volume_info(s.volume_of(id)).map(|v| v.info.root_path.clone()).unwrap_or_default(),
        ColumnId::Depth => s.depth(id).to_string(),
        ColumnId::NameLength => s.name(id).len().to_string(),
        ColumnId::Frn => {
            if s.flags(id).contains(EntryFlags::ROOT) {
                String::new()
            } else {
                format!("{:#x}", s.frn(id))
            }
        }
        ColumnId::DupeGroup => f.results.and_then(|r| r.dupe_group(id)).map(|g| g.to_string()).unwrap_or_default(),
        ColumnId::Relevance => f
            .results
            .and_then(|r| r.relevance(id))
            .map(|v| format!("{:.0}", (v * 100.0).clamp(0.0, 100.0)))
            .unwrap_or_default(),
        ColumnId::Custom(i) => match custom(i).and_then(|c| c.1) {
            Some(formula) => {
                let props = f.props.filter(|st| st.instance() == s.instance());
                match formula.eval(&fnc_exec::formula::StoreVars { s, id, props }) {
                    fnc_query::formula::Val::Date(t) => date(FileTime(t)),
                    v => v.to_string(),
                }
            }
            None => String::new(),
        },
        ColumnId::Property(p) => {
            let fmt = |v: fnc_props::Value| match (p.ty(), v) {
                (PropType::Date, fnc_props::Value::Date(t)) => Some(date(t)),
                (PropType::Date, _) => None,
                (_, v) => Some(format_property(p, &v)),
            };
            match f.props.filter(|st| st.instance() == s.instance()) {
                // The property index is authoritative for what it has extracted.
                Some(st) if st.covers(id, p) => st.get(id, p).and_then(fmt),
                // Not extracted (not indexed, outside its scope, or not reached yet).
                _ => match f.lazy.and_then(|l| l.get(s, id, LazyCol::Prop(p))) {
                    Some(LazyVal::Prop(v)) => fmt(v),
                    _ => None,
                },
            }
            .unwrap_or_default()
        }
        ColumnId::LinkTarget | ColumnId::FileSignature | ColumnId::RecycleOrigin => {
            let col = match col {
                ColumnId::LinkTarget => LazyCol::LinkTarget,
                ColumnId::FileSignature => LazyCol::Signature,
                _ => LazyCol::RecycleOrigin,
            };
            match f.lazy.and_then(|l| l.get(s, id, col)) {
                Some(LazyVal::Text(t)) => t,
                _ => String::new(),
            }
        }
    }
}

/// Display text of a property value, with localised yes/no, pixels and stars (other
/// units are international abbreviations: `kbps`, `Hz`, `mm`, `f/2.8`, `fps`).
pub fn format_property(p: PropertyId, v: &fnc_props::Value) -> String {
    use fnc_props::{Unit, Value};
    match (p.ty(), p.unit(), v) {
        (PropType::Bool, _, Value::Int(i)) => t(if *i != 0 { "value-yes" } else { "value-no" }),
        (_, Unit::Pixels, Value::Int(i)) => crate::i18n::t_args("unit-pixels", &[("count", (*i).into())]),
        (_, Unit::Stars, Value::Int(i)) => crate::i18n::t_args("unit-stars", &[("count", (*i).into())]),
        _ => p.format(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_setting_roundtrip() {
        let spec: Vec<String> =
            ["path:500", "bogus:10", "size", "path:20", "dm:abc"].iter().map(|s| s.to_string()).collect();
        let cols = parse_columns(&spec);
        let ids: Vec<_> = cols.iter().map(|c| c.id).collect();
        assert_eq!(ids, [ColumnId::Name, ColumnId::Path, ColumnId::Size, ColumnId::DateModified]);
        assert_eq!(cols[1].width, 500.0);
        assert_eq!(cols[2].width, ColumnId::Size.default_width());
        assert_eq!(parse_columns(&columns_setting(&cols)), cols);
        for c in ColumnId::ALL.into_iter().chain(ColumnId::properties()) {
            assert_eq!(ColumnId::from_key(c.key()), Some(c));
        }
        let cols = parse_columns(&["width:77".to_string(), "camera-make".to_string()]);
        assert_eq!(cols[1], ColumnState { id: ColumnId::Property(PropertyId::WIDTH), width: 77.0 });
        assert_eq!(cols[2].id, ColumnId::Property(PropertyId::CAMERA_MAKER), "aliases resolve");
        assert_eq!(columns_setting(&cols)[2], "camera-maker:160");
        // The column chooser's filter box: titles and search names, ~300 entries.
        let all = ColumnId::properties();
        assert!(all.len() >= 290);
        assert_eq!(filter_properties(all.clone(), "").len(), all.len());
        let lens = filter_properties(all.clone(), "Lens");
        assert!(lens.contains(&ColumnId::Property(PropertyId::LENS_MAKER)), "{lens:?}");
        assert!(lens.contains(&ColumnId::Property(PropertyId::LENS_MODEL)));
        assert!(!lens.contains(&ColumnId::Property(PropertyId::WIDTH)));
        let by_name = filter_properties(all.clone(), "message-date");
        assert_eq!(by_name, [ColumnId::Property(PropertyId::DATE_RECEIVED), ColumnId::Property(PropertyId::DATE_SENT)]);
        assert!(filter_properties(all, "zzzz-nothing").is_empty());
        assert_eq!(ColumnId::Property(PropertyId::WIDTH).sort(), Some(SortKey::Property(PropertyId::WIDTH)));
        assert_eq!(ColumnId::DupeGroup.sort(), None);
        assert_eq!(ColumnId::Custom(2).sort(), Some(SortKey::Custom(2)));
        assert_eq!(ColumnId::Type.sort(), Some(SortKey::Column(SortColumn::Type)));
        assert_eq!(ColumnId::Extension.sort(), Some(SortKey::Column(SortColumn::Extension)));
        assert!(
            ColumnId::Property(PropertyId::WIDTH).numeric()
                && ColumnId::Property(PropertyId::WIDTH).sort_descending_first()
        );
        assert!(!ColumnId::Property(PropertyId::ARTIST).sort_descending_first());
    }

    #[test]
    fn sort_setting_roundtrip() {
        let c = SortKey::Column;
        let keys = parse_sort("size-descending;date-modified-ascending;bogus;name");
        assert_eq!(
            keys,
            [(c(SortColumn::Size), false), (c(SortColumn::DateModified), true), (c(SortColumn::Name), true)]
        );
        let mut spec = fnc_exec::SortSpec::by(SortColumn::Size, false, fnc_core::settings::FoldersFirst::Never);
        spec.push_key(SortColumn::DateModified, true);
        spec.push_property_key(PropertyId::DATE_TAKEN, false);
        assert_eq!(sort_setting(&spec), "size-descending;date-modified-ascending;date-taken-descending");
        assert_eq!(
            parse_sort(&sort_setting(&spec)),
            [
                (c(SortColumn::Size), false),
                (c(SortColumn::DateModified), true),
                (SortKey::Property(PropertyId::DATE_TAKEN), false)
            ]
        );
        // Custom formula columns and the type column round-trip too (session tabs).
        let ff = fnc_core::settings::FoldersFirst::Ascending;
        let mut spec = fnc_exec::SortSpec::by_custom(0, false, ff);
        spec.push_key(SortColumn::Type, true);
        spec.push_custom_key(8, true);
        let text = sort_setting(&spec);
        assert_eq!(text, "custom1-descending;type-ascending;custom9-ascending");
        assert_eq!(sort_spec(&text, ff), Some(spec));
        assert_eq!(parse_sort("custom3"), [(SortKey::Custom(2), true)]);
        assert!(parse_sort("custom10").is_empty());
    }

    #[test]
    fn file_columns_sort_and_persist() {
        let ff = fnc_core::settings::FoldersFirst::Ascending;
        for (c, f) in [
            (ColumnId::LinkTarget, FileField::LinkTarget),
            (ColumnId::FileSignature, FileField::Signature),
            (ColumnId::RecycleOrigin, FileField::RecycleOrigin),
        ] {
            assert_eq!(c.sort(), Some(SortKey::File(f)));
            assert!(!c.sort_descending_first(), "text: ascending first");
            assert_eq!(f.name(), c.key(), "sort key name = column key");
        }
        let mut spec = fnc_exec::SortSpec::by_file(FileField::LinkTarget, false, ff);
        spec.push_key(SortColumn::Size, true);
        spec.push_sort_key(SortKey::File(FileField::RecycleOrigin), true);
        let text = sort_setting(&spec);
        assert_eq!(text, "linktarget-descending;size-ascending;recyclebinoriginalpath-ascending");
        assert_eq!(sort_spec(&text, ff), Some(spec));
        assert_eq!(parse_sort("filesignature"), [(SortKey::File(FileField::Signature), true)]);
    }

    #[test]
    fn custom_formulas_follow_the_setting() {
        set_custom_columns(&["Len=LEN($name:)".to_string(), "$size:*2".to_string(), "bad=LEN(".to_string()]);
        let a = custom_formulas();
        assert_eq!(a.len(), 3);
        assert!(a[0].is_some() && a[1].is_some() && a[2].is_none());
        assert_eq!(a, custom_formulas(), "same Arcs while the setting is unchanged");
        set_custom_columns(&["Len=LEN($name:)".to_string()]);
        assert_ne!(custom_formulas(), a);
        set_custom_columns(&[]);
    }

    #[test]
    fn attribute_letters_order() {
        use fnc_core::flags::attr as a;
        assert_eq!(attribute_letters(a::HIDDEN | a::READONLY | a::ARCHIVE), "RHA");
        assert_eq!(attribute_letters(0), "");
    }
}

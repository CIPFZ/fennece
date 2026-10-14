//! Filters and bookmarks (SPEC ch.2 §7, ch.3 §3): models, built-ins, persistence in
//! `filters.toml` / `bookmarks.toml`, and import of Everything's `Filters.csv` /
//! `Bookmarks.csv`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use fnc_core::upgrade::Recovery;

use crate::i18n::t;

/// A named search restriction (filter bar / filter menu).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    /// Display name.
    pub name: String,
    /// Search AND-ed with the user's search.
    pub search: String,
    /// Macro name: `name:` in a search expands to this filter's search.
    #[serde(rename = "macro")]
    pub macro_name: String,
    /// Match case while active.
    pub case: bool,
    /// Match whole words while active.
    pub whole_word: bool,
    /// Match path while active.
    pub path: bool,
    /// Match diacritics while active.
    pub diacritics: bool,
    /// Treat the user's search as a regex while active.
    pub regex: bool,
    /// Optional shortcut (`Ctrl+Shift+A`).
    pub key: String,
    /// Built-in filters are re-localised and cannot be deleted.
    #[serde(skip)]
    pub builtin: bool,
}

impl Filter {
    /// The engine's view of this filter.
    pub fn to_query(&self) -> fnc_query::Filter {
        fnc_query::Filter {
            name: self.name.clone(),
            search: self.search.clone(),
            macro_name: Some(self.macro_name.trim().to_string()).filter(|m| !m.is_empty()),
            case: self.case,
            whole_word: self.whole_word,
            path: self.path,
            diacritics: self.diacritics,
            regex: self.regex,
        }
    }
}

/// A saved search.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    /// Display name.
    pub name: String,
    /// Search text.
    pub search: String,
    /// Filter name (empty = Everything).
    pub filter: String,
    /// Sort setting (`size-descending;name-ascending`), empty = keep.
    pub sort: String,
    /// Optional shortcut.
    pub key: String,
    /// Group in the bookmarks menu (empty = top level).
    pub folder: String,
}

#[derive(Default, Serialize, Deserialize)]
struct FiltersFile {
    #[serde(default)]
    filter: Vec<Filter>,
}

#[derive(Default, Serialize, Deserialize)]
struct BookmarksFile {
    #[serde(default)]
    bookmark: Vec<Bookmark>,
}

/// Built-in filters (SPEC ch.2 §7); names are localised at load time.
pub fn builtin_filters() -> Vec<Filter> {
    let f = |key: &str, search: &str, macro_name: &str| Filter {
        name: t(key),
        search: search.into(),
        macro_name: macro_name.into(),
        builtin: true,
        ..Filter::default()
    };
    vec![
        f("filter-audio", "audio:", ""),
        f("filter-compressed", "zip:", ""),
        f("filter-document", "doc:", ""),
        f("filter-executable", "exe:", ""),
        f("filter-folder", "folder:", ""),
        f("filter-picture", "pic:", ""),
        f("filter-video", "video:", ""),
    ]
}

/// User filters after the built-ins.
#[derive(Debug, Default)]
pub struct Filters {
    /// All filters (index 0 = first after "Everything").
    pub list: Vec<Filter>,
    path: Option<PathBuf>,
    /// The file was not used as it is (other schema, damage): where the original went.
    pub recovery: Option<Recovery>,
}

impl Filters {
    /// Built-ins followed by `path`'s user filters (ADR-038: a damaged or newer file is
    /// copied to a backup first).
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut list = builtin_filters();
        let mut recovery = None;
        if let Some(p) = &path {
            let (f, r) = crate::upgrade::load_toml::<FiltersFile>(p);
            list.extend(f.filter);
            recovery = r;
        }
        Self { list, path, recovery }
    }

    /// Saves the user filters.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let file = FiltersFile { filter: self.list.iter().filter(|f| !f.builtin).cloned().collect() };
        crate::upgrade::save_toml(p, &file)
    }

    /// Index of the filter named `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|f| f.name == name)
    }

    /// User macros defined by filters.
    pub fn macros(&self) -> Vec<fnc_query::UserMacro> {
        self.list.iter().filter_map(|f| f.to_query().user_macro()).collect()
    }

    /// Imports Everything's `Filters.csv`; returns the number added.
    pub fn import_csv(&mut self, text: &str) -> usize {
        let rows = csv_rows(text);
        let Some((header, body)) = rows.split_first() else { return 0 };
        let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
        let get = |row: &[String], name: &str| col(name).and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let flag = |row: &[String], name: &str| get(row, name).trim() == "1";
        let mut n = 0;
        for row in body {
            let name = get(row, "Name");
            if name.is_empty() || self.find(&name).is_some() {
                continue;
            }
            self.list.push(Filter {
                search: get(row, "Search"),
                macro_name: get(row, "Macro"),
                case: flag(row, "Case"),
                whole_word: flag(row, "Whole Word"),
                path: flag(row, "Path"),
                diacritics: flag(row, "Diacritics"),
                regex: flag(row, "Regex"),
                key: get(row, "Key"),
                name,
                builtin: false,
            });
            n += 1;
        }
        n
    }
}

/// Bookmarks.
#[derive(Debug, Default)]
pub struct Bookmarks {
    /// All bookmarks in menu order.
    pub list: Vec<Bookmark>,
    path: Option<PathBuf>,
    /// The file was not used as it is (other schema, damage): where the original went.
    pub recovery: Option<Recovery>,
}

impl Bookmarks {
    /// Loads `path` (ADR-038: a damaged or newer file is copied to a backup first).
    pub fn load(path: Option<PathBuf>) -> Self {
        let (file, recovery) = match path.as_deref() {
            Some(p) => crate::upgrade::load_toml::<BookmarksFile>(p),
            None => (BookmarksFile::default(), None),
        };
        Self { list: file.bookmark, path, recovery }
    }

    /// Saves.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        crate::upgrade::save_toml(p, &BookmarksFile { bookmark: self.list.clone() })
    }

    /// Imports Everything's `Bookmarks.csv`; returns the number added.
    pub fn import_csv(&mut self, text: &str) -> usize {
        let rows = csv_rows(text);
        let Some((header, body)) = rows.split_first() else { return 0 };
        let col = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
        let get = |row: &[String], name: &str| col(name).and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let mut n = 0;
        for row in body {
            let name = get(row, "Name");
            if name.is_empty() {
                continue;
            }
            self.list.push(Bookmark {
                search: get(row, "Search"),
                filter: get(row, "Filter"),
                key: get(row, "Key"),
                folder: get(row, "Folder"),
                sort: String::new(),
                name,
            });
            n += 1;
        }
        n
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

impl Filters {
    /// User filters as Everything's `Filters.csv`.
    pub fn export_csv(&self) -> String {
        let mut out = String::from("Name,Case,Whole Word,Path,Diacritics,Regex,Search,Macro,Key\r\n");
        let b = |v: bool| if v { "1" } else { "0" };
        for f in self.list.iter().filter(|f| !f.builtin) {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{}\r\n",
                csv_field(&f.name),
                b(f.case),
                b(f.whole_word),
                b(f.path),
                b(f.diacritics),
                b(f.regex),
                csv_field(&f.search),
                csv_field(&f.macro_name),
                csv_field(&f.key)
            ));
        }
        out
    }
}

impl Bookmarks {
    /// Bookmarks as Everything's `Bookmarks.csv`.
    pub fn export_csv(&self) -> String {
        let mut out = String::from("Name,Search,Filter,Key,Folder\r\n");
        for b in &self.list {
            out.push_str(&format!(
                "{},{},{},{},{}\r\n",
                csv_field(&b.name),
                csv_field(&b.search),
                csv_field(&b.filter),
                csv_field(&b.key),
                csv_field(&b.folder)
            ));
        }
        out
    }
}

fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut v = Vec::new();
            fnc_index::efu::split_csv_line(l, &mut v);
            v
        })
        .collect()
}

/// `filters.toml` / `bookmarks.toml` next to the settings.
pub fn data_file(settings_path: &Path, name: &str) -> PathBuf {
    settings_path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_persist_and_combine() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("filters.toml");
        let mut f = Filters::load(Some(p.clone()));
        let builtins = f.list.len();
        assert!(f.find("Audio").is_some());
        f.list.push(Filter {
            name: "Big".into(),
            search: "size:>1gb".into(),
            macro_name: "big".into(),
            ..Filter::default()
        });
        f.save().unwrap();
        let g = Filters::load(Some(p));
        assert_eq!(g.list.len(), builtins + 1);
        assert_eq!(g.macros()[0].name, "big");
        let q = g.list[builtins].to_query();
        assert_eq!(q.search, "size:>1gb");
        assert_eq!(q.user_macro().unwrap().name, "big");
    }

    #[test]
    fn imports_everything_csv() {
        crate::i18n::init("en");
        let csv = "Name,Case,Whole Word,Path,Diacritics,Regex,Search,Macro,Key\n\
                   \"Photos 2026\",0,0,1,0,0,\"pic: dm:2026\",photos,\n\
                   Audio,0,0,0,0,0,audio:,,\n";
        let mut f = Filters::load(None);
        assert_eq!(f.import_csv(csv), 1, "existing names are skipped");
        let p = &f.list[f.find("Photos 2026").unwrap()];
        assert!(p.path && !p.case);
        assert_eq!(p.search, "pic: dm:2026");
        let mut b = Bookmarks::load(None);
        assert_eq!(b.import_csv("Name,Search,Filter\nLogs,ext:log,\n"), 1);
        assert_eq!(b.list[0].search, "ext:log");
    }

    #[test]
    fn csv_export_roundtrip() {
        crate::i18n::init("en");
        let mut f = Filters::load(None);
        f.list.push(Filter { name: "A, b".into(), search: "ext:\"x\"".into(), case: true, ..Filter::default() });
        let csv = f.export_csv();
        let mut g = Filters::load(None);
        assert_eq!(g.import_csv(&csv), 1);
        let a = &g.list[g.find("A, b").unwrap()];
        assert!(a.case && a.search == "ext:\"x\"");
        let mut b = Bookmarks::load(None);
        b.list.push(Bookmark { name: "n".into(), search: "s".into(), folder: "dir".into(), ..Bookmark::default() });
        let mut c = Bookmarks::load(None);
        assert_eq!(c.import_csv(&b.export_csv()), 1);
        assert_eq!(c.list[0].folder, "dir");
    }
}

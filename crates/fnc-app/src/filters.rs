//! Filters and bookmarks (SPEC ch.2 §7, ch.3 §3): models, built-ins, persistence in
//! `filters.toml` / `bookmarks.toml`, and import of Everything's `Filters.csv` /
//! `Bookmarks.csv`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::history::write_atomic;
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
}

impl Filters {
    /// Built-ins followed by `path`'s user filters.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut list = builtin_filters();
        if let Some(p) = &path
            && let Ok(text) = std::fs::read_to_string(p)
        {
            match toml::from_str::<FiltersFile>(&text) {
                Ok(f) => list.extend(f.filter),
                Err(e) => tracing::warn!(error = %e, file = %p.display(), "cannot read filters"),
            }
        }
        Self { list, path }
    }

    /// Saves the user filters.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let file = FiltersFile { filter: self.list.iter().filter(|f| !f.builtin).cloned().collect() };
        write_atomic(p, &toml::to_string(&file).map_err(std::io::Error::other)?)
    }

    /// Index of the filter named `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|f| f.name == name)
    }

    /// User macros defined by filters.
    pub fn macros(&self) -> Vec<fnc_query::UserMacro> {
        self.list
            .iter()
            .filter(|f| !f.macro_name.trim().is_empty())
            .map(|f| fnc_query::UserMacro { name: f.macro_name.trim().to_string(), search: f.search.clone() })
            .collect()
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

/// The search a set of active filters adds (`or`: any filter matches).
pub fn combine(filters: &[&Filter], or: bool) -> Option<String> {
    let parts: Vec<String> =
        filters.iter().filter(|f| !f.search.trim().is_empty()).map(|f| format!("<{}>", f.search.trim())).collect();
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(parts.join(if or { " | " } else { " " })),
    }
}

/// Bookmarks.
#[derive(Debug, Default)]
pub struct Bookmarks {
    /// All bookmarks in menu order.
    pub list: Vec<Bookmark>,
    path: Option<PathBuf>,
}

impl Bookmarks {
    /// Loads `path`.
    pub fn load(path: Option<PathBuf>) -> Self {
        let list = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str::<BookmarksFile>(&t).ok())
            .map(|f| f.bookmark)
            .unwrap_or_default();
        Self { list, path }
    }

    /// Saves.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        write_atomic(
            p,
            &toml::to_string(&BookmarksFile { bookmark: self.list.clone() }).map_err(std::io::Error::other)?,
        )
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
        let a = &g.list[0];
        let big = &g.list[builtins];
        assert_eq!(combine(&[a], true).unwrap(), "<audio:>");
        assert_eq!(combine(&[a, big], true).unwrap(), "<audio:> | <size:>1gb>");
        assert_eq!(combine(&[a, big], false).unwrap(), "<audio:> <size:>1gb>");
        assert!(combine(&[], true).is_none());
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
}

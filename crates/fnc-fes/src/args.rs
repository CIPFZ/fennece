//! `es.exe` 1.1 compatible argument parsing (SPEC ch.4 §2).
//!
//! Rules (from voidtools ES): switches start with `-` or `/`; dashes inside a switch name
//! are optional (`-maxresults` = `-max-results`) but may not be added (`-a-d` ≠ `-ad`);
//! `-switch=value` equals `-switch value`; `no-` disables boolean switches and columns;
//! unknown `-` switches are errors while unknown `/` words are search text (`/downloads`);
//! `--` ends switch parsing. Everything else is search text, joined with spaces (words
//! containing spaces are re-quoted because the shell already removed their quotes).

use std::path::PathBuf;

use fnc_ipc::Column;
use fnc_ipc::export::{ExportOptions, Format, Newline};
use fnc_ipc::format::{DateFormat, SizeFormat};

/// What fes does.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Search and print results.
    #[default]
    Search,
    /// Print the result count.
    ResultCount,
    /// Print the total size of the results.
    TotalSize,
    /// Print the version reported to Everything clients.
    EverythingVersion,
    /// Print fes's own version.
    Version,
    /// Print help.
    Help,
    /// Print the column/property names.
    ListProperties,
    /// `-get-run-count <file>`.
    GetRunCount(String),
    /// `-inc-run-count <file>`.
    IncRunCount(String),
    /// `-set-run-count <file> <n>`.
    SetRunCount(String, u32),
    /// `-get-folder-size <folder>`.
    FolderSize(String),
}

/// Lifecycle commands sent before the search (`-exit`, `-reindex`, `-update`, `-save-db`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `-exit`, `-quit`.
    Exit,
    /// `-reindex`, `-rebuild`.
    Rebuild,
    /// `-update`.
    Update,
    /// `-save-db`.
    SaveDb,
}

/// Parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    /// Mode.
    pub mode: Mode,
    /// Search text (switch-generated terms included).
    pub search: String,
    /// Filter from `-path`/`-parent`/`-ad`… (combined as `< filter > < search >`).
    pub filter: String,
    /// Match case.
    pub case: bool,
    /// Match whole words.
    pub whole_word: bool,
    /// Match path.
    pub match_path: bool,
    /// Match diacritics.
    pub diacritics: bool,
    /// Match prefix.
    pub prefix: bool,
    /// Match suffix.
    pub suffix: bool,
    /// Ignore punctuation.
    pub ignore_punctuation: bool,
    /// Ignore white space.
    pub ignore_whitespace: bool,
    /// `-o`.
    pub offset: u64,
    /// `-n` (`None` = all).
    pub max: Option<u64>,
    /// Primary sort `(column, ascending)`; `None` direction = column default.
    pub sort: Option<(Column, Option<bool>)>,
    /// `-sort-ascending` / `-sort-descending` without a column.
    pub sort_direction: Option<bool>,
    /// Export options (format, columns, header, formats, ...).
    pub export: ExportOptions,
    /// `-export-* <file>`.
    pub export_file: Option<PathBuf>,
    /// `-highlight`.
    pub highlight: bool,
    /// `-highlight-color` (console attribute).
    pub highlight_color: u8,
    /// `-instance`.
    pub instance: String,
    /// `-timeout` in milliseconds.
    pub timeout_ms: Option<u64>,
    /// `-no-result-error`.
    pub no_result_error: bool,
    /// `-footer`.
    pub footer: Option<bool>,
    /// Lifecycle commands.
    pub commands: Vec<Command>,
    /// `-save-settings`.
    pub save_settings: bool,
    /// `-clear-settings`.
    pub clear_settings: bool,
    /// `-offline`: query `index.fnx` directly.
    pub offline: bool,
    /// `-filelist <file.efu>`: query file lists directly (repeatable).
    pub file_lists: Vec<PathBuf>,
    /// `-db <file>`: index file for `-offline`.
    pub db: Option<PathBuf>,
    /// Switches that are accepted but have no effect (for diagnostics).
    pub ignored: Vec<String>,
    /// Raw switch arguments worth saving with `-save-settings`.
    pub settings: Vec<String>,
    /// `-debug-panic` (hidden, not in the help): panic to test the crash report (REL-02).
    pub debug_panic: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            mode: Mode::Search,
            search: String::new(),
            filter: String::new(),
            case: false,
            whole_word: false,
            match_path: false,
            diacritics: false,
            prefix: false,
            suffix: false,
            ignore_punctuation: false,
            ignore_whitespace: false,
            offset: 0,
            max: None,
            sort: None,
            sort_direction: None,
            export: ExportOptions::default(),
            export_file: None,
            highlight: false,
            highlight_color: 0x0A,
            instance: String::new(),
            timeout_ms: None,
            no_result_error: false,
            footer: None,
            commands: Vec::new(),
            save_settings: false,
            clear_settings: false,
            offline: false,
            file_lists: Vec::new(),
            db: None,
            ignored: Vec::new(),
            settings: Vec::new(),
            debug_panic: false,
        }
    }
}

impl Args {
    /// Search sent to the index: `< filter > < search >` when a filter is present.
    pub fn full_search(&self) -> String {
        if self.filter.is_empty() {
            return self.search.clone();
        }
        let mut s = format!("< {} > < {}", self.filter, self.search);
        if self.search.matches('"').count() % 2 == 1 {
            s.push('"');
        }
        s.push_str(" >");
        s
    }

    /// Effective sort `(column, ascending)`.
    pub fn effective_sort(&self) -> (Column, bool) {
        let (col, dir) = self.sort.unwrap_or((Column::Name, None));
        let asc = dir.or(self.sort_direction).unwrap_or_else(|| col.default_ascending());
        (col, asc)
    }
}

/// Argument errors (exit code 2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    /// Unknown `-` switch.
    #[error("unknown switch: {0}")]
    Unknown(String),
    /// A switch needs a value.
    #[error("{0} expects a value")]
    MissingValue(String),
    /// Bad value.
    #[error("invalid value for {0}: {1}")]
    BadValue(String, String),
    /// Recognised but not supported (Everything 1.5 journal).
    #[error("{0} is not supported by fes")]
    Unsupported(String),
}

/// es-style switch name match: dashes in `pattern` are optional in `name`, other
/// characters must match case-insensitively.
pub fn option_eq(name: &str, pattern: &str) -> bool {
    let n = name.as_bytes();
    let mut i = 0;
    for &p in pattern.as_bytes() {
        if p == b'-' {
            if n.get(i) == Some(&b'-') {
                i += 1;
            }
            continue;
        }
        match n.get(i) {
            Some(&c) if c.to_ascii_lowercase() == p => i += 1,
            _ => return false,
        }
    }
    i == n.len()
}

fn any(name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| option_eq(name, p))
}

/// Column switch name → column (`-name`, `-path-column`, `-size`, `-dm`, ...).
pub fn column_switch(name: &str) -> Option<Column> {
    const TABLE: &[(&[&str], Column)] = &[
        (&["name"], Column::Name),
        (&["path-column"], Column::Path),
        (&["full-path-and-name", "filename-column", "full-path"], Column::FullPath),
        (&["extension", "ext"], Column::Extension),
        (&["type"], Column::Type),
        (&["size"], Column::Size),
        (&["date-created", "dc"], Column::DateCreated),
        (&["date-modified", "dm"], Column::DateModified),
        (&["date-accessed", "da"], Column::DateAccessed),
        (&["attributes", "attribs", "attrib"], Column::Attributes),
        (&["file-list-file-name", "filelist-filename", "file-list-filename"], Column::FileListName),
        (&["run-count"], Column::RunCount),
        (&["date-run"], Column::DateRun),
        (&["date-recently-changed", "rc"], Column::DateRecentlyChanged),
    ];
    TABLE.iter().find(|(names, _)| any(name, names)).map(|(_, c)| *c)
}

/// Sort/column property name (`size`, `date-modified`, `path`, ...).
pub fn property(name: &str) -> Option<Column> {
    if option_eq(name, "path") {
        return Some(Column::Path);
    }
    if any(name, &["filename", "full-path-and-name", "full-path"]) {
        return Some(Column::FullPath);
    }
    column_switch(name).or_else(|| Column::parse(name))
}

/// Parses `name[-ascending|-descending]` (also `nameascending`).
pub fn parse_sort(v: &str) -> Option<(Column, Option<bool>)> {
    let lower = v.to_ascii_lowercase();
    for (suffix, asc) in [("-ascending", true), ("-descending", false), ("ascending", true), ("descending", false)] {
        if let Some(col) = lower.strip_suffix(suffix)
            && !col.is_empty()
        {
            return property(col.trim_end_matches('-')).map(|c| (c, Some(asc)));
        }
    }
    property(&lower).map(|c| (c, None))
}

fn quote_word(w: &str) -> String {
    if w.contains(char::is_whitespace) && !w.contains('"') { format!("\"{w}\"") } else { w.to_string() }
}

fn push_term(s: &mut String, t: &str) {
    if !s.is_empty() {
        s.push(' ');
    }
    s.push_str(t);
}

fn absolute(p: &str) -> String {
    match std::path::absolute(p) {
        Ok(a) => a.to_string_lossy().into_owned(),
        Err(_) => p.to_string(),
    }
}

fn with_trailing_sep(p: &str) -> String {
    let sep = if p.starts_with('/') { '/' } else { '\\' };
    if p.ends_with(['\\', '/']) { p.to_string() } else { format!("{p}{sep}") }
}

fn parent_of(p: &str) -> String {
    let t = p.trim_end_matches(['\\', '/']);
    match t.rfind(['\\', '/']) {
        Some(i) => t[..=i].to_string(),
        None => t.to_string(),
    }
}

struct Cursor<I: Iterator<Item = String>> {
    it: std::iter::Peekable<I>,
    pending: Option<String>,
}

impl<I: Iterator<Item = String>> Cursor<I> {
    fn next(&mut self) -> Option<String> {
        self.pending.take().or_else(|| self.it.next())
    }

    fn value(&mut self, switch: &str) -> Result<String, ArgError> {
        self.next().ok_or_else(|| ArgError::MissingValue(switch.to_string()))
    }
}

fn int<T: std::str::FromStr>(switch: &str, v: &str) -> Result<T, ArgError> {
    let t = v.trim();
    let parsed = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok().and_then(|n| n.to_string().parse().ok())
    } else {
        t.parse().ok()
    };
    parsed.ok_or_else(|| ArgError::BadValue(switch.to_string(), v.to_string()))
}

const VALUE_IGNORED: &[&str] = &[
    "column-colors",
    "set-column-colors",
    "set-column-color",
    "add-column-colors",
    "add-column-color",
    "remove-column-colors",
    "remove-column-color",
    "column-widths",
    "set-column-widths",
    "set-column-width",
    "add-column-widths",
    "add-column-width",
    "remove-column-widths",
    "remove-column-width",
    "cp",
    "code-page",
    "console-wide",
    "console-width",
    "console-high",
    "console-height",
    "console-window-wide",
    "console-window-width",
    "console-window-high",
    "console-window-height",
    "locale-grouping",
    "locale-thousand",
    "locale-decimal",
    "locale-lzero",
    "locale-negnumber",
    "aspect-ratio-format",
    "redirect-stdout",
    "redirect-stderr",
];

const FLAG_IGNORED: &[&str] = &[
    "ipc1",
    "ipc2",
    "ipc3",
    "no-ipc1",
    "no-ipc2",
    "no-ipc3",
    "pause",
    "more",
    "no-pause",
    "no-more",
    "debug",
    "no-debug",
    "argv",
    "no-argv",
    "default-argv",
    "help-on-no-args",
    "no-help-on-no-args",
    "empty-search-help",
    "no-empty-search-help",
    "hide-empty-search-results",
    "no-hide-empty-search-results",
    "sort-mix",
    "no-sort-mix",
    "no-time-out",
];

const JOURNAL: &[&str] = &[
    "j",
    "journal",
    "get-journal-id",
    "get-journal-pos",
    "watch",
    "from-journal-id",
    "from-change-id",
    "from-journal-pos",
    "after-journal-pos",
    "from-date",
    "from-yesterday",
    "from-today",
    "from-now",
    "to-journal-id",
    "to-change-id",
    "to-journal-pos",
    "to-date",
    "to-now",
    "to-today",
    "to-tomorrow",
    "action-filter",
    "changed-today",
    "changed-yesterday",
    "added-filename-only",
    "removed-filename-only",
];

fn color_switch(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.strip_suffix("color").is_some_and(|base| {
        let base = base.trim_end_matches('-');
        base == "highlight" || column_switch(base).is_some() || property(base).is_some() || base == "filename"
    })
}

fn width_switch(name: &str) -> Option<Column> {
    let n = name.to_ascii_lowercase();
    let base = n.strip_suffix("width").or_else(|| n.strip_suffix("wide"))?.trim_end_matches('-').to_string();
    if base == "filename" || base == "file-name" {
        return Some(Column::FullPath);
    }
    if base == "path" {
        return Some(Column::Path);
    }
    column_switch(&base).or_else(|| property(&base))
}

fn set_format(a: &mut Args, f: Format) {
    a.export.format = f;
}

fn columns_list(v: &str) -> Result<Vec<Column>, ArgError> {
    v.split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| property(s).ok_or_else(|| ArgError::BadValue("-columns".into(), s.to_string())))
        .collect()
}

/// Parses an `es`-style argument list (without the program name).
pub fn parse_args(raw: impl IntoIterator<Item = String>) -> Result<Args, ArgError> {
    let mut a = Args::default();
    let mut cur = Cursor { it: raw.into_iter().peekable(), pending: None };
    let mut words: Vec<String> = Vec::new();
    let mut columns: Vec<Column> = Vec::new();
    while let Some(arg) = cur.next() {
        let Some(body) = arg.strip_prefix('-').or_else(|| arg.strip_prefix('/')) else {
            words.push(quote_word(&arg));
            continue;
        };
        let slash = arg.starts_with('/');
        if arg == "--" {
            for w in cur.it.by_ref() {
                words.push(quote_word(&w));
            }
            break;
        }
        // `-switch=value`
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) if !n.is_empty() => (n.to_string(), Some(v.to_string())),
            _ => (body.to_string(), None),
        };
        if let Some(v) = inline {
            cur.pending = Some(v);
        }
        let n = name.as_str();
        let save = |a: &mut Args, extra: Option<&str>| {
            a.settings.push(arg.clone());
            if let Some(v) = extra {
                a.settings.push(v.to_string());
            }
        };
        // ---- search modifiers
        if any(n, &["r", "regex"]) {
            let v = cur.value(&arg)?;
            push_term(&mut a.search, &format!("regex:{v}"));
        } else if any(n, &["i", "case"]) {
            a.case = true;
            save(&mut a, None);
        } else if any(n, &["no-i", "no-case"]) {
            a.case = false;
            save(&mut a, None);
        } else if any(n, &["w", "ww", "whole-word", "whole-words"]) {
            a.whole_word = true;
            save(&mut a, None);
        } else if any(n, &["no-w", "no-ww", "no-whole-word", "no-whole-words"]) {
            a.whole_word = false;
            save(&mut a, None);
        } else if any(n, &["p", "match-path"]) {
            a.match_path = true;
            save(&mut a, None);
        } else if any(n, &["no-p", "no-match-path"]) {
            a.match_path = false;
            save(&mut a, None);
        } else if any(n, &["a", "diacritics"]) {
            a.diacritics = true;
            save(&mut a, None);
        } else if any(n, &["no-a", "no-diacritics"]) {
            a.diacritics = false;
            save(&mut a, None);
        } else if option_eq(n, "prefix") {
            a.prefix = true;
        } else if option_eq(n, "no-prefix") {
            a.prefix = false;
        } else if option_eq(n, "suffix") {
            a.suffix = true;
        } else if option_eq(n, "no-suffix") {
            a.suffix = false;
        } else if any(n, &["ignore-punctuation", "ignore-punc", "no-punctuation", "no-punc"]) {
            a.ignore_punctuation = true;
        } else if any(n, &["no-ignore-punctuation", "no-ignore-punc", "punctuation", "punc"]) {
            a.ignore_punctuation = false;
        } else if any(n, &["ignore-white-space", "ignore-ws", "no-white-space", "no-ws"]) {
            a.ignore_whitespace = true;
        } else if any(n, &["no-ignore-white-space", "no-ignore-ws", "white-space", "ws"]) {
            a.ignore_whitespace = false;
        // ---- instance / lifecycle
        } else if option_eq(n, "instance") {
            a.instance = cur.value(&arg)?;
        } else if any(n, &["exit", "quit"]) {
            a.commands.push(Command::Exit);
        } else if any(n, &["re-index", "re-build"]) {
            a.commands.push(Command::Rebuild);
        } else if option_eq(n, "update") {
            a.commands.push(Command::Update);
        } else if option_eq(n, "save-db") {
            a.commands.push(Command::SaveDb);
        } else if option_eq(n, "set-run-count") {
            let f = cur.value(&arg)?;
            let c = cur.value(&arg)?;
            a.mode = Mode::SetRunCount(f, int(&arg, &c)?);
        } else if option_eq(n, "inc-run-count") {
            a.mode = Mode::IncRunCount(cur.value(&arg)?);
        } else if option_eq(n, "get-run-count") {
            a.mode = Mode::GetRunCount(cur.value(&arg)?);
        } else if option_eq(n, "get-folder-size") {
            a.mode = Mode::FolderSize(cur.value(&arg)?);
        // ---- highlight
        } else if option_eq(n, "highlight-color") {
            let v = cur.value(&arg)?;
            a.highlight_color = int(&arg, &v)?;
            save(&mut a, Some(&v));
        } else if option_eq(n, "highlight") {
            a.highlight = true;
            save(&mut a, None);
        } else if option_eq(n, "no-highlight") {
            a.highlight = false;
            save(&mut a, None);
        // ---- formats / export
        } else if let Some((f, export)) = [
            ("csv", Format::Csv),
            ("efu", Format::Efu),
            ("txt", Format::Txt),
            ("m3u", Format::M3u),
            ("m3u8", Format::M3u8),
            ("tsv", Format::Tsv),
            ("json", Format::Json),
        ]
        .iter()
        .find_map(|&(s, f)| {
            if option_eq(n, s) {
                Some((f, false))
            } else if option_eq(n, &format!("export-{s}")) {
                Some((f, true))
            } else {
                None
            }
        }) {
            set_format(&mut a, f);
            if export {
                a.export_file = Some(PathBuf::from(cur.value(&arg)?));
            }
        } else if option_eq(n, "no-format") {
            set_format(&mut a, Format::Plain);
        } else if option_eq(n, "header") {
            a.export.header = Some(true);
        } else if option_eq(n, "no-header") {
            a.export.header = Some(false);
        } else if option_eq(n, "default-header") {
            a.export.header = None;
        } else if option_eq(n, "footer") {
            a.footer = Some(true);
        } else if option_eq(n, "no-footer") {
            a.footer = Some(false);
        } else if option_eq(n, "default-footer") {
            a.footer = None;
        } else if option_eq(n, "double-quote") {
            a.export.double_quote = true;
            save(&mut a, None);
        } else if option_eq(n, "no-double-quote") {
            a.export.double_quote = false;
            save(&mut a, None);
        } else if option_eq(n, "utf8-bom") {
            a.export.utf8_bom = true;
        } else if option_eq(n, "no-utf8-bom") {
            a.export.utf8_bom = false;
        } else if option_eq(n, "folder-append-path-separator") {
            a.export.folder_append_separator = Some(true);
        } else if option_eq(n, "no-folder-append-path-separator") {
            a.export.folder_append_separator = Some(false);
        } else if option_eq(n, "default-folder-append-path-separator") {
            a.export.folder_append_separator = None;
        } else if option_eq(n, "nul") {
            a.export.newline = Some(Newline::Nul);
        } else if option_eq(n, "lf") {
            a.export.newline = Some(Newline::Lf);
        } else if option_eq(n, "crlf") {
            a.export.newline = Some(Newline::CrLf);
        } else if option_eq(n, "no-new-line") {
            a.export.newline = Some(Newline::None);
        } else if option_eq(n, "size-format") {
            let v = cur.value(&arg)?;
            a.export.size_format =
                SizeFormat::from_es(int(&arg, &v)?).ok_or_else(|| ArgError::BadValue(arg.clone(), v.clone()))?;
            save(&mut a, Some(&v));
        } else if option_eq(n, "date-format") {
            let v = cur.value(&arg)?;
            a.export.date_format =
                DateFormat::from_es(int(&arg, &v)?).ok_or_else(|| ArgError::BadValue(arg.clone(), v.clone()))?;
            save(&mut a, Some(&v));
        } else if option_eq(n, "no-digit-grouping") {
            a.export.digit_grouping = false;
            save(&mut a, None);
        } else if option_eq(n, "digit-grouping") {
            a.export.digit_grouping = true;
            save(&mut a, None);
        } else if option_eq(n, "size-leading-zero") {
            a.export.size_leading_zero = true;
        } else if option_eq(n, "no-size-leading-zero") {
            a.export.size_leading_zero = false;
        } else if option_eq(n, "run-count-leading-zero") {
            a.export.run_count_leading_zero = true;
        } else if option_eq(n, "no-run-count-leading-zero") {
            a.export.run_count_leading_zero = false;
        // ---- settings, help, version
        } else if option_eq(n, "save-settings") {
            a.save_settings = true;
        } else if option_eq(n, "clear-settings") {
            a.clear_settings = true;
        } else if any(n, &["?", "help", "h"]) {
            a.mode = Mode::Help;
        } else if option_eq(n, "debug-panic") {
            a.debug_panic = true;
        } else if option_eq(n, "version") {
            a.mode = Mode::Version;
        } else if option_eq(n, "get-everything-version") {
            a.mode = Mode::EverythingVersion;
        } else if option_eq(n, "list-properties") {
            a.mode = Mode::ListProperties;
        // ---- paths (before column switches: `-path` would be eaten as a column)
        } else if option_eq(n, "path") {
            let p = cur.value(&arg)?;
            push_term(&mut a.filter, &format!("\"{}\"", with_trailing_sep(&absolute(&p))));
        } else if option_eq(n, "parent-path") {
            let p = cur.value(&arg)?;
            push_term(&mut a.filter, &format!("\"{}\"", with_trailing_sep(&parent_of(&absolute(&p)))));
        } else if option_eq(n, "parent") {
            let p = cur.value(&arg)?;
            push_term(&mut a.filter, &format!("parent:\"{}\"", with_trailing_sep(&absolute(&p))));
        // ---- sort
        } else if option_eq(n, "sort-ascending") {
            a.sort_direction = Some(true);
            save(&mut a, None);
        } else if option_eq(n, "sort-descending") {
            a.sort_direction = Some(false);
            save(&mut a, None);
        } else if option_eq(n, "sort") {
            let v = cur.value(&arg)?;
            let first = v.split(';').next().unwrap_or_default();
            a.sort = Some(parse_sort(first).ok_or_else(|| ArgError::BadValue(arg.clone(), v.clone()))?);
            save(&mut a, Some(&v));
        } else if option_eq(n, "s") {
            a.sort = Some((Column::Path, Some(true)));
            save(&mut a, None);
        } else if let Some(s) = sort_switch(n) {
            a.sort = Some(s);
            save(&mut a, None);
        } else if slash && let Some(s) = dir_sort(n) {
            a.sort = Some(s);
        // ---- paging
        } else if any(n, &["o", "offset", "viewport-offset"]) {
            let v = cur.value(&arg)?;
            a.offset = int(&arg, &v)?;
        } else if any(n, &["n", "max-results", "count", "viewport-count"]) {
            let v = cur.value(&arg)?;
            a.max = Some(int(&arg, &v)?);
        } else if option_eq(n, "time-out") {
            let v = cur.value(&arg)?;
            a.timeout_ms = Some(int(&arg, &v)?);
        // ---- attribute filters
        } else if option_eq(n, "ad") {
            push_term(&mut a.filter, "folder:");
        } else if option_eq(n, "a-d") {
            push_term(&mut a.filter, "file:");
        } else if any(n, &["get-result-count"]) {
            a.mode = Mode::ResultCount;
        } else if option_eq(n, "get-total-size") {
            a.mode = Mode::TotalSize;
        } else if any(n, &["no-result-error", "no-results-error", "error-on-no-results"]) {
            a.no_result_error = true;
        } else if any(n, &["q", "search"]) {
            let v = cur.value(&arg)?;
            words.push(v);
        } else if any(n, &["q*", "search*", "s*"]) {
            let rest: Vec<String> = cur.it.by_ref().collect();
            words.push(rest.join(" "));
            break;
        // ---- columns
        } else if any(n, &["columns", "set-columns"]) {
            columns = columns_list(&cur.value(&arg)?)?;
        } else if any(n, &["add-columns", "add-column", "add-col"]) {
            columns.extend(columns_list(&cur.value(&arg)?)?);
        } else if any(n, &["remove-columns", "remove-column", "remove-col"]) {
            let rm = columns_list(&cur.value(&arg)?)?;
            columns.retain(|c| !rm.contains(c));
        } else if let Some(c) = column_switch(n) {
            columns.push(c);
            save(&mut a, None);
        } else if let Some(c) = negated_column(n) {
            columns.retain(|&x| x != c);
            if c == Column::FullPath {
                a.export.no_default_filename = true;
            }
        } else if color_switch(n) {
            cur.value(&arg)?;
            a.ignored.push(arg.clone());
        } else if let Some(c) = width_switch(n) {
            let v = cur.value(&arg)?;
            let w: usize = int(&arg, &v)?;
            a.export.widths.retain(|x| x.0 != c);
            a.export.widths.push((c, w.min(65535)));
        } else if VALUE_IGNORED.iter().any(|p| option_eq(n, p)) {
            cur.value(&arg)?;
            a.ignored.push(arg.clone());
        } else if FLAG_IGNORED.iter().any(|p| option_eq(n, p)) {
            a.ignored.push(arg.clone());
        } else if JOURNAL.iter().any(|p| option_eq(n, p)) {
            return Err(ArgError::Unsupported(arg));
        // ---- Fennec extensions
        } else if option_eq(n, "offline") {
            a.offline = true;
        } else if option_eq(n, "file-list") {
            a.file_lists.push(PathBuf::from(cur.value(&arg)?));
        } else if option_eq(n, "db") {
            a.db = Some(PathBuf::from(cur.value(&arg)?));
        // ---- DIR style attributes (`/aRH-S`), only with more letters after `a`
        } else if n.len() > 1 && n.starts_with(['a', 'A']) && n.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
            attributes(&mut a, &n[1..], &arg)?;
        } else if body.chars().all(|c| c == '-') {
            words.push(arg.clone());
        } else if slash {
            // Unknown `/word`: search text (`/downloads`).
            words.push(quote_word(&arg));
        } else {
            return Err(ArgError::Unknown(arg));
        }
        if cur.pending.is_some() {
            // `-flag=value` on a switch that takes no value.
            return Err(ArgError::BadValue(name.clone(), cur.pending.take().unwrap_or_default()));
        }
    }
    for w in words {
        push_term(&mut a.search, &w);
    }
    a.export.columns = columns;
    Ok(a)
}

fn sort_switch(n: &str) -> Option<(Column, Option<bool>)> {
    let l = n.to_ascii_lowercase();
    let rest = l.strip_prefix("sort-").or_else(|| l.strip_prefix("sort"))?;
    parse_sort(rest)
}

fn negated_column(n: &str) -> Option<Column> {
    let l = n.to_ascii_lowercase();
    let rest = l.strip_prefix("no-").or_else(|| l.strip_prefix("no"))?;
    column_switch(rest)
}

fn dir_sort(n: &str) -> Option<(Column, Option<bool>)> {
    let l = n.to_ascii_lowercase();
    let rest = l.strip_prefix('o')?;
    let (desc, key) = match rest.strip_prefix('-') {
        Some(k) => (true, k),
        None => (false, rest),
    };
    let col = match key {
        "n" => Column::Name,
        "s" => Column::Size,
        "e" => Column::Extension,
        "d" => Column::DateModified,
        _ => return None,
    };
    // DIR: /os = smallest first, /o-s = largest first.
    Some((col, Some(!desc)))
}

fn attributes(a: &mut Args, spec: &str, arg: &str) -> Result<(), ArgError> {
    let mut on = String::new();
    let mut off = String::new();
    let mut chars = spec.chars();
    while let Some(c) = chars.next() {
        let (target, ch) = if c == '-' {
            match chars.next() {
                Some(x) => (&mut off, x),
                None => return Err(ArgError::BadValue(arg.to_string(), spec.to_string())),
            }
        } else {
            (&mut on, c)
        };
        if !ch.is_ascii_alphabetic() {
            return Err(ArgError::BadValue(arg.to_string(), format!("unknown attribute {ch}")));
        }
        target.push(ch.to_ascii_lowercase());
    }
    if !on.is_empty() {
        push_term(&mut a.search, &format!("attrib:{on}"));
    }
    if !off.is_empty() {
        push_term(&mut a.search, &format!("!attrib:{off}"));
    }
    Ok(())
}

/// Help text.
pub const HELP: &str = "\
fes - Fennec command-line search (es.exe 1.1 compatible)

Usage: fes [options] [search text]

Search:   -r/-regex <re>  -i/-case  -w/-ww/-whole-word  -p/-match-path  -a/-diacritics
          -prefix -suffix -ignore-punctuation -ignore-white-space
          -path <path>  -parent-path <path>  -parent <path>  /ad  /a-d  /a[RHSDA...]
Results:  -o/-offset <n>  -n/-max-results/-count <n>
Sort:     -s  -sort <name|path|size|extension|date-created|date-modified|date-accessed|
          attributes|file-list-filename|run-count|date-recently-changed|date-run>[-ascending|-descending]
          -sort-<name>[-ascending|-descending]  -sort-ascending  -sort-descending  /on /o-s ...
Columns:  -name -path-column -full-path-and-name -extension -size -date-created -date-modified
          -date-accessed -attributes -file-list-file-name -run-count -date-run
          -date-recently-changed -type  (-no-<column> removes, -columns <a;b> sets)
Output:   -csv -efu -txt -m3u -m3u8 -tsv -json  -export-<format> <file>
          -no-header -double-quote -size-format <0..4> -date-format <0..6> -no-digit-grouping
          -size-leading-zero -highlight -highlight-color <n> -utf8-bom -lf -crlf -nul
Info:     -get-result-count -get-total-size -get-folder-size <folder> -get-everything-version
          -get-run-count <file> -inc-run-count <file> -set-run-count <file> <n>
Control:  -instance <name> -timeout <ms> -exit -reindex -update -save-db -no-result-error
          -save-settings -clear-settings -version -help
Fennec:   -offline (query index.fnx without a running instance)  -db <index.fnx>
          -filelist <file.efu> (query file lists directly)

Exit codes: 0 success, 1 Fennec not running, 2 bad arguments, 3 timeout,
            5 cannot create export file, 7 query failed, 9 no results (-no-result-error).
";

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Args {
        parse_args(args.iter().map(|s| s.to_string())).unwrap()
    }

    fn err(args: &[&str]) -> ArgError {
        parse_args(args.iter().map(|s| s.to_string())).unwrap_err()
    }

    #[test]
    fn option_matching() {
        assert!(option_eq("max-results", "max-results"));
        assert!(option_eq("maxresults", "max-results"));
        assert!(option_eq("MAXRESULTS", "max-results"));
        assert!(!option_eq("max--results", "max-results"));
        assert!(!option_eq("a-d", "ad"));
        assert!(option_eq("a-d", "a-d"));
        assert!(option_eq("ad", "a-d"));
        assert!(!option_eq("sizes", "size"));
    }

    #[test]
    fn int02_example() {
        let a = p(&["-n", "5", "-sort", "size-descending", "*.iso", "-csv"]);
        assert_eq!(a.max, Some(5));
        assert_eq!(a.export.format, Format::Csv);
        assert_eq!(a.search, "*.iso");
        assert_eq!(a.effective_sort(), (Column::Size, false));
    }

    #[test]
    fn search_switches() {
        let a = p(&["-i", "-w", "-p", "-a", "-r", "^a.*b$", "foo bar", "baz"]);
        assert!(a.case && a.whole_word && a.match_path && a.diacritics);
        assert_eq!(a.search, "regex:^a.*b$ \"foo bar\" baz");
        let a = p(&["-case", "-no-case", "-ww", "-match-path", "-diacritics"]);
        assert!(!a.case && a.whole_word && a.match_path && a.diacritics);
        let a = p(&["-regex", "x", "--", "-n", "5"]);
        assert_eq!(a.search, "regex:x -n 5");
        assert_eq!(a.max, None);
        let a = p(&["-q", "-weird", "-search*", "a", "-b"]);
        assert_eq!(a.search, "-weird a -b");
        let a = p(&["/downloads"]);
        assert_eq!(a.search, "/downloads");
        let a = p(&["-prefix", "-suffix", "-ignore-punctuation", "-ignore-ws"]);
        assert!(a.prefix && a.suffix && a.ignore_punctuation && a.ignore_whitespace);
    }

    #[test]
    fn sorts() {
        assert_eq!(p(&["-s"]).effective_sort(), (Column::Path, true));
        assert_eq!(p(&["-sort", "size"]).effective_sort(), (Column::Size, false), "size defaults descending");
        assert_eq!(p(&["-sort", "name"]).effective_sort(), (Column::Name, true));
        assert_eq!(p(&["-sort", "dm-ascending"]).effective_sort(), (Column::DateModified, true));
        assert_eq!(p(&["-sort", "date-modified", "-sort-ascending"]).effective_sort(), (Column::DateModified, true));
        assert_eq!(p(&["-sort-size-ascending"]).effective_sort(), (Column::Size, true));
        assert_eq!(p(&["-sort-path"]).effective_sort(), (Column::Path, true));
        assert_eq!(p(&["-sortextension"]).effective_sort(), (Column::Extension, true));
        assert_eq!(p(&["-sort=run-count-descending"]).effective_sort(), (Column::RunCount, false));
        assert_eq!(p(&["/o-s"]).effective_sort(), (Column::Size, false));
        assert_eq!(p(&["/on"]).effective_sort(), (Column::Name, true));
        for c in [
            "name",
            "path",
            "size",
            "ext",
            "extension",
            "date-modified",
            "date-created",
            "date-accessed",
            "attributes",
            "run-count",
            "date-run",
            "date-recently-changed",
            "file-list-filename",
        ] {
            assert!(parse_sort(c).is_some(), "{c}");
            assert!(parse_sort(&format!("{c}-descending")).is_some(), "{c}");
        }
        assert!(matches!(err(&["-sort", "bogus"]), ArgError::BadValue(..)));
        assert!(matches!(err(&["-sort"]), ArgError::MissingValue(..)));
    }

    #[test]
    fn columns_and_formats() {
        let a = p(&[
            "-name",
            "-path-column",
            "-full-path-and-name",
            "-ext",
            "-size",
            "-dc",
            "-dm",
            "-da",
            "-attribs",
            "-file-list-file-name",
            "-run-count",
            "-date-run",
            "-rc",
            "-no-size",
        ]);
        assert_eq!(
            a.export.columns,
            [
                Column::Name,
                Column::Path,
                Column::FullPath,
                Column::Extension,
                Column::DateCreated,
                Column::DateModified,
                Column::DateAccessed,
                Column::Attributes,
                Column::FileListName,
                Column::RunCount,
                Column::DateRun,
                Column::DateRecentlyChanged
            ]
        );
        for (s, f) in [
            ("-csv", Format::Csv),
            ("-efu", Format::Efu),
            ("-txt", Format::Txt),
            ("-m3u", Format::M3u),
            ("-m3u8", Format::M3u8),
            ("-tsv", Format::Tsv),
            ("-json", Format::Json),
        ] {
            assert_eq!(p(&[s]).export.format, f, "{s}");
            let e = p(&[&format!("-export{s}"), "out.x"]);
            assert_eq!((e.export.format, e.export_file), (f, Some(PathBuf::from("out.x"))));
        }
        let a = p(&["-no-header", "-double-quote", "-size-format", "0", "-date-format", "3", "-no-digit-grouping"]);
        assert_eq!(a.export.header, Some(false));
        assert!(a.export.double_quote && !a.export.digit_grouping);
        assert_eq!(a.export.size_format, SizeFormat::Auto);
        assert_eq!(a.export.date_format, DateFormat::IsoUtc);
        assert!(matches!(err(&["-size-format", "9"]), ArgError::BadValue(..)));
        assert_eq!(p(&["-columns", "size;name"]).export.columns, [Column::Size, Column::Name]);
        assert_eq!(p(&["-name-width", "10"]).export.widths, [(Column::Name, 10)]);
        assert!(p(&["-no-filename-column"]).export.no_default_filename);
    }

    #[test]
    fn info_and_control() {
        assert_eq!(p(&["-get-result-count", "abc"]).mode, Mode::ResultCount);
        assert_eq!(p(&["-get-total-size"]).mode, Mode::TotalSize);
        assert_eq!(p(&["-get-everything-version"]).mode, Mode::EverythingVersion);
        assert_eq!(p(&["-version"]).mode, Mode::Version);
        assert!(p(&["-debug-panic"]).debug_panic && !p(&["-debug"]).debug_panic);
        assert_eq!(p(&["-help"]).mode, Mode::Help);
        assert_eq!(p(&["/?"]).mode, Mode::Help);
        assert_eq!(p(&["-set-run-count", "C:\\a", "3"]).mode, Mode::SetRunCount("C:\\a".into(), 3));
        assert_eq!(p(&["-inc-run-count", "C:\\a"]).mode, Mode::IncRunCount("C:\\a".into()));
        let a = p(&["-instance", "work", "-timeout", "1500", "-exit", "-reindex", "-save-db", "-update"]);
        assert_eq!(a.instance, "work");
        assert_eq!(a.timeout_ms, Some(1500));
        assert_eq!(a.commands, [Command::Exit, Command::Rebuild, Command::SaveDb, Command::Update]);
        assert_eq!(p(&["-instance=1.5a"]).instance, "1.5a");
        assert_eq!(p(&["-o", "10", "-n", "0x10"]).offset, 10);
        assert_eq!(p(&["-o", "10", "-n", "0x10"]).max, Some(16));
        assert_eq!(p(&["-viewport-count", "3"]).max, Some(3));
        let a = p(&["-ipc2", "-pause", "-filename-color", "12", "-cp", "65001"]);
        assert_eq!(a.ignored.len(), 4);
        assert!(matches!(err(&["-journal"]), ArgError::Unsupported(..)));
        assert!(matches!(err(&["-bogus"]), ArgError::Unknown(..)));
        assert!(matches!(err(&["-n"]), ArgError::MissingValue(..)));
        assert!(matches!(err(&["-n", "x"]), ArgError::BadValue(..)));
        assert!(matches!(err(&["-case=1"]), ArgError::BadValue(..)));
        let a = p(&["-offline", "-db", "x.fnx", "-filelist", "a.efu", "-filelist", "b.efu"]);
        assert!(a.offline);
        assert_eq!(a.file_lists.len(), 2);
    }

    #[test]
    fn filters_and_attributes() {
        let a = p(&["/ad", "abc"]);
        assert_eq!(a.full_search(), "< folder: > < abc >");
        assert_eq!(p(&["/a-d", "x"]).filter, "file:");
        let a = p(&["/arh-s"]);
        assert_eq!(a.search, "attrib:rh !attrib:s");
        let base = if cfg!(windows) { "C:\\data" } else { "/data" };
        let a = p(&["-path", base, "x"]);
        let sep = if cfg!(windows) { '\\' } else { '/' };
        assert_eq!(a.filter, format!("\"{base}{sep}\""));
        let a = p(&["-parent", base]);
        assert_eq!(a.filter, format!("parent:\"{base}{sep}\""));
        let a = p(&["-parent-path", &format!("{base}{sep}sub")]);
        assert_eq!(a.filter, format!("\"{base}{sep}\""));
        // Unbalanced quote in the search is closed before the filter group ends.
        let a = p(&["/ad", "\"abc"]);
        assert_eq!(a.full_search(), "< folder: > < \"abc\" >");
    }
}

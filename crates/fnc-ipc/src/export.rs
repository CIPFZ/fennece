//! Result export in the `es.exe` formats (SPEC ch.4 §7): EFU, CSV, TSV, TXT, M3U, M3U8,
//! JSON and the plain console layout.
//!
//! Format rules (from voidtools ES 1.1):
//!
//! | format | separator | header | text quoting | sizes | dates (default) | folders |
//! |---|---|---|---|---|---|---|
//! | Plain | space, fixed widths | no | none (`-double-quote` wraps) | grouped | `YYYY-MM-DD HH:MM:SS` | as is |
//! | CSV | `,` | yes | always `"…"`, `""` escapes | bytes | formatted | trailing `\` on full paths |
//! | TSV | tab | yes | only when needed | bytes | formatted | trailing `\` |
//! | EFU | `,` | yes | always | bytes | FILETIME | trailing `\` |
//! | TXT / M3U / M3U8 | — | no | none | — | — | trailing `\`; full path only |
//! | JSON | — | no | JSON | number | FILETIME number | trailing `\` |
//!
//! Lines end with CRLF. EFU always has the columns `Filename,Size,Date Modified,Date
//! Created,Attributes` (re-mountable as a file list, INT-07). JSON output is
//! `[{"filename":"…"},\r\n{…}]\r\n`; an empty result set is written as `[]` (es writes
//! nothing).

use std::io::{self, Write};

use fnc_core::EntryId;
use fnc_exec::Caches;
use fnc_index::IndexStore;

use crate::format::{
    DateFormat, SizeFormat, format_attributes, format_filetime, format_number, format_size, parse_highlight,
    strip_highlight,
};
use crate::proto::{Column, Results, UNKNOWN};
use crate::rows::{ColumnSet, Row, entry_row};

/// Output format.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Format {
    /// es console layout (aligned columns).
    #[default]
    Plain,
    /// Comma separated values.
    Csv,
    /// Tab separated values.
    Tsv,
    /// Everything file list.
    Efu,
    /// One full path per line.
    Txt,
    /// Playlist (full paths).
    M3u,
    /// UTF-8 playlist (full paths).
    M3u8,
    /// JSON array of objects.
    Json,
}

impl Format {
    /// From a file extension or switch name (`csv`, `efu`, `txt`, `m3u`, `m3u8`, `tsv`, `json`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "csv" => Format::Csv,
            "tsv" => Format::Tsv,
            "efu" => Format::Efu,
            "txt" => Format::Txt,
            "m3u" => Format::M3u,
            "m3u8" => Format::M3u8,
            "json" => Format::Json,
            _ => return None,
        })
    }
}

/// Export options (es switches in parentheses).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportOptions {
    /// Format (`-csv`, `-efu`, ...).
    pub format: Format,
    /// Columns in display order (`-name`, `-size`, ...); resolved with [`resolve_columns`].
    pub columns: Vec<Column>,
    /// Header line (`-header` / `-no-header`); `None` = format default.
    pub header: Option<bool>,
    /// Wrap text in double quotes (`-double-quote`).
    pub double_quote: bool,
    /// Size format for the plain layout (`-size-format`).
    pub size_format: SizeFormat,
    /// Date format (`-date-format`).
    pub date_format: DateFormat,
    /// Digit grouping in the plain layout (`-no-digit-grouping` turns it off).
    pub digit_grouping: bool,
    /// Pad sizes with zeros instead of spaces (`-size-leading-zero`, needs no grouping).
    pub size_leading_zero: bool,
    /// Pad run counts with zeros (`-run-count-leading-zero`).
    pub run_count_leading_zero: bool,
    /// Append a separator to folder full paths; `None` = format default.
    pub folder_append_separator: Option<bool>,
    /// Write a UTF-8 BOM first (`-utf8-bom`).
    pub utf8_bom: bool,
    /// Plain layout: render `*` highlight markup with this console colour (es
    /// `-highlight-color`, Windows attribute byte) as ANSI escapes; `None` = print the
    /// markup as is.
    pub ansi_highlight: Option<u8>,
    /// Column width overrides (`-name-width`, ...).
    pub widths: Vec<(Column, usize)>,
    /// Do not add the default "Filename" column.
    pub no_default_filename: bool,
    /// Line terminator; `None` = CRLF.
    pub newline: Option<Newline>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: Format::Plain,
            columns: Vec::new(),
            header: None,
            double_quote: false,
            size_format: SizeFormat::Bytes,
            date_format: DateFormat::Default,
            digit_grouping: true,
            size_leading_zero: false,
            run_count_leading_zero: false,
            folder_append_separator: None,
            utf8_bom: false,
            ansi_highlight: None,
            widths: Vec::new(),
            no_default_filename: false,
            newline: None,
        }
    }
}

impl ExportOptions {
    /// Options for `format` with default columns.
    pub fn new(format: Format) -> Self {
        Self { format, ..Self::default() }
    }
}

/// Default es column width (characters) of `c` in the plain layout.
pub fn default_width(c: Column) -> usize {
    match c {
        Column::Name | Column::Type | Column::FileListName => 30,
        Column::Path | Column::FullPath => 47,
        Column::Size => 14,
        Column::Extension | Column::Attributes => 4,
        Column::DateModified
        | Column::DateCreated
        | Column::DateAccessed
        | Column::DateRun
        | Column::DateRecentlyChanged => 19,
        Column::RunCount => 7,
    }
}

fn right_aligned(c: Column) -> bool {
    matches!(c, Column::Size | Column::RunCount)
}

/// Effective columns for `opts` (es rules): TXT/M3U list the full path only; EFU drops
/// name/path and always carries `Filename,Size,Date Modified,Date Created,Attributes`; any
/// other format gets a "Filename" column when no name/path column was chosen (first for
/// exports, last for the plain layout). Duplicates are removed.
pub fn resolve_columns(opts: &ExportOptions) -> Vec<Column> {
    let mut cols: Vec<Column> = Vec::new();
    for &c in &opts.columns {
        if !cols.contains(&c) {
            cols.push(c);
        }
    }
    match opts.format {
        Format::Txt | Format::M3u | Format::M3u8 => vec![Column::FullPath],
        Format::Efu => {
            cols.retain(|c| !matches!(c, Column::Name | Column::Path | Column::FullPath));
            let std = [Column::FullPath, Column::Size, Column::DateModified, Column::DateCreated, Column::Attributes];
            cols.retain(|c| !std.contains(c));
            std.into_iter().chain(cols).collect()
        }
        _ => {
            let has_text = cols.iter().any(|c| matches!(c, Column::Name | Column::Path | Column::FullPath));
            if !has_text && !opts.no_default_filename {
                if opts.format == Format::Plain {
                    cols.push(Column::FullPath);
                } else {
                    cols.insert(0, Column::FullPath);
                }
            }
            cols
        }
    }
}

/// Line terminator (es `-crlf`, `-lf`, `-nul`, `-no-new-line`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Newline {
    /// `\r\n` (default).
    #[default]
    CrLf,
    /// `\n`.
    Lf,
    /// NUL character.
    Nul,
    /// Nothing.
    None,
}

impl Newline {
    /// The terminator text.
    pub fn as_str(self) -> &'static str {
        match self {
            Newline::CrLf => "\r\n",
            Newline::Lf => "\n",
            Newline::Nul => "\0",
            Newline::None => "",
        }
    }
}

/// Streaming exporter.
pub struct Exporter<W: Write> {
    out: W,
    opts: ExportOptions,
    cols: Vec<Column>,
    rows: u64,
    append_sep: bool,
    line: String,
    overflow: usize,
}

fn csv_quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

fn json_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// ANSI SGR sequence for a Windows console attribute byte (low nibble foreground, high
/// nibble background; bit 0 blue, 1 green, 2 red, 3 intensity).
pub fn ansi_color(attr: u8) -> String {
    let rgb =
        |n: u8| (if n & 4 != 0 { 1 } else { 0 }) | (if n & 2 != 0 { 2 } else { 0 }) | (if n & 1 != 0 { 4 } else { 0 });
    let fg = attr & 0x0f;
    let bg = attr >> 4;
    let fg_code = if fg & 8 != 0 { 90 + rgb(fg) } else { 30 + rgb(fg) };
    if bg == 0 {
        format!("\x1b[{fg_code}m")
    } else {
        let bg_code = if bg & 8 != 0 { 100 + rgb(bg) } else { 40 + rgb(bg) };
        format!("\x1b[{fg_code};{bg_code}m")
    }
}

impl<W: Write> Exporter<W> {
    /// Starts an export: writes the BOM and the header line.
    pub fn new(mut out: W, opts: ExportOptions) -> io::Result<Self> {
        let cols = resolve_columns(&opts);
        let header = opts.header.unwrap_or(matches!(opts.format, Format::Csv | Format::Tsv | Format::Efu));
        let header = header && !matches!(opts.format, Format::Json | Format::M3u | Format::M3u8);
        let append_sep = opts.folder_append_separator.unwrap_or(opts.format != Format::Plain);
        if opts.utf8_bom {
            out.write_all(&[0xEF, 0xBB, 0xBF])?;
        }
        let mut e = Self { out, opts, cols, rows: 0, append_sep, line: String::with_capacity(512), overflow: 0 };
        if header {
            e.write_header()?;
        }
        Ok(e)
    }

    /// Effective columns.
    pub fn columns(&self) -> &[Column] {
        &self.cols
    }

    /// Text columns the rows must carry ([`ColumnSet`] for [`entry_row`]).
    pub fn needed(&self) -> ColumnSet {
        ColumnSet::of(&self.cols)
    }

    fn separator(&self) -> &'static str {
        match self.opts.format {
            Format::Tsv => "\t",
            Format::Plain => " ",
            Format::Txt | Format::M3u | Format::M3u8 => "",
            _ => ",",
        }
    }

    fn width(&self, c: Column) -> usize {
        self.opts.widths.iter().find(|w| w.0 == c).map_or_else(|| default_width(c), |w| w.1)
    }

    fn write_header(&mut self) -> io::Result<()> {
        self.line.clear();
        self.overflow = 0;
        let cols = self.cols.clone();
        for (i, &c) in cols.iter().enumerate() {
            if i > 0 {
                self.line.push_str(self.separator());
            }
            if self.opts.format == Format::Plain {
                self.plain_cell(c, c.header(), c.header().chars().count(), i + 1 == cols.len(), false);
            } else {
                self.line.push_str(c.header());
            }
        }
        self.line.push_str(self.newline());
        self.out.write_all(self.line.as_bytes())
    }

    /// Pads/aligns a plain-layout cell (es fill + overflow rules).
    fn plain_cell(&mut self, c: Column, text: &str, visible_len: usize, last: bool, in_row: bool) {
        let width = self.width(c);
        let fill = |len: usize, overflow: &mut usize| -> usize {
            let mut n = width.saturating_sub(len);
            if *overflow > 0 {
                if n > *overflow {
                    n -= *overflow;
                    *overflow = 0;
                } else {
                    *overflow -= n;
                    n = 0;
                }
            }
            n
        };
        if !last && right_aligned(c) && visible_len < width {
            let n = fill(visible_len, &mut self.overflow);
            let zero = in_row
                && !self.opts.digit_grouping
                && ((c == Column::Size && self.opts.size_leading_zero)
                    || (c == Column::RunCount && self.opts.run_count_leading_zero));
            self.line.extend(std::iter::repeat_n(if zero { '0' } else { ' ' }, n));
        }
        self.line.push_str(text);
        if !last {
            if !right_aligned(c) && visible_len < width {
                let n = fill(visible_len, &mut self.overflow);
                self.line.extend(std::iter::repeat_n(' ', n));
            }
            if visible_len > width {
                self.overflow += visible_len - width;
            }
        }
    }

    fn text_cell(&mut self, c: Column, value: &str, markup: Option<&str>, last: bool) {
        match self.opts.format {
            Format::Csv => csv_quote(value, &mut self.line),
            Format::Efu => csv_quote(value, &mut self.line),
            Format::Tsv => {
                if self.opts.double_quote || value.contains(['\t', '"', '\r', '\n']) {
                    csv_quote(value, &mut self.line);
                } else {
                    self.line.push_str(value);
                }
            }
            Format::Json => {
                self.line.push('"');
                self.line.push_str(c.json_name());
                self.line.push_str("\":\"");
                json_escape(markup.unwrap_or(value), &mut self.line);
                self.line.push('"');
            }
            Format::Txt | Format::M3u | Format::M3u8 => {
                if self.opts.double_quote {
                    self.line.push('"');
                    self.line.push_str(value);
                    self.line.push('"');
                } else {
                    self.line.push_str(value);
                }
            }
            Format::Plain => {
                let q = if self.opts.double_quote { "\"" } else { "" };
                match (markup, self.opts.ansi_highlight) {
                    (Some(m), Some(color)) => {
                        let mut text = String::from(q);
                        let on = ansi_color(color);
                        for (seg, hl) in parse_highlight(m) {
                            if hl {
                                text.push_str(&on);
                                text.push_str(&seg);
                                text.push_str("\x1b[0m");
                            } else {
                                text.push_str(&seg);
                            }
                        }
                        text.push_str(q);
                        let len = strip_highlight(m).chars().count() + 2 * q.len();
                        self.plain_cell(c, &text, len, last, true);
                    }
                    (Some(m), None) => {
                        let text = format!("{q}{m}{q}");
                        let len = strip_highlight(m).chars().count() + 2 * q.len();
                        self.plain_cell(c, &text, len, last, true);
                    }
                    (None, _) => {
                        let text = format!("{q}{value}{q}");
                        let len = text.chars().count();
                        self.plain_cell(c, &text, len, last, true);
                    }
                }
            }
        }
    }

    fn raw_cell(&mut self, c: Column, text: &str, last: bool) {
        if self.opts.format == Format::Plain {
            let len = text.chars().count();
            self.plain_cell(c, text, len, last, true);
        } else {
            self.line.push_str(text);
        }
    }

    fn json_null(&mut self, c: Column) {
        self.line.push('"');
        self.line.push_str(c.json_name());
        self.line.push_str("\":null");
    }

    fn json_number(&mut self, c: Column, n: u64) {
        use std::fmt::Write as _;
        let _ = write!(self.line, "\"{}\":{n}", c.json_name());
    }

    fn date_cell(&mut self, c: Column, v: u64, last: bool) {
        let fmt = self.opts.date_format;
        if self.opts.format == Format::Json {
            if v == UNKNOWN {
                self.json_null(c);
            } else if !matches!(fmt, DateFormat::Default | DateFormat::Filetime) {
                let s = format_filetime(v, fmt);
                self.line.push('"');
                self.line.push_str(c.json_name());
                self.line.push_str("\":\"");
                self.line.push_str(&s);
                self.line.push('"');
            } else {
                self.json_number(c, v);
            }
            return;
        }
        let text = if v == UNKNOWN {
            String::new()
        } else if matches!(self.opts.format, Format::Plain | Format::Csv | Format::Tsv) || fmt != DateFormat::Default {
            format_filetime(v, fmt)
        } else {
            v.to_string()
        };
        self.raw_cell(c, &text, last);
    }

    /// Writes one row.
    pub fn write_row(&mut self, r: &Row) -> io::Result<()> {
        if self.opts.format == Format::Json {
            self.out.write_all(if self.rows == 0 { b"[" } else { b",\r\n" })?;
        }
        self.line.clear();
        self.overflow = 0;
        if self.opts.format == Format::Json {
            self.line.push('{');
        }
        let cols = self.cols.clone();
        let n = cols.len();
        let folder = r.is_folder();
        for (i, &c) in cols.iter().enumerate() {
            let last = i + 1 == n;
            if i > 0 {
                self.line.push_str(self.separator());
            }
            match c {
                Column::Name => self.text_cell(c, &r.name, r.highlighted_name.as_deref(), last),
                Column::Path => self.text_cell(c, &r.path, r.highlighted_path.as_deref(), last),
                Column::FullPath => {
                    if self.append_sep && folder && !r.full_path.ends_with(['\\', '/']) && !r.full_path.is_empty() {
                        let sep = if r.full_path.starts_with('/') { '/' } else { '\\' };
                        let v = format!("{}{sep}", r.full_path);
                        let m = r.highlighted_full_path.as_ref().map(|m| format!("{m}{sep}"));
                        self.text_cell(c, &v, m.as_deref(), last);
                    } else {
                        self.text_cell(c, &r.full_path, r.highlighted_full_path.as_deref(), last);
                    }
                }
                Column::Extension | Column::Type | Column::FileListName => {
                    let v = match c {
                        Column::Extension => &r.extension,
                        Column::Type => &r.type_name,
                        _ => &r.file_list_name,
                    };
                    if c == Column::Extension && folder {
                        if self.opts.format == Format::Json {
                            self.json_null(c);
                        } else {
                            self.raw_cell(c, "", last);
                        }
                    } else {
                        let v = v.clone();
                        self.text_cell(c, &v, None, last);
                    }
                }
                Column::Size => {
                    if self.opts.format == Format::Json {
                        if r.size == UNKNOWN { self.json_null(c) } else { self.json_number(c, r.size) }
                    } else {
                        let text = if r.size == UNKNOWN {
                            String::new()
                        } else if self.opts.format == Format::Plain {
                            format_size(r.size, self.opts.size_format, self.opts.digit_grouping)
                        } else {
                            r.size.to_string()
                        };
                        self.raw_cell(c, &text, last);
                    }
                }
                Column::DateModified => self.date_cell(c, r.date_modified, last),
                Column::DateCreated => self.date_cell(c, r.date_created, last),
                Column::DateAccessed => self.date_cell(c, r.date_accessed, last),
                Column::DateRun => self.date_cell(c, r.date_run, last),
                Column::DateRecentlyChanged => self.date_cell(c, r.date_recently_changed, last),
                Column::Attributes => {
                    let a = if r.attributes == u32::MAX {
                        if folder { fnc_core::flags::attr::DIRECTORY } else { 0 }
                    } else {
                        r.attributes
                    };
                    match self.opts.format {
                        Format::Json => self.json_number(c, u64::from(a)),
                        Format::Plain => self.raw_cell(c, &format_attributes(a), last),
                        _ => self.raw_cell(c, &a.to_string(), last),
                    }
                }
                Column::RunCount => match self.opts.format {
                    Format::Json => self.json_number(c, u64::from(r.run_count)),
                    Format::Plain => {
                        let t = format_number(u64::from(r.run_count), self.opts.digit_grouping);
                        self.raw_cell(c, &t, last)
                    }
                    _ => self.raw_cell(c, &r.run_count.to_string(), last),
                },
            }
        }
        if self.opts.format == Format::Json {
            self.line.push('}');
        } else {
            self.line.push_str(self.newline());
        }
        self.rows += 1;
        self.out.write_all(self.line.as_bytes())
    }

    /// Line terminator in use.
    pub fn newline(&self) -> &'static str {
        self.opts.newline.unwrap_or_default().as_str()
    }

    /// Rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// Writes a free-form line (es footer, messages) with the configured terminator.
    pub fn write_line(&mut self, text: &str) -> io::Result<()> {
        self.out.write_all(text.as_bytes())?;
        self.out.write_all(self.newline().as_bytes())
    }

    /// Finishes (closes the JSON array) and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        if self.opts.format == Format::Json {
            self.out.write_all(if self.rows == 0 { b"[]" } else { b"]" })?;
            self.out.write_all(self.newline().as_bytes())?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Exports entries of an index (the app's File → Export, Ctrl+S). Rows are built from the
/// snapshot `store`; `caches` supplies type names. Returns the number of rows.
pub fn export_entries<W: Write>(
    out: W,
    opts: &ExportOptions,
    store: &IndexStore,
    ids: &[EntryId],
    caches: &Caches,
) -> io::Result<u64> {
    let mut e = Exporter::new(out, opts.clone())?;
    let need = e.needed();
    for &id in ids {
        if store.is_live(id) {
            e.write_row(&entry_row(store, id, need, caches, None))?;
        }
    }
    let n = e.rows();
    e.finish()?;
    Ok(n)
}

/// Exports one page of IPC results.
pub fn export_results<W: Write>(out: W, opts: &ExportOptions, results: &Results) -> io::Result<u64> {
    let mut e = Exporter::new(out, opts.clone())?;
    for i in 0..results.len() {
        e.write_row(&results.row(i))?;
    }
    let n = e.rows();
    e.finish()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::FLAG_FOLDER;

    fn rows() -> Vec<Row> {
        vec![
            Row {
                flags: FLAG_FOLDER,
                name: "Docs".into(),
                path: "C:".into(),
                full_path: "C:\\Docs".into(),
                date_modified: 133_000_000_000_000_000,
                date_created: UNKNOWN,
                attributes: 16,
                ..Row::default()
            },
            Row {
                name: "a, \"b\".txt".into(),
                path: "C:\\Docs".into(),
                full_path: "C:\\Docs\\a, \"b\".txt".into(),
                extension: "txt".into(),
                size: 1234567,
                date_modified: 133_000_000_000_000_000,
                date_created: 132_000_000_000_000_000,
                attributes: 33,
                run_count: 3,
                ..Row::default()
            },
        ]
    }

    fn export(opts: ExportOptions) -> String {
        let mut e = Exporter::new(Vec::new(), opts).unwrap();
        for r in rows() {
            e.write_row(&r).unwrap();
        }
        String::from_utf8(e.finish().unwrap()).unwrap()
    }

    #[test]
    fn csv_quotes_text_always() {
        let s = export(ExportOptions { columns: vec![Column::Size], ..ExportOptions::new(Format::Csv) });
        assert_eq!(s, "Filename,Size\r\n\"C:\\Docs\\\",\r\n\"C:\\Docs\\a, \"\"b\"\".txt\",1234567\r\n");
        let s = export(ExportOptions { header: Some(false), ..ExportOptions::new(Format::Csv) });
        assert!(s.starts_with("\"C:\\Docs\\\"\r\n"));
    }

    #[test]
    fn tsv_quotes_only_when_needed() {
        let s =
            export(ExportOptions { columns: vec![Column::Name, Column::Extension], ..ExportOptions::new(Format::Tsv) });
        assert_eq!(s, "Name\tExtension\r\nDocs\t\r\n\"a, \"\"b\"\".txt\"\ttxt\r\n");
        let s = export(ExportOptions {
            columns: vec![Column::Name],
            double_quote: true,
            ..ExportOptions::new(Format::Tsv)
        });
        assert!(s.contains("\"Docs\""));
    }

    #[test]
    fn efu_layout() {
        let s =
            export(ExportOptions { columns: vec![Column::Name, Column::RunCount], ..ExportOptions::new(Format::Efu) });
        assert_eq!(
            s,
            "Filename,Size,Date Modified,Date Created,Attributes,Run Count\r\n\
             \"C:\\Docs\\\",,133000000000000000,,16,0\r\n\
             \"C:\\Docs\\a, \"\"b\"\".txt\",1234567,133000000000000000,132000000000000000,33,3\r\n"
        );
    }

    #[test]
    fn txt_m3u_json() {
        assert_eq!(
            export(ExportOptions { columns: vec![Column::Size], ..ExportOptions::new(Format::Txt) }),
            "C:\\Docs\\\r\nC:\\Docs\\a, \"b\".txt\r\n"
        );
        assert_eq!(export(ExportOptions::new(Format::M3u8)), "C:\\Docs\\\r\nC:\\Docs\\a, \"b\".txt\r\n");
        let j = export(ExportOptions {
            columns: vec![Column::Name, Column::Size, Column::DateCreated, Column::Extension],
            ..ExportOptions::new(Format::Json)
        });
        assert_eq!(
            j,
            "[{\"name\":\"Docs\",\"size\":null,\"date_created\":null,\"extension\":null},\r\n\
             {\"name\":\"a, \\\"b\\\".txt\",\"size\":1234567,\"date_created\":132000000000000000,\"extension\":\"txt\"}]\r\n"
        );
        let empty = Exporter::new(Vec::new(), ExportOptions::new(Format::Json)).unwrap().finish().unwrap();
        assert_eq!(empty, b"[]\r\n");
    }

    #[test]
    fn plain_alignment() {
        let s = export(ExportOptions {
            columns: vec![Column::Size, Column::Attributes],
            date_format: DateFormat::Filetime,
            ..ExportOptions::default()
        });
        assert_eq!(s, "               D    C:\\Docs\r\n     1,234,567 RA   C:\\Docs\\a, \"b\".txt\r\n");
        let s = export(ExportOptions {
            columns: vec![Column::Size],
            digit_grouping: false,
            size_leading_zero: true,
            ..ExportOptions::default()
        });
        assert!(s.contains("00000001234567 C:\\Docs\\a"), "{s}");
        let s = export(ExportOptions {
            columns: vec![Column::Size],
            size_format: SizeFormat::Auto,
            header: Some(true),
            ..ExportOptions::default()
        });
        assert!(s.starts_with("          Size Filename\r\n"), "{s}");
        assert!(s.contains("       1.17 MB C:"), "{s}");
    }

    #[test]
    fn plain_highlight() {
        let mut e = Exporter::new(Vec::new(), ExportOptions { ansi_highlight: Some(0x0A), ..ExportOptions::default() })
            .unwrap();
        let r = Row { full_path: "C:\\abc".into(), highlighted_full_path: Some("C:\\*ab*c".into()), ..Row::default() };
        e.write_row(&r).unwrap();
        assert_eq!(String::from_utf8(e.finish().unwrap()).unwrap(), "C:\\\x1b[92mab\x1b[0mc\r\n");
        assert_eq!(ansi_color(0x1C), "\x1b[91;44m");
    }

    #[test]
    fn efu_roundtrip_through_index() {
        // INT-07: export EFU, mount it again, same entries.
        let src = "Filename,Size,Date Modified,Date Created,Attributes\n\
                   \"C:\\Docs\",,,,16\n\
                   \"C:\\Docs\\a.txt\",10,133000000000000000,132000000000000000,32\n\
                   \"C:\\Docs\\Sub\\b, c.pdf\",20,133000000000000001,,33\n";
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, src.as_bytes(), "a.efu").unwrap();
        let ids: Vec<EntryId> = (0..s.len() as u32).map(EntryId).filter(|&i| s.is_live(i)).collect();
        let mut out = Vec::new();
        export_entries(&mut out, &ExportOptions::new(Format::Efu), &s, &ids, &Caches::new()).unwrap();
        let mut t = IndexStore::default();
        fnc_index::efu::load_efu(&mut t, &out[..], "b.efu").unwrap();
        let dump = |s: &IndexStore| {
            let mut v: Vec<(String, Option<u64>, i64, i64, bool)> = (0..s.len() as u32)
                .map(EntryId)
                .filter(|&i| s.is_live(i))
                .map(|i| {
                    let mut p = String::new();
                    s.full_path(i, &mut p);
                    (p, s.size(i).filter(|_| !s.is_dir(i)), s.date_modified(i).0, s.date_created(i).0, s.is_dir(i))
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(dump(&s), dump(&t));
        assert!(dump(&t).len() >= 4);
    }
}

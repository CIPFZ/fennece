//! Wire format shared by the server and the client: command lines, replies, the
//! `SITE EVERYTHING` search state and its `QUERY` reply, sort names, directory listing
//! lines, passive-mode addresses and the welcome message.
//!
//! Formats follow voidtools' ETP/FTP server (`etp_server.c` 1.0.2.5, MIT) and what
//! Everything 1.4/1.5 clients send (captured on Windows: `USER`, `PASS`, `OPTS UTF8 ON`,
//! then `EVERYTHING <option> <value>` lines and `EVERYTHING QUERY`).

use std::net::{Ipv4Addr, SocketAddrV4};

use chrono::{Datelike, Timelike};
use fnc_core::FileTime;
use fnc_ipc::proto::{Column, Mods, UNKNOWN};

/// Longest accepted command line (bytes, without CRLF).
pub const MAX_LINE: usize = 8 * 1024;

/// Splits `VERB param` at the first ASCII white space. Everything keeps every further
/// space in the parameter (file names may start with spaces); a trailing CR is removed.
pub fn split_command(line: &str) -> (&str, &str) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    match line.find(|c: char| c.is_ascii_whitespace()) {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    }
}

/// One reply: a code and its text lines (the first line without the code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// Three-digit code.
    pub code: u16,
    /// Text of every line (continuation lines as sent, without CRLF).
    pub lines: Vec<String>,
}

impl Reply {
    /// Text of the first line.
    pub fn text(&self) -> &str {
        self.lines.first().map_or("", String::as_str)
    }

    /// 1xx.
    pub fn is_preliminary(&self) -> bool {
        (100..200).contains(&self.code)
    }

    /// 2xx.
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.code)
    }
}

/// Incremental reply parser (RFC 959 multi-line replies: `ddd-` … `ddd `).
#[derive(Debug, Default)]
pub struct ReplyParser {
    code: Option<u16>,
    lines: Vec<String>,
}

impl ReplyParser {
    /// Feeds one line (without CRLF); returns the reply once it is complete. Lines that
    /// are not part of a reply are an error.
    pub fn push(&mut self, line: &str) -> Result<Option<Reply>, String> {
        let head = |l: &str| -> Option<(u16, u8)> {
            let b = l.as_bytes();
            if b.len() >= 3 && b[..3].iter().all(u8::is_ascii_digit) {
                let code = u16::from(b[0] - b'0') * 100 + u16::from(b[1] - b'0') * 10 + u16::from(b[2] - b'0');
                Some((code, b.get(3).copied().unwrap_or(b' ')))
            } else {
                None
            }
        };
        match self.code {
            None => match head(line) {
                Some((code, b'-')) => {
                    self.code = Some(code);
                    self.lines.push(line.get(4..).unwrap_or("").to_string());
                    Ok(None)
                }
                Some((code, _)) => Ok(Some(Reply { code, lines: vec![line.get(4..).unwrap_or("").to_string()] })),
                None => Err(format!("unexpected line: {line}")),
            },
            Some(code) => match head(line) {
                Some((c, b' ')) if c == code => {
                    self.lines.push(line.get(4..).unwrap_or("").to_string());
                    self.code = None;
                    Ok(Some(Reply { code, lines: std::mem::take(&mut self.lines) }))
                }
                _ => {
                    self.lines.push(line.to_string());
                    Ok(None)
                }
            },
        }
    }
}

/// `SITE EVERYTHING SORT` names (ascending flag). `INVERSE_SIZE_*` are accepted as in
/// Everything; `DATE_RECENTLY_CHANGED_*` is the 1.5 extension.
pub const SORT_NAMES: &[(&str, Column, bool)] = &[
    ("NAME_ASCENDING", Column::Name, true),
    ("NAME_DESCENDING", Column::Name, false),
    ("PATH_ASCENDING", Column::Path, true),
    ("PATH_DESCENDING", Column::Path, false),
    ("SIZE_ASCENDING", Column::Size, true),
    ("SIZE_DESCENDING", Column::Size, false),
    ("INVERSE_SIZE_ASCENDING", Column::Size, false),
    ("INVERSE_SIZE_DESCENDING", Column::Size, true),
    ("EXTENSION_ASCENDING", Column::Extension, true),
    ("EXTENSION_DESCENDING", Column::Extension, false),
    ("DATE_CREATED_ASCENDING", Column::DateCreated, true),
    ("DATE_CREATED_DESCENDING", Column::DateCreated, false),
    ("DATE_MODIFIED_ASCENDING", Column::DateModified, true),
    ("DATE_MODIFIED_DESCENDING", Column::DateModified, false),
    ("ATTRIBUTES_ASCENDING", Column::Attributes, true),
    ("ATTRIBUTES_DESCENDING", Column::Attributes, false),
    ("FILE_LIST_FILENAME_ASCENDING", Column::FileListName, true),
    ("FILE_LIST_FILENAME_DESCENDING", Column::FileListName, false),
    ("DATE_RECENTLY_CHANGED_ASCENDING", Column::DateRecentlyChanged, true),
    ("DATE_RECENTLY_CHANGED_DESCENDING", Column::DateRecentlyChanged, false),
];

/// Column and direction of a sort name (case-insensitive).
pub fn sort_from_name(name: &str) -> Option<(Column, bool)> {
    SORT_NAMES.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name.trim())).map(|&(_, c, a)| (c, a))
}

/// Sort name of a column and direction (`None` if ETP has no such sort).
pub fn sort_name(col: Column, ascending: bool) -> Option<&'static str> {
    SORT_NAMES.iter().find(|(n, c, a)| *c == col && *a == ascending && !n.starts_with("INVERSE")).map(|&(n, _, _)| n)
}

/// Optional result fields (`SITE EVERYTHING <NAME>_COLUMN x`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResultFields {
    /// `PATH` (parent folder).
    pub path: bool,
    /// `SIZE`.
    pub size: bool,
    /// `DATE_MODIFIED` (FILETIME).
    pub date_modified: bool,
    /// `DATE_CREATED` (FILETIME).
    pub date_created: bool,
    /// `ATTRIBUTES` (Win32 attributes).
    pub attributes: bool,
    /// `FILE_LIST_FILENAME`.
    pub file_list: bool,
    /// `DATE_RECENTLY_CHANGED` (FILETIME).
    pub date_recently_changed: bool,
}

/// Search options of one connection; they persist between queries (Everything only
/// sends what changed).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SearchState {
    /// `SEARCH` text.
    pub search: String,
    /// Match options (`CASE`, `WHOLE_WORD`, `PATH`, `DIACRITICS`, `PREFIX`, `SUFFIX`,
    /// `IGNORE_PUNCTUATION`, `IGNORE_WHITESPACE`, `REGEX`).
    pub mods: Mods,
    /// `HIDE_EMPTY_SEARCH_RESULTS`.
    pub hide_empty: bool,
    /// `FILTER_SEARCH` (secondary search, ANDed).
    pub filter_search: String,
    /// `FILTER_*` options.
    pub filter_mods: Mods,
    /// `SORT` (default name ascending).
    pub sort: (Column, bool),
    /// `OFFSET`.
    pub offset: u32,
    /// `COUNT` (default all).
    pub count: u32,
    /// Fields included per result.
    pub fields: ResultFields,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            search: String::new(),
            mods: Mods::default(),
            hide_empty: false,
            filter_search: String::new(),
            filter_mods: Mods::default(),
            sort: (Column::Name, true),
            offset: 0,
            count: u32::MAX,
            fields: ResultFields::default(),
        }
    }
}

/// What an `EVERYTHING` sub-command asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EverythingAction {
    /// An option was set: send this reply.
    Reply(String),
    /// `QUERY`: run the search with the current state.
    Query,
}

/// Leading-number parse like Everything's `string_to_int` (garbage → 0).
fn leading_u64(s: &str) -> u64 {
    let s = s.trim_start();
    let digits: &str = &s[..s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len())];
    digits.parse::<u64>().unwrap_or(if digits.is_empty() { 0 } else { u64::MAX })
}

fn flag(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix('-').unwrap_or(t);
    leading_u64(t) != 0
}

impl SearchState {
    /// Applies `SITE EVERYTHING <sub> <param>` (or the bare `EVERYTHING` command Everything
    /// clients send). Unknown sub-commands give `500 Unknown Everything command.`.
    pub fn apply(&mut self, sub: &str, param: &str) -> EverythingAction {
        let on = flag(param);
        let b = |v: bool| u8::from(v);
        let reply = |s: String| EverythingAction::Reply(format!("200 {s}"));
        let sub = sub.to_ascii_uppercase();
        let m = &mut self.mods;
        let f = &mut self.filter_mods;
        match sub.as_str() {
            "CASE" => {
                m.case = on;
                reply(format!("Case set to ({}).", b(on)))
            }
            "WHOLE_WORD" => {
                m.whole_word = on;
                reply(format!("Whole word set to ({}).", b(on)))
            }
            "PATH" => {
                m.path = on;
                reply(format!("Path set to ({}).", b(on)))
            }
            "DIACRITICS" => {
                m.diacritics = on;
                reply(format!("Diacritics set to ({}).", b(on)))
            }
            "PREFIX" => {
                m.prefix = on;
                reply(format!("Prefix set to ({}).", b(on)))
            }
            "SUFFIX" => {
                m.suffix = on;
                reply(format!("Suffix set to ({}).", b(on)))
            }
            "IGNORE_PUNCTUATION" => {
                m.ignore_punctuation = on;
                reply(format!("Ignore punctuation set to ({}).", b(on)))
            }
            "IGNORE_WHITESPACE" => {
                m.ignore_whitespace = on;
                reply(format!("Ignore whitespace set to ({}).", b(on)))
            }
            "REGEX" => {
                m.regex = on;
                reply(format!("Regex set to ({}).", b(on)))
            }
            "HIDE_EMPTY_SEARCH_RESULTS" => {
                self.hide_empty = on;
                reply(format!("Hide empty search results set to ({}).", b(on)))
            }
            "SEARCH" => {
                self.search = param.to_string();
                reply(format!("Search set to ({}).", one_line(param)))
            }
            "FILTER_SEARCH" => {
                self.filter_search = param.to_string();
                reply(format!("Filter search set to ({}).", one_line(param)))
            }
            "FILTER_CASE" => {
                f.case = on;
                reply(format!("Filter case set to ({}).", b(on)))
            }
            "FILTER_WHOLE_WORD" => {
                f.whole_word = on;
                reply(format!("Filter whole word set to ({}).", b(on)))
            }
            "FILTER_PATH" => {
                f.path = on;
                reply(format!("Filter path set to ({}).", b(on)))
            }
            "FILTER_DIACRITICS" => {
                f.diacritics = on;
                reply(format!("Filter diacritics set to ({}).", b(on)))
            }
            "FILTER_PREFIX" => {
                f.prefix = on;
                reply(format!("Filter prefix set to ({}).", b(on)))
            }
            "FILTER_SUFFIX" => {
                f.suffix = on;
                reply(format!("Filter suffix set to ({}).", b(on)))
            }
            "FILTER_IGNORE_PUNCTUATION" => {
                f.ignore_punctuation = on;
                reply(format!("Filter ignore punctuation set to ({}).", b(on)))
            }
            "FILTER_IGNORE_WHITESPACE" => {
                f.ignore_whitespace = on;
                reply(format!("Filter ignore whitespace set to ({}).", b(on)))
            }
            "FILTER_REGEX" => {
                f.regex = on;
                reply(format!("Filter regex set to ({}).", b(on)))
            }
            "SORT" => match sort_from_name(param) {
                Some(s) => {
                    self.sort = s;
                    reply(format!("Sort set to ({}).", one_line(param.trim())))
                }
                None => EverythingAction::Reply("500 Unknown sort type.".into()),
            },
            "OFFSET" => {
                self.offset = leading_u64(param).min(u64::from(u32::MAX)) as u32;
                reply(format!("Offset set to ({}).", self.offset))
            }
            "COUNT" => {
                self.count = leading_u64(param).min(u64::from(u32::MAX)) as u32;
                reply(format!("Count set to ({}).", self.count))
            }
            "SIZE_COLUMN" => {
                self.fields.size = on;
                reply(format!("Size column set to ({}).", b(on)))
            }
            "ATTRIBUTES_COLUMN" => {
                self.fields.attributes = on;
                reply(format!("Attributes column set to ({}).", b(on)))
            }
            "DATE_MODIFIED_COLUMN" => {
                self.fields.date_modified = on;
                reply(format!("Date modified column set to ({}).", b(on)))
            }
            "DATE_CREATED_COLUMN" => {
                self.fields.date_created = on;
                reply(format!("Date created column set to ({}).", b(on)))
            }
            "PATH_COLUMN" => {
                self.fields.path = on;
                reply(format!("Path column set to ({}).", b(on)))
            }
            "FILE_LIST_FILENAME_COLUMN" => {
                self.fields.file_list = on;
                reply(format!("File list filename column set to ({}).", b(on)))
            }
            "DATE_RECENTLY_CHANGED_COLUMN" => {
                self.fields.date_recently_changed = on;
                reply(format!("Date recently changed column set to ({}).", b(on)))
            }
            "QUERY" => EverythingAction::Query,
            _ => EverythingAction::Reply("500 Unknown Everything command.".into()),
        }
    }

    /// The commands that bring a connection from `sent` to `self` (in the order the
    /// Everything client uses), without `QUERY`.
    pub fn commands_from(&self, sent: &SearchState) -> Vec<String> {
        let mut out = Vec::new();
        let mut put = |name: &str, a: bool, b: bool| {
            if a != b {
                out.push(format!("{name} {}", u8::from(a)));
            }
        };
        let (f, g) = (&self.fields, &sent.fields);
        put("SIZE_COLUMN", f.size, g.size);
        put("DATE_MODIFIED_COLUMN", f.date_modified, g.date_modified);
        put("DATE_CREATED_COLUMN", f.date_created, g.date_created);
        put("ATTRIBUTES_COLUMN", f.attributes, g.attributes);
        put("PATH_COLUMN", f.path, g.path);
        put("FILE_LIST_FILENAME_COLUMN", f.file_list, g.file_list);
        put("DATE_RECENTLY_CHANGED_COLUMN", f.date_recently_changed, g.date_recently_changed);
        let (m, n) = (&self.mods, &sent.mods);
        put("CASE", m.case, n.case);
        put("WHOLE_WORD", m.whole_word, n.whole_word);
        put("PATH", m.path, n.path);
        put("DIACRITICS", m.diacritics, n.diacritics);
        put("PREFIX", m.prefix, n.prefix);
        put("SUFFIX", m.suffix, n.suffix);
        put("IGNORE_PUNCTUATION", m.ignore_punctuation, n.ignore_punctuation);
        put("IGNORE_WHITESPACE", m.ignore_whitespace, n.ignore_whitespace);
        put("REGEX", m.regex, n.regex);
        put("HIDE_EMPTY_SEARCH_RESULTS", self.hide_empty, sent.hide_empty);
        let (m, n) = (&self.filter_mods, &sent.filter_mods);
        put("FILTER_CASE", m.case, n.case);
        put("FILTER_WHOLE_WORD", m.whole_word, n.whole_word);
        put("FILTER_PATH", m.path, n.path);
        put("FILTER_DIACRITICS", m.diacritics, n.diacritics);
        put("FILTER_PREFIX", m.prefix, n.prefix);
        put("FILTER_SUFFIX", m.suffix, n.suffix);
        put("FILTER_IGNORE_PUNCTUATION", m.ignore_punctuation, n.ignore_punctuation);
        put("FILTER_IGNORE_WHITESPACE", m.ignore_whitespace, n.ignore_whitespace);
        put("FILTER_REGEX", m.regex, n.regex);
        if self.count != sent.count {
            out.push(format!("COUNT {}", self.count));
        }
        if self.offset != sent.offset {
            out.push(format!("OFFSET {}", self.offset));
        }
        if self.sort != sent.sort
            && let Some(name) = sort_name(self.sort.0, self.sort.1)
        {
            out.push(format!("SORT {name}"));
        }
        if self.filter_search != sent.filter_search {
            out.push(format!("FILTER_SEARCH {}", one_line(&self.filter_search)));
        }
        if self.search != sent.search {
            out.push(format!("SEARCH {}", one_line(&self.search)));
        }
        out
    }
}

/// Replaces line breaks (they would end the command or reply line).
pub fn one_line(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\r', '\n']) { s.replace(['\r', '\n'], " ").into() } else { s.into() }
}

/// One `SITE EVERYTHING QUERY` result. Numbers use [`UNKNOWN`] (`u64::MAX`) when the
/// value is unknown, as Everything sends them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultItem {
    /// Folder (`FOLDER name`) or file (`FILE name`).
    pub folder: bool,
    /// Name.
    pub name: String,
    /// Parent path on the server (`PATH`).
    pub path: Option<String>,
    /// `SIZE`.
    pub size: Option<u64>,
    /// `DATE_MODIFIED`.
    pub date_modified: Option<u64>,
    /// `DATE_CREATED`.
    pub date_created: Option<u64>,
    /// `ATTRIBUTES`.
    pub attributes: Option<u32>,
    /// `FILE_LIST_FILENAME`.
    pub file_list: Option<String>,
    /// `DATE_RECENTLY_CHANGED`.
    pub date_recently_changed: Option<u64>,
}

impl ResultItem {
    /// A result with only a name.
    pub fn new(name: impl Into<String>, folder: bool) -> Self {
        Self {
            folder,
            name: name.into(),
            path: None,
            size: None,
            date_modified: None,
            date_created: None,
            attributes: None,
            file_list: None,
            date_recently_changed: None,
        }
    }

    /// Size if known.
    pub fn known_size(&self) -> Option<u64> {
        self.size.filter(|&s| s != UNKNOWN)
    }

    /// Full path on the server (`PATH` + separator + name; the name alone without a path).
    pub fn full_path(&self) -> String {
        match self.path.as_deref() {
            None | Some("") => self.name.clone(),
            Some(p) => fnc_ipc::rows::join_path(p, &self.name),
        }
    }
}

/// Appends the `200-Query results` … `200 End.` reply for `items` (`total` results).
pub fn write_query_reply(out: &mut String, total: u64, items: &[ResultItem]) {
    use std::fmt::Write;
    out.push_str("200-Query results\r\n");
    let _ = write!(out, " RESULT_COUNT {total}\r\n");
    for r in items {
        if let Some(p) = &r.path {
            let _ = write!(out, " PATH {}\r\n", one_line(p));
        }
        if let Some(a) = r.attributes {
            let _ = write!(out, " ATTRIBUTES {a}\r\n");
        }
        if let Some(s) = r.size {
            let _ = write!(out, " SIZE {s}\r\n");
        }
        if let Some(d) = r.date_modified {
            let _ = write!(out, " DATE_MODIFIED {d}\r\n");
        }
        if let Some(d) = r.date_created {
            let _ = write!(out, " DATE_CREATED {d}\r\n");
        }
        if let Some(f) = &r.file_list {
            let _ = write!(out, " FILE_LIST_FILENAME {}\r\n", one_line(f));
        }
        if let Some(d) = r.date_recently_changed {
            let _ = write!(out, " DATE_RECENTLY_CHANGED {d}\r\n");
        }
        let _ = write!(out, " {} {}\r\n", if r.folder { "FOLDER" } else { "FILE" }, one_line(&r.name));
    }
    out.push_str("200 End.\r\n");
}

/// A parsed `QUERY` reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryPage {
    /// `RESULT_COUNT`: all results of the search (not only this page).
    pub total: u64,
    /// The results of this page.
    pub items: Vec<ResultItem>,
}

/// Parses the lines of a `QUERY` reply (as returned by [`ReplyParser`]). Unknown keys
/// are ignored.
pub fn parse_query_reply(reply: &Reply) -> Result<QueryPage, String> {
    if reply.code != 200 {
        return Err(format!("{} {}", reply.code, reply.text()));
    }
    let mut page = QueryPage::default();
    let mut cur = ResultItem::new("", false);
    let num = |v: &str| v.trim().parse::<u64>().ok();
    for line in reply.lines.iter().skip(1) {
        let Some(body) = line.strip_prefix(' ') else { continue };
        let (key, value) = body.split_once(' ').unwrap_or((body, ""));
        match key {
            "RESULT_COUNT" => page.total = num(value).unwrap_or(0),
            "PATH" => cur.path = Some(value.to_string()),
            "SIZE" => cur.size = num(value),
            "DATE_MODIFIED" => cur.date_modified = num(value),
            "DATE_CREATED" => cur.date_created = num(value),
            "DATE_RECENTLY_CHANGED" => cur.date_recently_changed = num(value),
            "ATTRIBUTES" => cur.attributes = num(value).map(|a| a.min(u64::from(u32::MAX)) as u32),
            "FILE_LIST_FILENAME" => cur.file_list = Some(value.to_string()),
            "FILE" | "FOLDER" => {
                let mut item = std::mem::replace(&mut cur, ResultItem::new("", false));
                item.folder = key == "FOLDER";
                item.name = value.to_string();
                page.items.push(item);
            }
            _ => {}
        }
    }
    Ok(page)
}

/// `ls -l` style line of `LIST` (`drwxr-xr-x`-like permissions, owner `ftp`; date in local
/// time: `Mon dd HH:MM` within six months, else `Mon dd  YYYY`).
pub fn list_line(is_dir: bool, size: Option<u64>, modified: FileTime, name: &str, now: FileTime) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let local = modified.to_local();
    let (month, day, when) = match local {
        Some(t) => {
            let recent = now.0 >= modified.0 && now.0 - modified.0 < 180 * 24 * 3600 * 10_000_000;
            let when = if recent { format!("{:02}:{:02}", t.hour(), t.minute()) } else { format!("{:>5}", t.year()) };
            (MONTHS[(t.month0() as usize).min(11)], t.day(), when)
        }
        None => ("Jan", 1, " 1980".to_string()),
    };
    format!(
        "{}r--r--r-- 1 ftp ftp {:>13} {month} {day:02} {when} {}\r\n",
        if is_dir { 'd' } else { '-' },
        if is_dir { 0 } else { size.unwrap_or(0) },
        one_line(name)
    )
}

/// `YYYYMMDDHHMMSS` in UTC (MLSD/MLST `modify=`), `None` when unknown.
pub fn mlsx_time(t: FileTime) -> Option<String> {
    let u = t.to_utc()?;
    Some(format!("{:04}{:02}{:02}{:02}{:02}{:02}", u.year(), u.month(), u.day(), u.hour(), u.minute(), u.second()))
}

/// `213 YYYYMMDDHHMMSS.mmm` time of `MDTM` (UTC), `None` when unknown.
pub fn mdtm_time(t: FileTime) -> Option<String> {
    let u = t.to_utc()?;
    Some(format!("{}.{:03}", mlsx_time(t)?, u.timestamp_subsec_millis().min(999)))
}

/// MLSD/MLST facts: `type=dir;modify=…;size=…; name`.
pub fn mlsx_facts(is_dir: bool, size: Option<u64>, modified: FileTime, name: &str) -> String {
    let mut s = format!("type={};", if is_dir { "dir" } else { "file" });
    if let Some(m) = mlsx_time(modified) {
        s.push_str("modify=");
        s.push_str(&m);
        s.push(';');
    }
    if let Some(sz) = size.filter(|_| !is_dir) {
        s.push_str(&format!("size={sz};"));
    }
    s.push(' ');
    s.push_str(&one_line(name));
    s
}

/// `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2).`
pub fn pasv_reply(addr: SocketAddrV4) -> String {
    let [a, b, c, d] = addr.ip().octets();
    let p = addr.port();
    format!("227 Entering Passive Mode ({a},{b},{c},{d},{},{}).", p >> 8, p & 0xff)
}

/// Six comma-separated numbers (`PORT` argument, or the parenthesised part of a 227
/// reply) as an IPv4 socket address.
pub fn parse_host_port(s: &str) -> Option<SocketAddrV4> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let nums: Vec<u8> = s[start..]
        .split(',')
        .take(6)
        .map(|p| {
            let p = p.trim();
            let end = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
            p[..end].parse::<u8>().ok()
        })
        .collect::<Option<Vec<u8>>>()?;
    if nums.len() != 6 {
        return None;
    }
    Some(SocketAddrV4::new(
        Ipv4Addr::new(nums[0], nums[1], nums[2], nums[3]),
        u16::from(nums[4]) << 8 | u16::from(nums[5]),
    ))
}

/// Port of a `229 Entering Extended Passive Mode (|||port|)` reply.
pub fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let inner = &text[open + 1..text[open..].find(')').map(|i| open + i)?];
    let d = inner.chars().next()?;
    let parts: Vec<&str> = inner.split(d).collect();
    parts.get(3)?.trim().parse().ok()
}

/// `EPRT |proto|addr|port|` argument.
pub fn parse_eprt(s: &str) -> Option<std::net::SocketAddr> {
    let s = s.trim();
    let d = s.chars().next()?;
    let parts: Vec<&str> = s.split(d).collect();
    if parts.len() < 5 {
        return None;
    }
    let ip: std::net::IpAddr = parts[2].parse().ok()?;
    let port: u16 = parts[3].parse().ok()?;
    match (parts[1], ip) {
        ("1", std::net::IpAddr::V4(_)) | ("2", std::net::IpAddr::V6(_)) => Some(std::net::SocketAddr::new(ip, port)),
        _ => None,
    }
}

/// Default welcome text (Everything: `Welcome to Everything ETP/FTP`).
pub const DEFAULT_WELCOME: &str = "Welcome to Fennec ETP/FTP";

/// The `220` greeting for `message` (`%n` new line, `%v` version, `%%` percent; raw line
/// breaks are ignored as in Everything).
pub fn welcome(message: &str, version: &str) -> String {
    let message = if message.trim().is_empty() { DEFAULT_WELCOME } else { message };
    let mut lines = vec![String::new()];
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {}
            '%' => match chars.next() {
                Some('n') => lines.push(String::new()),
                Some('v') => lines.last_mut().into_iter().for_each(|l| l.push_str(version)),
                Some('%') => lines.last_mut().into_iter().for_each(|l| l.push('%')),
                _ => {}
            },
            c => lines.last_mut().into_iter().for_each(|l| l.push(c)),
        }
    }
    let n = lines.len();
    lines.iter().enumerate().map(|(i, l)| format!("220{}{l}\r\n", if i + 1 == n { ' ' } else { '-' })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_everything() {
        assert_eq!(split_command("USER bob\r"), ("USER", "bob"));
        assert_eq!(split_command("NOOP"), ("NOOP", ""));
        // Only the first space separates; names may start with spaces.
        assert_eq!(split_command("CWD  spaced"), ("CWD", " spaced"));
        assert_eq!(split_command("EVERYTHING SEARCH a b"), ("EVERYTHING", "SEARCH a b"));
    }

    #[test]
    fn multi_line_replies() {
        let mut p = ReplyParser::default();
        assert_eq!(p.push("220 hi").unwrap(), Some(Reply { code: 220, lines: vec!["hi".into()] }));
        assert_eq!(p.push("211-Features:").unwrap(), None);
        assert_eq!(p.push(" MDTM").unwrap(), None);
        // A different code inside a multi-line reply is text.
        assert_eq!(p.push("123 not the end").unwrap(), None);
        let r = p.push("211 End").unwrap().unwrap();
        assert_eq!(r.code, 211);
        assert_eq!(r.lines, ["Features:", " MDTM", "123 not the end", "End"]);
        assert!(p.push("garbage").is_err());
    }

    #[test]
    fn search_state_and_replies() {
        let mut s = SearchState::default();
        assert_eq!(s.apply("SEARCH", "abc *.txt"), EverythingAction::Reply("200 Search set to (abc *.txt).".into()));
        assert_eq!(s.search, "abc *.txt");
        assert_eq!(s.apply("case", "1"), EverythingAction::Reply("200 Case set to (1).".into()));
        assert!(s.mods.case);
        s.apply("CASE", "0");
        assert!(!s.mods.case);
        s.apply("COUNT", "26");
        s.apply("OFFSET", "100");
        assert_eq!((s.count, s.offset), (26, 100));
        assert_eq!(
            s.apply("SORT", "size_descending"),
            EverythingAction::Reply("200 Sort set to (size_descending).".into())
        );
        assert_eq!(s.sort, (Column::Size, false));
        assert_eq!(s.apply("SORT", "bogus"), EverythingAction::Reply("500 Unknown sort type.".into()));
        assert_eq!(s.sort, (Column::Size, false));
        s.apply("PATH_COLUMN", "1");
        s.apply("FILTER_REGEX", "7");
        assert!(s.fields.path && s.filter_mods.regex);
        assert_eq!(s.apply("QUERY", ""), EverythingAction::Query);
        assert_eq!(s.apply("FROB", "1"), EverythingAction::Reply("500 Unknown Everything command.".into()));
        // Garbage numbers are 0 like Everything's string_to_int.
        s.apply("COUNT", "x");
        assert_eq!(s.count, 0);
    }

    #[test]
    fn client_sends_only_changes() {
        let sent = SearchState::default();
        let mut want = sent.clone();
        assert!(want.commands_from(&sent).is_empty());
        want.search = "abc".into();
        want.count = 26;
        want.fields.size = true;
        want.mods.case = true;
        want.sort = (Column::DateModified, false);
        assert_eq!(
            want.commands_from(&sent),
            ["SIZE_COLUMN 1", "CASE 1", "COUNT 26", "SORT DATE_MODIFIED_DESCENDING", "SEARCH abc"]
        );
        // Applying the commands on the server reproduces the state.
        let mut server = SearchState::default();
        for c in want.commands_from(&sent) {
            let (sub, p) = split_command(&c);
            server.apply(sub, p);
        }
        assert_eq!(server, want);
    }

    #[test]
    fn sort_names() {
        assert_eq!(sort_from_name("INVERSE_SIZE_ASCENDING"), Some((Column::Size, false)));
        assert_eq!(sort_name(Column::Size, false), Some("SIZE_DESCENDING"));
        assert_eq!(sort_name(Column::Type, true), None);
        for (n, c, a) in SORT_NAMES {
            assert_eq!(sort_from_name(n), Some((*c, *a)));
        }
    }

    #[test]
    fn query_reply_round_trip() {
        let mut a = ResultItem::new("alpha.txt", false);
        a.path = Some("C:\\Data".into());
        a.size = Some(1234);
        a.date_modified = Some(133_000_000_000_000_000);
        let mut b = ResultItem::new("beta folder", true);
        b.path = Some("C:\\Data".into());
        b.size = Some(UNKNOWN);
        b.attributes = Some(16);
        let mut out = String::new();
        write_query_reply(&mut out, 7, &[a.clone(), b.clone()]);
        assert_eq!(
            out,
            "200-Query results\r\n RESULT_COUNT 7\r\n PATH C:\\Data\r\n SIZE 1234\r\n DATE_MODIFIED 133000000000000000\r\n \
             FILE alpha.txt\r\n PATH C:\\Data\r\n ATTRIBUTES 16\r\n SIZE 18446744073709551615\r\n FOLDER beta folder\r\n200 End.\r\n"
        );
        let mut p = ReplyParser::default();
        let mut reply = None;
        for l in out.split("\r\n").filter(|l| !l.is_empty()) {
            reply = p.push(l).unwrap();
        }
        let page = parse_query_reply(&reply.unwrap()).unwrap();
        assert_eq!(page.total, 7);
        assert_eq!(page.items, [a.clone(), b]);
        assert_eq!(a.full_path(), "C:\\Data\\alpha.txt");
        assert_eq!(page.items[1].known_size(), None);
    }

    #[test]
    fn list_and_mlsd_lines() {
        let now = FileTime::from_utc(chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap());
        let old = FileTime::from_utc(chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap());
        let l = list_line(false, Some(42), old, "a b.txt", now);
        assert!(l.starts_with("-r--r--r-- 1 ftp ftp            42 Sep "), "{l}");
        assert!(l.ends_with(" 2020 a b.txt\r\n"), "{l}");
        let recent = list_line(true, None, now, "dir", now);
        assert!(recent.starts_with("dr--r--r-- 1 ftp ftp             0 "), "{recent}");
        let when = recent.trim_end().rsplit(' ').nth(1).unwrap();
        assert!(when.len() == 5 && when.as_bytes()[2] == b':', "{recent}");
        assert_eq!(mlsx_facts(false, Some(5), old, "x"), "type=file;modify=20200913122640;size=5; x");
        assert_eq!(mlsx_facts(true, Some(5), FileTime::UNKNOWN, "d"), "type=dir; d");
        assert_eq!(mdtm_time(old).unwrap(), "20200913122640.000");
    }

    #[test]
    fn passive_addresses() {
        let a = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 7), 50_001);
        let r = pasv_reply(a);
        assert_eq!(r, "227 Entering Passive Mode (10,0,0,7,195,81).");
        assert_eq!(parse_host_port(&r[4..]), Some(a));
        // Everything's own wording has a period before the parenthesis.
        assert_eq!(parse_host_port("Entering Passive Mode. (10,0,0,7,195,81)"), Some(a));
        assert_eq!(parse_host_port("1,2,3"), None);
        assert_eq!(parse_epsv("Entering Extended Passive Mode (|||6446|)"), Some(6446));
        assert_eq!(parse_eprt("|1|132.235.1.2|6275|"), Some("132.235.1.2:6275".parse().unwrap()));
        assert_eq!(parse_eprt("|2|::1|6275|"), Some("[::1]:6275".parse().unwrap()));
        assert_eq!(parse_eprt("|1|::1|6275|"), None);
    }

    #[test]
    fn welcome_messages() {
        assert_eq!(welcome("", "1.0"), "220 Welcome to Fennec ETP/FTP\r\n");
        assert_eq!(welcome("Hi%nFennec %v 100%%\r\n", "0.1.0"), "220-Hi\r\n220 Fennec 0.1.0 100%\r\n");
    }
}

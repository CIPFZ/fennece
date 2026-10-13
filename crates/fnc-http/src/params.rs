//! Request parameters, text helpers and security primitives.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use fnc_ipc::proto::{Column, Mods};

/// Characters left unescaped in query components (`-_.~` per RFC 3986).
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
/// Characters left unescaped in path segments (also `:` for drive letters).
const SEGMENT: &AsciiSet = &COMPONENT.remove(b':');

/// Percent-encodes a query component.
pub fn encode_component(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// URL path for downloading `full_path` (`C:\a b\c` → `/C:/a%20b/c`).
pub fn download_url(full_path: &str) -> String {
    let mut out = String::with_capacity(full_path.len() + 8);
    for seg in full_path.split(['\\', '/']) {
        out.push('/');
        out.push_str(&utf8_percent_encode(seg, SEGMENT).to_string());
    }
    out
}

/// Decodes a query component (`+` is a space).
pub fn decode_component(s: &str) -> String {
    let plus = s.replace('+', " ");
    percent_decode_str(&plus).decode_utf8_lossy().into_owned()
}

/// Parses a query string into decoded `(key, value)` pairs.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (decode_component(k).to_ascii_lowercase(), decode_component(v)),
            None => (decode_component(kv).to_ascii_lowercase(), String::new()),
        })
        .collect()
}

/// HTML-escapes text for element content and quoted attributes.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// JSON string escaping (without the quotes).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Search request parameters (Everything HTTP server names; aliases in parentheses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchParams {
    /// `search` (`s`, `q`).
    pub search: String,
    /// `offset` (`o`).
    pub offset: u64,
    /// `count` (`c`); `None` = default (page size / all for JSON).
    pub count: Option<u64>,
    /// `json` (`j`).
    pub json: bool,
    /// `case` (`i`), `wholeword` (`w`), `path` (`p`), `regex` (`r`), `diacritics` (`m`),
    /// `prefix`, `suffix`, `ignore_punctuation`, `ignore_whitespace`.
    pub mods: Mods,
    /// `sort` (default name).
    pub sort: Column,
    /// `ascending` (default 1).
    pub ascending: bool,
    /// `path_column`.
    pub path_column: bool,
    /// `size_column`.
    pub size_column: bool,
    /// `date_modified_column`.
    pub date_modified_column: bool,
    /// `date_created_column`.
    pub date_created_column: bool,
    /// `attributes_column`.
    pub attributes_column: bool,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            search: String::new(),
            offset: 0,
            count: None,
            json: false,
            mods: Mods::default(),
            sort: Column::Name,
            ascending: true,
            path_column: false,
            size_column: false,
            date_modified_column: false,
            date_created_column: false,
            attributes_column: false,
        }
    }
}

fn truthy(v: &str) -> bool {
    match v.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => false,
        // Non-numeric text counts as "on".
        s => s.parse::<i64>().ok() != Some(0),
    }
}

impl SearchParams {
    /// Parses a query string. Unknown keys are ignored (like Everything).
    pub fn parse(query: &str) -> Result<Self, String> {
        let mut p = SearchParams::default();
        for (k, v) in parse_query(query) {
            let num = |v: &str| v.trim().parse::<u64>().map_err(|_| format!("invalid {k}: {v}"));
            match k.as_str() {
                "search" | "s" | "q" => p.search = v,
                "offset" | "o" => p.offset = num(&v)?,
                "count" | "c" => p.count = Some(num(&v)?),
                "json" | "j" => p.json = truthy(&v),
                "case" | "i" => p.mods.case = truthy(&v),
                "wholeword" | "w" | "whole_word" => p.mods.whole_word = truthy(&v),
                "path" | "p" => p.mods.path = truthy(&v),
                "regex" | "r" => p.mods.regex = truthy(&v),
                "diacritics" | "m" => p.mods.diacritics = truthy(&v),
                "prefix" => p.mods.prefix = truthy(&v),
                "suffix" => p.mods.suffix = truthy(&v),
                "ignore_punctuation" => p.mods.ignore_punctuation = truthy(&v),
                "ignore_whitespace" => p.mods.ignore_whitespace = truthy(&v),
                "sort" => p.sort = Column::parse(&v).ok_or_else(|| format!("invalid sort: {v}"))?,
                "ascending" => p.ascending = truthy(&v),
                "path_column" => p.path_column = truthy(&v),
                "size_column" => p.size_column = truthy(&v),
                "date_modified_column" => p.date_modified_column = truthy(&v),
                "date_created_column" => p.date_created_column = truthy(&v),
                "attributes_column" => p.attributes_column = truthy(&v),
                _ => {}
            }
        }
        Ok(p)
    }

    /// Query string reproducing these parameters with `offset`/`sort`/`ascending`
    /// overridden (HTML links).
    pub fn link(&self, offset: u64, sort: Column, ascending: bool) -> String {
        let mut q = format!("?search={}", encode_component(&self.search));
        let flags = [
            ("case", self.mods.case),
            ("wholeword", self.mods.whole_word),
            ("path", self.mods.path),
            ("regex", self.mods.regex),
            ("diacritics", self.mods.diacritics),
        ];
        for (k, on) in flags {
            if on {
                q.push_str(&format!("&{k}=1"));
            }
        }
        if offset > 0 {
            q.push_str(&format!("&offset={offset}"));
        }
        if let Some(c) = self.count {
            q.push_str(&format!("&count={c}"));
        }
        if sort != Column::Name || !ascending {
            q.push_str(&format!("&sort={}&ascending={}", sort.json_name(), u8::from(ascending)));
        }
        q
    }
}

/// Rejects searches that are too long or have too many terms/wildcards (cheap guard
/// against pathological regex/wildcard work on a network-facing server).
pub fn check_complexity(search: &str, max_len: usize, max_terms: usize) -> Result<(), String> {
    if search.len() > max_len {
        return Err(format!("search longer than {max_len} characters"));
    }
    let terms = search.split(|c: char| c.is_whitespace() || c == '|').filter(|t| !t.is_empty()).count();
    if terms > max_terms {
        return Err(format!("search has more than {max_terms} terms"));
    }
    let stars = search.chars().filter(|&c| c == '*' || c == '?').count();
    if stars > 4 * max_terms {
        return Err("too many wildcards".into());
    }
    Ok(())
}

/// Constant-time byte comparison (length leaks only through the result).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let n = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0xff);
        diff |= u64::from(x ^ y);
    }
    diff == 0
}

/// Decodes standard base64 (with or without padding); `None` on invalid input.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i);
        }
        let n = chunk.len() - 1;
        for i in 0..n {
            out.push((acc >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let p = SearchParams::parse(
            "search=abc+def&json=1&path_column=1&size_column=1&date_modified_column=1&offset=5&count=10&sort=date_modified&ascending=0&case=1&w=1&regex=0&m=1",
        )
        .unwrap();
        assert_eq!(p.search, "abc def");
        assert!(p.json && p.path_column && p.size_column && p.date_modified_column);
        assert!(!p.date_created_column);
        assert_eq!((p.offset, p.count), (5, Some(10)));
        assert_eq!((p.sort, p.ascending), (Column::DateModified, false));
        assert!(p.mods.case && p.mods.whole_word && !p.mods.regex && p.mods.diacritics);
        assert_eq!(SearchParams::parse("s=%E4%B8%AD%20x&j=true").unwrap().search, "中 x");
        assert_eq!(SearchParams::parse("q=a").unwrap().search, "a");
        assert!(SearchParams::parse("offset=x").is_err());
        assert!(SearchParams::parse("sort=bogus").is_err());
        assert_eq!(SearchParams::parse("unknown=1&search=z").unwrap().search, "z");
    }

    #[test]
    fn escaping_and_urls() {
        assert_eq!(
            html_escape("<script>alert('x')&\"</script>"),
            "&lt;script&gt;alert(&#39;x&#39;)&amp;&quot;&lt;/script&gt;"
        );
        assert_eq!(json_escape("a\"b\\c\n\u{1}"), "a\\\"b\\\\c\\n\\u0001");
        assert_eq!(download_url("C:\\a b\\c#?.txt"), "/C:/a%20b/c%23%3F.txt");
        assert_eq!(download_url("/home/u/x"), "//home/u/x");
        assert_eq!(encode_component("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn security_helpers() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secret2"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
        assert_eq!(base64_decode("dTpw").unwrap(), b"u:p");
        assert_eq!(base64_decode("dXNlcjpwYXNz").unwrap(), b"user:pass");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("@@@@").is_none());
        assert!(check_complexity("a b c", 100, 3).is_ok());
        assert!(check_complexity("a b c d", 100, 3).is_err());
        assert!(check_complexity(&"x".repeat(101), 100, 3).is_err());
        assert!(check_complexity(&"*".repeat(13), 100, 3).is_err());
    }
}

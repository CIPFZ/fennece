//! Index scope configuration (SPEC ch.1 §8.1, §9.1).
//!
//! The settings themselves live in the application (`fnc-core::settings`); these structs
//! are what the app builds from them and hands to the extractors and stores.

use std::time::Duration;

use crate::schema::{Extractor, PropertyId, normalize};

/// Folder and extension scope shared by the property and content indexes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// Included folders (`D:\Photos`, wildcards `*` within a component and `**` across
    /// components allowed); empty = every folder. Subfolders are always included.
    pub folders: Vec<String>,
    /// Included extensions without the dot (case-insensitive); empty = every file.
    pub extensions: Vec<String>,
}

impl Scope {
    /// Parses `;`-separated lists (the settings' text form).
    pub fn parse(folders: &str, extensions: &str) -> Self {
        let split = |s: &str| s.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        let exts: Vec<String> = split(extensions);
        Self {
            folders: split(folders),
            extensions: exts.iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect(),
        }
    }

    /// `true` if a file with full path `path` and extension `ext` (no dot) is in scope.
    pub fn contains(&self, path: &str, ext: &str) -> bool {
        if !self.extensions.is_empty() && !self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return false;
        }
        self.folders.is_empty() || self.folders.iter().any(|f| folder_matches(f, path))
    }
}

/// Lowercase with `/` → `\` and no trailing separator.
fn norm_path(p: &str) -> String {
    let s = p.replace('/', "\\").to_lowercase();
    let t = s.trim_end_matches('\\');
    if t.is_empty() { s } else { t.to_string() }
}

/// `true` if `path` lies inside (or is) a folder matching `pattern`.
pub fn folder_matches(pattern: &str, path: &str) -> bool {
    let pat = norm_path(pattern);
    let p = norm_path(path);
    if !pat.contains(['*', '?']) {
        return p == pat
            || (p.starts_with(&pat) && (p.as_bytes().get(pat.len()) == Some(&b'\\') || pat.ends_with('\\')));
    }
    // Wildcards: try every folder prefix of the path.
    let pat: Vec<char> = pat.chars().collect();
    let mut prefix = String::new();
    for (i, comp) in p.split('\\').enumerate() {
        if i > 0 {
            prefix.push('\\');
        }
        prefix.push_str(comp);
        let s: Vec<char> = prefix.chars().collect();
        if wild_match(&pat, &s) {
            return true;
        }
    }
    false
}

/// Wildcard match: `?` one char except `\`, `*` any run except `\`, `**` any run.
fn wild_match(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => {
            let star_all = p.get(1) == Some(&'*');
            let rest = if star_all { &p[2..] } else { &p[1..] };
            for k in 0..=s.len() {
                if wild_match(rest, &s[k..]) {
                    return true;
                }
                if k < s.len() && !star_all && s[k] == '\\' {
                    break;
                }
            }
            false
        }
        Some('?') => s.first().is_some_and(|&c| c != '\\') && wild_match(&p[1..], &s[1..]),
        Some(&c) => s.first() == Some(&c) && wild_match(&p[1..], &s[1..]),
    }
}

/// Property index configuration (SPEC ch.1 §8.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyIndexConfig {
    /// Indexed properties (derived ones pull in their sources automatically).
    pub properties: Vec<PropertyId>,
    /// Included folders (see [`Scope::folders`]); empty = all.
    pub folders: Vec<String>,
    /// Included extensions without the dot; empty = all files.
    pub extensions: Vec<String>,
    /// Keep a sorted order for instant sorting by these properties ("fast sort").
    pub fast_sort: bool,
}

impl PropertyIndexConfig {
    /// Builds a config from `;`/`,`-separated property names; unknown names are returned.
    pub fn from_names(names: &str, scope: Scope, fast_sort: bool) -> (Self, Vec<String>) {
        let mut properties = Vec::new();
        let mut unknown = Vec::new();
        for n in names.split([';', ',']).map(str::trim).filter(|s| !s.is_empty()) {
            match PropertyId::lookup(n) {
                Some(p) if !properties.contains(&p) => properties.push(p),
                Some(_) => {}
                None => unknown.push(n.to_string()),
            }
        }
        (Self { properties, folders: scope.folders, extensions: scope.extensions, fast_sort }, unknown)
    }

    /// `true` if nothing is configured (property index off).
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// `true` if the file should be indexed.
    pub fn wants(&self, path: &str, ext: &str) -> bool {
        !self.properties.is_empty() && self.scope().contains(path, ext)
    }

    /// The folder/extension scope.
    pub fn scope(&self) -> Scope {
        Scope { folders: self.folders.clone(), extensions: self.extensions.clone() }
    }

    /// Stored properties: the configured ones with derived properties replaced by their
    /// sources (`dimensions` → `width`, `height`, `frame-width`, `frame-height`).
    pub fn stored(&self) -> Vec<PropertyId> {
        let mut out = Vec::new();
        for &p in &self.properties {
            for s in sources(p) {
                if !out.contains(s) {
                    out.push(*s);
                }
            }
        }
        out
    }

    /// Extractors needed by the configured properties.
    pub fn extractors(&self) -> Vec<Extractor> {
        let mut v = Vec::new();
        for p in self.stored() {
            let e = p.extractor();
            if !v.contains(&e) {
                v.push(e);
            }
        }
        v
    }
}

/// Stored properties a property is computed from (itself unless derived).
pub fn sources(p: PropertyId) -> &'static [PropertyId] {
    const DIMS: &[PropertyId] =
        &[PropertyId::WIDTH, PropertyId::HEIGHT, PropertyId::FRAME_WIDTH, PropertyId::FRAME_HEIGHT];
    match p {
        PropertyId::DIMENSIONS | PropertyId::ASPECT_RATIO => DIMS,
        _ => PROPS_SELF.get(p.0 as usize).map_or(&[], std::slice::from_ref),
    }
}

static PROPS_SELF: std::sync::LazyLock<Vec<PropertyId>> = std::sync::LazyLock::new(|| PropertyId::all().collect());

/// Default content-index extensions (SPEC ch.1 §9.1).
pub const DEFAULT_CONTENT_EXTENSIONS: &str =
    "txt;md;ini;log;json;xml;csv;html;htm;js;ts;py;rs;java;c;cpp;h;cs;go;toml;yaml;yml";

/// Extraction limits (SPEC ch.1 §9.1: per-file size, CPU budget).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentLimits {
    /// Files larger than this are not read (default 4 MiB).
    pub max_file_bytes: u64,
    /// Extracted text is truncated to this many bytes (default 4 MiB).
    pub max_text_bytes: usize,
    /// Wall-clock budget per file; parsers stop early when exceeded (default 2 s).
    pub time_budget: Duration,
    /// Parse Office Open XML documents (docx/xlsx/pptx).
    pub office: bool,
    /// Parse PDF documents.
    pub pdf: bool,
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 << 20,
            max_text_bytes: 4 << 20,
            time_budget: Duration::from_secs(2),
            office: true,
            pdf: true,
        }
    }
}

/// Content index configuration (SPEC ch.1 §9.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentIndexConfig {
    /// Folder/extension scope (extensions default to [`DEFAULT_CONTENT_EXTENSIONS`]).
    pub scope: Scope,
    /// Per-file limits.
    pub limits: ContentLimits,
    /// Total compressed size limit of the store in bytes (0 = unlimited).
    pub max_total_bytes: u64,
}

impl Default for ContentIndexConfig {
    fn default() -> Self {
        Self {
            scope: Scope::parse("", DEFAULT_CONTENT_EXTENSIONS),
            limits: ContentLimits::default(),
            max_total_bytes: 0,
        }
    }
}

impl ContentIndexConfig {
    /// `true` if the file should be content-indexed.
    pub fn wants(&self, path: &str, ext: &str, size: u64) -> bool {
        size <= self.limits.max_file_bytes && self.scope.contains(path, ext)
    }
}

/// Normalised property name list for settings round-trips (`width;height`).
pub fn property_names(props: &[PropertyId]) -> String {
    props.iter().map(|p| p.name()).collect::<Vec<_>>().join(";")
}

/// `true` if `a` and `b` name the same property.
pub fn same_name(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_and_extensions() {
        let s = Scope::parse("D:\\Photos;C:\\Users\\*\\Pictures", "jpg;.PNG");
        assert!(s.contains("D:\\Photos\\2026\\a.jpg", "jpg"));
        assert!(s.contains("d:/photos/a.png", "PNG"));
        assert!(!s.contains("D:\\Photos2\\a.jpg", "jpg"));
        assert!(!s.contains("D:\\Photos\\a.gif", "gif"));
        assert!(s.contains("C:\\Users\\bob\\Pictures\\x\\a.jpg", "jpg"));
        assert!(!s.contains("C:\\Users\\bob\\x\\Pictures\\a.jpg", "jpg"));
        let all = Scope::default();
        assert!(all.contains("x", ""));
        assert!(folder_matches("C:\\**\\raw", "C:\\a\\b\\raw\\x.nef"));
        assert!(folder_matches("C:\\", "C:\\a.txt"));
        assert!(folder_matches("/home/u", "/home/u/x.txt"));
    }

    #[test]
    fn property_config() {
        let (c, unknown) = PropertyIndexConfig::from_names("width; Height;dimensions;bogus", Scope::default(), false);
        assert_eq!(unknown, ["bogus"]);
        assert!(c.wants("C:\\a.jpg", "jpg"));
        let stored = c.stored();
        assert!(stored.contains(&PropertyId::FRAME_WIDTH) && !stored.contains(&PropertyId::DIMENSIONS));
        assert_eq!(c.extractors(), [Extractor::Image, Extractor::Video]);
        assert!(!PropertyIndexConfig::default().wants("C:\\a.jpg", "jpg"));
        assert_eq!(property_names(&c.properties), "width;height;dimensions");
        assert!(same_name("Date-Taken", "datetaken"));
    }

    #[test]
    fn content_config() {
        let c = ContentIndexConfig::default();
        assert!(c.wants("C:\\n\\a.md", "md", 10));
        assert!(!c.wants("C:\\n\\a.md", "md", 5 << 20));
        assert!(!c.wants("C:\\n\\a.exe", "exe", 10));
    }
}

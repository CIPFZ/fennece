//! Content store (`content.fnx`, SPEC ch.1 §9.3).
//!
//! Per entry: the extracted text, LZ4-compressed (ADR-013), its line/word/char counts and
//! its token set. A token inverted index (token → rows) prunes `content:` searches to the
//! documents that can contain the pattern; candidates are then verified on the
//! decompressed text (exact substring/word/regex semantics, first-hit offset and hit
//! count).
//!
//! Tokens are taken from the default fold (case- and diacritic-insensitive): runs of
//! letters/digits, and overlapping bigrams for CJK scripts (no word separators there).
//! A pattern prunes with its own tokens: a letter/digit token must be a *substring* of a
//! document token (so `content:plan` finds "planning"), a CJK bigram must be present
//! exactly, a single CJK character must occur in some document bigram.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fnc_core::EntryId;
use fnc_core::fold::{FoldOptions, fold_str_into, fold_str_mapped};
use fnc_index::IndexStore;
use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{RawRwLock, RwLock, RwLockWriteGuard};
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};

use crate::fnx::{Dec, Enc, FnxError, FnxResult, FnxWriter, segments};
use crate::rows::{NONE, RowMap};

/// `content.fnx` magic.
pub const CONTENT_MAGIC: [u8; 4] = *b"FNXC";
/// `content.fnx` layout version.
pub const CONTENT_VERSION: u32 = 1;
/// Longest indexed token (longer runs are cut; such pattern tokens do not prune).
const MAX_TOKEN_CHARS: usize = 48;

/// Line, word and character counts of a text.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TextStats {
    /// Lines (a final line without newline counts).
    pub lines: u32,
    /// Whitespace-separated words.
    pub words: u32,
    /// Characters excluding line breaks.
    pub chars: u32,
}

/// Counts lines, words and characters.
pub fn text_stats(s: &str) -> TextStats {
    let mut st = TextStats::default();
    let mut in_word = false;
    let mut line_has = false;
    for c in s.chars() {
        if c == '\n' {
            st.lines += 1;
            line_has = false;
        } else if c != '\r' {
            st.chars = st.chars.saturating_add(1);
            line_has = true;
        }
        let w = !c.is_whitespace();
        if w && !in_word {
            st.words = st.words.saturating_add(1);
        }
        in_word = w;
    }
    if line_has {
        st.lines += 1;
    }
    st
}

/// `true` for scripts written without spaces (Han, kana, Hangul).
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF | 0x1100..=0x11FF
        | 0x20000..=0x2FFFF)
}

/// One token of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PatTok {
    /// Must be a substring of a document token.
    Within(String),
    /// Must be a document token.
    Exact(String),
}

/// Tokens of default-folded text (`f(token)` for each, duplicates included).
fn tokenize(folded: &str, mut f: impl FnMut(&str, bool)) {
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_word = |w: &mut String, f: &mut dyn FnMut(&str, bool)| {
        if !w.is_empty() {
            let cut: String = w.chars().take(MAX_TOKEN_CHARS).collect();
            f(&cut, w.chars().count() > MAX_TOKEN_CHARS);
            w.clear();
        }
    };
    let flush_cjk = |c: &mut Vec<char>, f: &mut dyn FnMut(&str, bool)| {
        match c.len() {
            0 => {}
            1 => f(&c[0].to_string(), false),
            _ => {
                for p in c.windows(2) {
                    let s: String = p.iter().collect();
                    f(&s, false);
                }
            }
        }
        c.clear();
    };
    for ch in folded.chars() {
        if is_cjk(ch) {
            flush_word(&mut word, &mut f);
            cjk.push(ch);
        } else if ch.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut f);
            word.push(ch);
        } else {
            flush_word(&mut word, &mut f);
            flush_cjk(&mut cjk, &mut f);
        }
    }
    flush_word(&mut word, &mut f);
    flush_cjk(&mut cjk, &mut f);
}

/// Pruning tokens of a (default-folded) pattern.
fn pattern_tokens(folded: &str) -> Vec<PatTok> {
    let mut out = Vec::new();
    tokenize(folded, |t, cut| {
        if cut || t.chars().count() > MAX_TOKEN_CHARS / 2 {
            return;
        }
        let first = t.chars().next();
        let tok = if first.is_some_and(is_cjk) && t.chars().count() == 2 {
            PatTok::Exact(t.to_string())
        } else {
            PatTok::Within(t.to_string())
        };
        if !out.contains(&tok) {
            out.push(tok);
        }
    });
    out
}

/// How a content pattern matches the text.
#[derive(Debug)]
pub enum ContentMatcher {
    /// Substring (folded per options).
    Substring {
        /// Folded needle.
        needle: String,
        /// Folding applied to both sides.
        fold: FoldOptions,
    },
    /// Whole word (letters/digits around the match are not allowed).
    Word {
        /// Folded needle.
        needle: String,
        /// Folding.
        fold: FoldOptions,
    },
    /// Regular expression on the original text.
    Regex(Regex),
}

impl ContentMatcher {
    /// Substring matcher (`case`/`diacritics` = sensitive).
    pub fn substring(pattern: &str, case: bool, diacritics: bool) -> Self {
        let fold = FoldOptions { fold_case: !case, strip_diacritics: !diacritics };
        let mut needle = String::new();
        fold_str_into(pattern, fold, &mut needle);
        ContentMatcher::Substring { needle, fold }
    }

    /// Whole-word matcher.
    pub fn word(pattern: &str, case: bool, diacritics: bool) -> Self {
        match Self::substring(pattern, case, diacritics) {
            ContentMatcher::Substring { needle, fold } => ContentMatcher::Word { needle, fold },
            other => other,
        }
    }

    /// Regex matcher (`case` = case-sensitive).
    pub fn regex(pattern: &str, case: bool, dot_all: bool, multi_line: bool) -> Result<Self, regex::Error> {
        RegexBuilder::new(pattern)
            .case_insensitive(!case)
            .dot_matches_new_line(dot_all)
            .multi_line(multi_line)
            .size_limit(1 << 23)
            .build()
            .map(ContentMatcher::Regex)
    }

    /// Wildcard matcher: `*` any run within a line, `?` one character, unanchored.
    pub fn wildcard(pattern: &str, case: bool) -> Result<Self, regex::Error> {
        let mut re = String::new();
        for c in pattern.chars() {
            match c {
                '*' => re.push_str(".*?"),
                '?' => re.push('.'),
                c => re.push_str(&regex::escape(&c.to_string())),
            }
        }
        Self::regex(&re, case, false, false)
    }

    /// Default-folded needle for index pruning (`None` for regexes).
    fn prune_text(&self) -> Option<String> {
        match self {
            ContentMatcher::Substring { needle, .. } | ContentMatcher::Word { needle, .. } => {
                let mut s = String::new();
                fold_str_into(needle, FoldOptions::DEFAULT, &mut s);
                Some(s)
            }
            ContentMatcher::Regex(_) => None,
        }
    }

    /// Calls `f(start, end)` (byte offsets in `text`) for every match until it returns
    /// `false`. An empty needle matches once at 0.
    pub fn each_match(&self, text: &str, sc: &mut MatchScratch, mut f: impl FnMut(usize, usize) -> bool) {
        match self {
            ContentMatcher::Regex(r) => {
                for m in r.find_iter(text) {
                    if !f(m.start(), m.end()) {
                        return;
                    }
                }
            }
            ContentMatcher::Substring { needle, fold } | ContentMatcher::Word { needle, fold } => {
                if needle.is_empty() {
                    f(0, 0);
                    return;
                }
                let word = matches!(self, ContentMatcher::Word { .. });
                let MatchScratch { buf, map } = sc;
                let (hay, mapped) = MatchScratch::fold_into(buf, map, text, *fold);
                let finder = memchr::memmem::Finder::new(needle.as_bytes());
                for p in finder.find_iter(hay.as_bytes()) {
                    let end = p + needle.len();
                    if word {
                        let before = hay[..p].chars().next_back();
                        let after = hay[end..].chars().next();
                        if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
                            continue;
                        }
                    }
                    let (s, e) = if mapped {
                        (
                            map.get(p).copied().unwrap_or(0) as usize,
                            map.get(end).copied().unwrap_or(text.len() as u32) as usize,
                        )
                    } else {
                        (p, end)
                    };
                    if !f(s, e) {
                        return;
                    }
                }
            }
        }
    }

    /// `(first match byte offset in text, number of matches)`, `None` if no match.
    pub fn find(&self, text: &str, sc: &mut MatchScratch) -> Option<(usize, u32)> {
        let mut first = None;
        let mut count = 0u32;
        self.each_match(text, sc, |s, _| {
            first.get_or_insert(s);
            count = count.saturating_add(1);
            true
        });
        first.map(|f| (f, count))
    }

    /// Byte ranges of the first `max` matches in `text` (preview highlighting).
    pub fn ranges(&self, text: &str, sc: &mut MatchScratch, max: usize) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        self.each_match(text, sc, |s, e| {
            if e > s {
                out.push(s..e);
            }
            out.len() < max
        });
        out
    }
}

/// Reusable buffers for [`ContentMatcher::find`].
#[derive(Default)]
pub struct MatchScratch {
    buf: String,
    map: Vec<u32>,
}

impl MatchScratch {
    /// Folds `text` into `buf`; returns the haystack and whether offsets need mapping
    /// through `map`.
    fn fold_into<'a>(buf: &'a mut String, map: &mut Vec<u32>, text: &'a str, fold: FoldOptions) -> (&'a str, bool) {
        if !fold.fold_case && !fold.strip_diacritics {
            return (text, false);
        }
        if text.is_ascii() {
            buf.clear();
            if fold.fold_case {
                buf.extend(text.bytes().map(|b| b.to_ascii_lowercase() as char));
            } else {
                buf.push_str(text);
            }
            return (buf, false);
        }
        fold_str_mapped(text, fold, buf, map);
        (buf, true)
    }
}

/// One matching document.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContentHit {
    /// Entry.
    pub id: EntryId,
    /// Byte offset of the first match in the stored text.
    pub offset: u32,
    /// Number of matches.
    pub hits: u32,
}

#[derive(Clone, Debug)]
struct Doc {
    data: Box<[u8]>,
    stats: TextStats,
    tokens: Box<[u32]>,
}

/// Token dictionary with postings.
#[derive(Clone, Debug, Default)]
struct Tokens {
    /// Tokens joined by `\n` (substring scans run over this one buffer).
    blob: String,
    /// Start of each token in `blob`.
    starts: Vec<u32>,
    map: HashMap<Box<str>, u32>,
    postings: Vec<Vec<u32>>,
}

impl Tokens {
    fn intern(&mut self, t: &str) -> u32 {
        if let Some(&i) = self.map.get(t) {
            return i;
        }
        let i = self.starts.len() as u32;
        self.starts.push(self.blob.len() as u32);
        self.blob.push_str(t);
        self.blob.push('\n');
        self.map.insert(t.into(), i);
        self.postings.push(Vec::new());
        i
    }

    fn token(&self, i: u32) -> &str {
        let s = self.starts.get(i as usize).copied().unwrap_or(0) as usize;
        let e = self.starts.get(i as usize + 1).map_or(self.blob.len(), |&e| e as usize);
        self.blob.get(s..e.saturating_sub(1).max(s)).unwrap_or("")
    }

    /// Ids of tokens containing `needle`.
    fn containing(&self, needle: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for p in memchr::memmem::find_iter(self.blob.as_bytes(), needle.as_bytes()) {
            let k = self.starts.partition_point(|&s| s as usize <= p).saturating_sub(1) as u32;
            if out.last() != Some(&k) {
                out.push(k);
            }
        }
        out
    }
}

/// Fixed-size bit set over rows.
struct RowBits(Vec<u64>);

impl RowBits {
    fn new(n: usize) -> Self {
        RowBits(vec![0; n.div_ceil(64)])
    }
    fn set(&mut self, i: u32) {
        if let Some(w) = self.0.get_mut(i as usize / 64) {
            *w |= 1 << (i % 64);
        }
    }
    fn and(&mut self, o: &RowBits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a &= *b;
        }
    }
    fn ones(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(w, &bits)| {
            let mut b = bits;
            std::iter::from_fn(move || {
                if b == 0 {
                    return None;
                }
                let t = b.trailing_zeros();
                b &= b - 1;
                Some(w as u32 * 64 + t)
            })
        })
    }
}

/// The content store of one index instance.
#[derive(Clone, Debug)]
pub struct ContentStore {
    rows: RowMap,
    generation: u64,
    docs: Vec<Option<Doc>>,
    tokens: Tokens,
    compressed: u64,
    raw: u64,
}

impl ContentStore {
    /// Empty store for `IndexStore::instance()` `instance`.
    pub fn new(instance: u64) -> Self {
        Self {
            rows: RowMap::new(instance),
            generation: 1,
            docs: Vec::new(),
            tokens: Tokens::default(),
            compressed: 0,
            raw: 0,
        }
    }

    /// Index instance of the ids.
    pub fn instance(&self) -> u64 {
        self.rows.instance()
    }

    /// Incremented by every mutation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Indexed documents.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Compressed bytes stored.
    pub fn compressed_bytes(&self) -> u64 {
        self.compressed
    }

    /// Uncompressed text bytes stored.
    pub fn text_bytes(&self) -> u64 {
        self.raw
    }

    /// `true` if `id` has indexed content.
    #[inline]
    pub fn has(&self, id: EntryId) -> bool {
        self.rows.row(id).is_some()
    }

    /// Row map (entry ↔ row).
    pub fn rows(&self) -> &RowMap {
        &self.rows
    }

    /// Stores `text` for `id` (replacing earlier content).
    pub fn insert(&mut self, id: EntryId, text: &str) {
        self.remove_quiet(id);
        let (row, _) = self.rows.ensure(id);
        let mut folded = String::with_capacity(text.len());
        fold_str_into(text, FoldOptions::DEFAULT, &mut folded);
        let mut ids: Vec<u32> = Vec::new();
        let toks = &mut self.tokens;
        tokenize(&folded, |t, _| ids.push(toks.intern(t)));
        ids.sort_unstable();
        ids.dedup();
        for &t in &ids {
            if let Some(p) = self.tokens.postings.get_mut(t as usize) {
                p.push(row);
            }
        }
        let data = lz4_flex::block::compress_prepend_size(text.as_bytes()).into_boxed_slice();
        self.compressed += data.len() as u64;
        self.raw += text.len() as u64;
        let doc = Doc { data, stats: text_stats(text), tokens: ids.into() };
        if self.docs.len() <= row as usize {
            self.docs.resize(row as usize + 1, None);
        }
        self.docs[row as usize] = Some(doc);
        self.generation += 1;
    }

    fn remove_quiet(&mut self, id: EntryId) -> bool {
        let Some(row) = self.rows.remove(id) else { return false };
        self.drop_row(row);
        true
    }

    fn drop_row(&mut self, row: u32) {
        if let Some(doc) = self.docs.get_mut(row as usize).and_then(Option::take) {
            self.compressed -= doc.data.len() as u64;
            self.raw -= u64::from(doc_len(&doc.data));
            for &t in doc.tokens.iter() {
                if let Some(p) = self.tokens.postings.get_mut(t as usize)
                    && let Some(i) = p.iter().position(|&r| r == row)
                {
                    p.swap_remove(i);
                }
            }
        }
    }

    /// Forgets `id`.
    pub fn remove(&mut self, id: EntryId) {
        if self.remove_quiet(id) {
            self.generation += 1;
        }
    }

    /// Follows [`IndexStore::compact`].
    pub fn remap(&mut self, remap: &[u32], new_instance: u64) {
        for row in self.rows.remap(remap, new_instance) {
            self.drop_row(row);
        }
        self.generation += 1;
    }

    /// Stored text of `id` (for the preview panel).
    pub fn text(&self, id: EntryId) -> Option<String> {
        let doc = self.docs.get(self.rows.row(id)? as usize)?.as_ref()?;
        decompress(&doc.data)
    }

    /// Line/word/char counts of `id`.
    pub fn stats(&self, id: EntryId) -> Option<TextStats> {
        Some(self.docs.get(self.rows.row(id)? as usize)?.as_ref()?.stats)
    }

    /// Rows that may match (`None` = no pruning possible).
    fn candidates(&self, m: &ContentMatcher) -> Option<RowBits> {
        let toks = pattern_tokens(&m.prune_text()?);
        if toks.is_empty() {
            return None;
        }
        let n = self.rows.rows();
        let mut acc: Option<RowBits> = None;
        for t in toks {
            let mut bits = RowBits::new(n);
            let ids = match &t {
                PatTok::Exact(s) => self.tokens.map.get(s.as_str()).copied().into_iter().collect(),
                PatTok::Within(s) => self.tokens.containing(s),
            };
            for id in ids {
                for &r in self.tokens.postings.get(id as usize).map_or(&[][..], |p| p.as_slice()) {
                    bits.set(r);
                }
            }
            match &mut acc {
                Some(a) => a.and(&bits),
                None => acc = Some(bits),
            }
        }
        acc
    }

    /// Documents matching `m`, in row order; `None` if `cancelled()` became true.
    pub fn search(&self, m: &ContentMatcher, cancelled: &(dyn Fn() -> bool + Sync)) -> Option<Vec<ContentHit>> {
        let rows: Vec<u32> = match self.candidates(m) {
            Some(bits) => bits.ones().collect(),
            None => (0..self.docs.len() as u32).collect(),
        };
        let ids = self.rows.ids();
        let hits: Vec<Option<ContentHit>> = rows
            .par_iter()
            .map_init(MatchScratch::default, |sc, &r| {
                if cancelled() {
                    return None;
                }
                let id = *ids.get(r as usize)?;
                if id == NONE {
                    return None;
                }
                let doc = self.docs.get(r as usize)?.as_ref()?;
                let text = decompress(&doc.data)?;
                let (offset, hits) = m.find(&text, sc)?;
                Some(ContentHit { id: EntryId(id), offset: offset as u32, hits })
            })
            .collect();
        if cancelled() {
            return None;
        }
        let mut out: Vec<ContentHit> = hits.into_iter().flatten().collect();
        out.sort_unstable_by_key(|h| h.id);
        Some(out)
    }

    /// Convenience: substring search with default options.
    pub fn search_text(&self, pattern: &str) -> Vec<ContentHit> {
        self.search(&ContentMatcher::substring(pattern, false, false), &|| false).unwrap_or_default()
    }

    /// Approximate heap use in bytes.
    pub fn heap_bytes(&self) -> usize {
        self.rows.heap_bytes()
            + self.compressed as usize
            + self.docs.iter().flatten().map(|d| d.tokens.len() * 4 + 48).sum::<usize>()
            + self.tokens.blob.len() * 2
            + self.tokens.starts.len() * 4
            + self.tokens.map.len() * 32
            + self.tokens.postings.iter().map(|p| p.capacity() * 4 + 24).sum::<usize>()
    }

    // ------------------------------------------------------------------ persistence

    /// Serialises the store (`content.fnx`); `s` must be the index the ids belong to.
    pub fn to_bytes(&self, s: &IndexStore) -> FnxResult<Vec<u8>> {
        if s.instance() != self.instance() {
            return Err(FnxError::Instance);
        }
        let mut w = FnxWriter::new(CONTENT_MAGIC, CONTENT_VERSION);
        w.segment(*b"ROWS", &self.rows.encode(s));
        // Compacted token dictionary (only tokens still referenced).
        let mut new_id = vec![NONE; self.tokens.starts.len()];
        let mut dict = Enc::default();
        let mut used: Vec<u32> = Vec::new();
        for d in self.docs.iter().flatten() {
            for &t in d.tokens.iter() {
                if let Some(slot) = new_id.get_mut(t as usize)
                    && *slot == NONE
                {
                    *slot = used.len() as u32;
                    used.push(t);
                }
            }
        }
        dict.u32(used.len() as u32);
        for &t in &used {
            dict.str(self.tokens.token(t));
        }
        w.segment(*b"TOKS", &dict.0);
        let mut docs = Enc::default();
        docs.u32(self.docs.len() as u32);
        for d in &self.docs {
            match d {
                None => docs.u8(0),
                Some(d) => {
                    docs.u8(1);
                    docs.bytes(&d.data);
                    docs.u32(d.stats.lines);
                    docs.u32(d.stats.words);
                    docs.u32(d.stats.chars);
                    docs.u32(d.tokens.len() as u32);
                    for &t in d.tokens.iter() {
                        docs.u32(new_id.get(t as usize).copied().unwrap_or(NONE));
                    }
                }
            }
        }
        w.segment(*b"DOCS", &docs.0);
        Ok(w.finish())
    }

    /// Saves atomically.
    pub fn save(&self, s: &IndexStore, path: &Path) -> FnxResult<()> {
        crate::fnx::write_atomic(path, &self.to_bytes(s)?)
    }

    /// Loads and attaches to `s`.
    pub fn load(path: &Path, s: &IndexStore) -> FnxResult<Self> {
        Self::from_bytes(&crate::fnx::read_file(path)?, s)
    }

    /// Decodes `content.fnx` bytes against `s`.
    pub fn from_bytes(buf: &[u8], s: &IndexStore) -> FnxResult<Self> {
        let segs = segments(buf, CONTENT_MAGIC, CONTENT_VERSION, "content store")?;
        let find = |t: &[u8; 4]| {
            segs.iter().find(|(tag, _)| tag == t).map(|(_, b)| *b).ok_or(FnxError::Corrupt("missing segment"))
        };
        let (rows, assign) = RowMap::decode(find(b"ROWS")?, s)?;
        let mut store = ContentStore::new(s.instance());
        store.rows = rows;
        let mut d = Dec::new(find(b"TOKS")?);
        let nt = d.count(4)?;
        let mut file_tokens = Vec::with_capacity(nt);
        for _ in 0..nt {
            file_tokens.push(d.str()?);
        }
        let mut d = Dec::new(find(b"DOCS")?);
        let nd = d.count(1)?;
        if nd != assign.len() {
            return Err(FnxError::Corrupt("document count"));
        }
        for &row in assign.iter().take(nd) {
            if d.u8()? == 0 {
                continue;
            }
            let data = d.bytes()?;
            let stats = TextStats { lines: d.u32()?, words: d.u32()?, chars: d.u32()? };
            let n = d.count(4)?;
            let mut toks = Vec::with_capacity(n);
            for _ in 0..n {
                let t = d.u32()?;
                let name = file_tokens.get(t as usize).ok_or(FnxError::Corrupt("token id"))?;
                toks.push(store.tokens.intern(name));
            }
            if row == NONE {
                continue;
            }
            // Validate the compressed block now: a corrupt document must not surface later.
            if decompress(data).is_none() {
                return Err(FnxError::Corrupt("document data"));
            }
            toks.sort_unstable();
            toks.dedup();
            for &t in &toks {
                if let Some(p) = store.tokens.postings.get_mut(t as usize) {
                    p.push(row);
                }
            }
            store.compressed += data.len() as u64;
            store.raw += u64::from(doc_len(data));
            if store.docs.len() <= row as usize {
                store.docs.resize(row as usize + 1, None);
            }
            store.docs[row as usize] = Some(Doc { data: data.into(), stats, tokens: toks.into() });
        }
        // Rows whose document was empty in the file are dropped again.
        let orphan: Vec<EntryId> = store
            .rows
            .ids()
            .iter()
            .enumerate()
            .filter(|&(r, &id)| id != NONE && store.docs.get(r).is_none_or(Option::is_none))
            .map(|(_, &id)| EntryId(id))
            .collect();
        for id in orphan {
            store.rows.remove(id);
        }
        Ok(store)
    }
}

fn doc_len(data: &[u8]) -> u32 {
    data.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn decompress(data: &[u8]) -> Option<String> {
    let bytes = lz4_flex::block::decompress_size_prepended(data).ok()?;
    String::from_utf8(bytes).ok()
}

/// Shared handle to a [`ContentStore`].
#[derive(Clone)]
pub struct ContentIndex(Arc<RwLock<ContentStore>>);

/// Owned read guard.
pub type ContentSnapshot = ArcRwLockReadGuard<RawRwLock, ContentStore>;

impl ContentIndex {
    /// Wraps a store.
    pub fn new(store: ContentStore) -> Self {
        Self(Arc::new(RwLock::new(store)))
    }

    /// Read snapshot.
    pub fn snapshot(&self) -> ContentSnapshot {
        self.0.read_arc()
    }

    /// Read snapshot within `timeout` (UI thread).
    pub fn try_snapshot_for(&self, timeout: std::time::Duration) -> Option<ContentSnapshot> {
        self.0.try_read_arc_for(timeout)
    }

    /// Exclusive access.
    pub fn write(&self) -> RwLockWriteGuard<'_, ContentStore> {
        self.0.write()
    }

    /// `true` if both handles share one store.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for ContentIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentIndex(..)")
    }
}

impl PartialEq for ContentIndex {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(n: usize) -> IndexStore {
        let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
        for i in 0..n {
            efu.push_str(&format!("\"C:\\notes\\n{i}.txt\",{},5,,32\n", 100 + i));
        }
        let mut s = IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s
    }

    fn id(s: &IndexStore, i: usize) -> EntryId {
        let name = format!("n{i}.txt");
        (0..s.len() as u32).map(EntryId).find(|&e| s.name_string(e) == name).unwrap()
    }

    fn sample() -> (IndexStore, ContentStore) {
        let s = index(5);
        let mut c = ContentStore::new(s.instance());
        c.insert(id(&s, 0), "The Quarterly Plan for 2026.\nSecond line about planning.");
        c.insert(id(&s, 1), "quarterly report; no p-l-a-n here");
        c.insert(id(&s, 2), "第三季度的计划：季度报告和预算");
        c.insert(id(&s, 3), "Café crème — CAFE au lait, café");
        c.insert(id(&s, 4), "plan quarterly");
        (s, c)
    }

    fn ids(s: &IndexStore, hits: &[ContentHit]) -> Vec<usize> {
        hits.iter().map(|h| (0..5).find(|&i| id(s, i) == h.id).unwrap()).collect()
    }

    #[test]
    fn stats_and_tokens() {
        assert_eq!(text_stats("a b\nc\n"), TextStats { lines: 2, words: 3, chars: 4 });
        assert_eq!(text_stats("x"), TextStats { lines: 1, words: 1, chars: 1 });
        assert_eq!(text_stats(""), TextStats::default());
        let mut t = Vec::new();
        tokenize("ab 季度报告x", |s, _| t.push(s.to_string()));
        assert_eq!(t, ["ab", "季度", "度报", "报告", "x"]);
        assert_eq!(
            pattern_tokens("quarterly plan"),
            [PatTok::Within("quarterly".into()), PatTok::Within("plan".into())]
        );
        assert_eq!(pattern_tokens("季度"), [PatTok::Exact("季度".into())]);
        assert_eq!(pattern_tokens("季"), [PatTok::Within("季".into())]);
    }

    #[test]
    fn search_semantics() {
        let (s, c) = sample();
        assert_eq!(ids(&s, &c.search_text("quarterly plan")), [0]);
        assert_eq!(ids(&s, &c.search_text("plan")), [0, 4], "substring: planning; p-l-a-n is not plan");
        assert_eq!(ids(&s, &c.search_text("季度")), [2]);
        assert_eq!(ids(&s, &c.search_text("季")), [2]);
        assert_eq!(ids(&s, &c.search_text("cafe")), [3]);
        let hit = c.search_text("cafe")[0];
        assert_eq!(hit.hits, 3);
        assert_eq!(hit.offset, 0);
        let h = c.search_text("planning")[0];
        assert_eq!(&c.text(h.id).unwrap()[h.offset as usize..h.offset as usize + 8], "planning");
        let cjk = c.search_text("报告")[0];
        assert_eq!(&c.text(cjk.id).unwrap()[cjk.offset as usize..], "报告和预算");
        let case = ContentMatcher::substring("Quarterly", true, false);
        assert_eq!(ids(&s, &c.search(&case, &|| false).unwrap()), [0]);
        let diac = ContentMatcher::substring("café", false, true);
        assert_eq!(c.search(&diac, &|| false).unwrap()[0].hits, 2);
        let word = ContentMatcher::word("plan", false, false);
        assert_eq!(ids(&s, &c.search(&word, &|| false).unwrap()), [0, 4]);
        let re = ContentMatcher::regex(r"\d{4}", false, false, false).unwrap();
        assert_eq!(ids(&s, &c.search(&re, &|| false).unwrap()), [0]);
        assert!(c.search(&ContentMatcher::substring("x", false, false), &|| true).is_none());
        assert_eq!(ids(&s, &c.search_text("!!")), [], "no tokens: full scan, no match");
        assert_eq!(c.search_text("").len(), 5);
    }

    #[test]
    fn update_remove_remap_and_stats() {
        let (mut s, mut c) = sample();
        assert_eq!(c.stats(id(&s, 0)), Some(TextStats { lines: 2, words: 9, chars: 55 }));
        c.insert(id(&s, 4), "nothing relevant");
        assert_eq!(ids(&s, &c.search_text("quarterly")), [0, 1]);
        c.remove(id(&s, 1));
        assert_eq!(ids(&s, &c.search_text("quarterly")), [0]);
        assert!(!c.has(id(&s, 1)));
        s.apply(vec![fnc_index::Change::Delete(fnc_index::EntryRef::Id(id(&s, 0)))]);
        let remap = s.compact();
        c.remap(&remap, s.instance());
        assert_eq!(c.len(), 3);
        assert!(c.search_text("quarterly").is_empty());
        assert_eq!(c.search_text("季度").len(), 1);
        assert!(c.compressed_bytes() > 0 && c.text_bytes() > 0 && c.heap_bytes() > 0);
    }

    #[test]
    fn persistence_roundtrip_and_corruption() {
        let (s, mut c) = sample();
        c.remove(id(&s, 4));
        let bytes = c.to_bytes(&s).unwrap();
        let s2 = s.clone();
        let d = ContentStore::from_bytes(&bytes, &s2).unwrap();
        assert_eq!(d.len(), 4);
        for i in 0..4 {
            assert_eq!(d.text(id(&s2, i)), c.text(id(&s, i)));
            assert_eq!(d.stats(id(&s2, i)), c.stats(id(&s, i)));
        }
        assert_eq!(d.search_text("quarterly plan").len(), 1);
        assert_eq!(d.search_text("季").len(), 1);
        assert!(matches!(c.to_bytes(&s2), Err(FnxError::Instance)));
        for i in (0..bytes.len()).step_by(17) {
            let mut b = bytes.clone();
            b[i] ^= 0x33;
            // Header padding/reserved/timestamp and inter-segment padding are not
            // covered; any accepted file must decode to the same content.
            match ContentStore::from_bytes(&b, &s2) {
                Err(_) => {}
                Ok(x) => {
                    assert!((0..4).all(|k| x.text(id(&s2, k)) == c.text(id(&s, k))), "flip at {i} changed content")
                }
            }
        }
        let h = ContentIndex::new(d);
        assert_eq!(h.snapshot().len(), 4);
        h.write().remove(id(&s2, 0));
        assert_eq!(h.try_snapshot_for(std::time::Duration::from_millis(5)).unwrap().len(), 3);
        assert_eq!(h, h.clone());
    }
}

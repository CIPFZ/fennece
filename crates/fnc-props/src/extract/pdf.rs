//! Minimal PDF reader: document information (`/Info`), page count and text of content
//! streams (FlateDecode, `/ToUnicode` CMaps, object streams). No OCR, no encryption
//! (encrypted documents yield metadata only when the `/Info` strings are unencrypted).
//!
//! Hand-written instead of `pdf-extract`/`lopdf` (ADR-014): those pull in large
//! dependency trees for features the index does not need. Everything is bounds-checked
//! and recursion/iteration is capped, so malformed files only produce less output.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fnc_core::FileTime;

use super::{ExtractLimits, Props, push_text, read_all};
use crate::schema::PropertyId as P;
use crate::value::Value;

/// PDF object.
#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    /// `null` or anything unparseable.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// Number.
    Num(f64),
    /// `/Name`.
    Name(Vec<u8>),
    /// String (literal or hex), raw bytes.
    Str(Vec<u8>),
    /// Array.
    Array(Vec<Obj>),
    /// Dictionary.
    Dict(Dict),
    /// Indirect reference.
    Ref(u32),
    /// Stream: dictionary and raw (still encoded) data range in the file.
    Stream(Dict, usize, usize),
    /// Content-stream operator (only produced when lexing content streams).
    Op(Vec<u8>),
}

/// Dictionary as a small association list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dict(pub Vec<(Vec<u8>, Obj)>);

impl Dict {
    /// Value of key `k` (without the slash).
    pub fn get(&self, k: &[u8]) -> Option<&Obj> {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// Tokenizer/parser over a byte slice.
struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8], pos: usize) -> Self {
        Self { b, pos, depth: 0 }
    }

    fn skip_ws(&mut self) {
        while let Some(&c) = self.b.get(self.pos) {
            if is_ws(c) {
                self.pos += 1;
            } else if c == b'%' {
                while let Some(&c) = self.b.get(self.pos) {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn word(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(&c) = self.b.get(self.pos) {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
        }
        &self.b[start..self.pos]
    }

    fn literal(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut nest = 1usize;
        while let Some(&c) = self.b.get(self.pos) {
            self.pos += 1;
            match c {
                b'(' => {
                    nest += 1;
                    out.push(c);
                }
                b')' => {
                    nest -= 1;
                    if nest == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(&e) = self.b.get(self.pos) else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.b.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.b.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn hex(&mut self) -> Vec<u8> {
        let mut digits = Vec::new();
        while let Some(&c) = self.b.get(self.pos) {
            self.pos += 1;
            if c == b'>' {
                break;
            }
            if let Some(d) = (c as char).to_digit(16) {
                digits.push(d as u8);
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect()
    }

    fn name(&mut self) -> Vec<u8> {
        let raw = self.word();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'#'
                && let Some(v) = raw
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
            out.push(raw[i]);
            i += 1;
        }
        out
    }

    /// Next object (`ops`: produce operators for unknown words, as in content streams).
    fn obj(&mut self, ops: bool) -> Option<Obj> {
        self.skip_ws();
        let c = *self.b.get(self.pos)?;
        if self.depth > 64 {
            return None;
        }
        Some(match c {
            b'/' => {
                self.pos += 1;
                Obj::Name(self.name())
            }
            b'(' => {
                self.pos += 1;
                Obj::Str(self.literal())
            }
            b'<' if self.b.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                self.depth += 1;
                let mut d = Dict::default();
                for _ in 0..100_000 {
                    self.skip_ws();
                    match self.b.get(self.pos) {
                        None => break,
                        Some(b'>') => {
                            self.pos += 2;
                            break;
                        }
                        Some(b'/') => {
                            self.pos += 1;
                            let k = self.name();
                            let v = self.obj(false).unwrap_or(Obj::Null);
                            d.0.push((k, v));
                        }
                        Some(_) => {
                            // Malformed: skip one token.
                            if self.obj(false).is_none() {
                                break;
                            }
                        }
                    }
                }
                self.depth -= 1;
                let save = self.pos;
                self.skip_ws();
                if self.b[self.pos.min(self.b.len())..].starts_with(b"stream") {
                    self.pos += 6;
                    if self.b.get(self.pos) == Some(&b'\r') {
                        self.pos += 1;
                    }
                    if self.b.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    let start = self.pos;
                    let end = memchr::memmem::find(&self.b[start..], b"endstream").map_or(self.b.len(), |e| start + e);
                    self.pos = end;
                    return Some(Obj::Stream(d, start, end));
                }
                self.pos = save;
                Obj::Dict(d)
            }
            b'<' => {
                self.pos += 1;
                Obj::Str(self.hex())
            }
            b'[' => {
                self.pos += 1;
                self.depth += 1;
                let mut v = Vec::new();
                for _ in 0..1_000_000 {
                    self.skip_ws();
                    match self.b.get(self.pos) {
                        None => break,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => match self.obj(ops) {
                            Some(o) => v.push(o),
                            None => break,
                        },
                    }
                }
                self.depth -= 1;
                Obj::Array(v)
            }
            b']' | b'>' | b')' | b'{' | b'}' => {
                self.pos += 1;
                Obj::Null
            }
            _ => {
                let w = self.word();
                if w.is_empty() {
                    self.pos += 1;
                    return Some(Obj::Null);
                }
                match w {
                    b"true" => Obj::Bool(true),
                    b"false" => Obj::Bool(false),
                    b"null" => Obj::Null,
                    _ => match std::str::from_utf8(w).ok().and_then(|s| s.parse::<f64>().ok()) {
                        Some(n) => {
                            // `n g R` reference?
                            if !ops && n >= 0.0 && n.fract() == 0.0 {
                                let save = self.pos;
                                self.skip_ws();
                                let g = self.word();
                                if !g.is_empty() && g.iter().all(u8::is_ascii_digit) {
                                    self.skip_ws();
                                    if self.b.get(self.pos) == Some(&b'R')
                                        && self.b.get(self.pos + 1).is_none_or(|&c| is_ws(c) || is_delim(c))
                                    {
                                        self.pos += 1;
                                        return Some(Obj::Ref(n as u32));
                                    }
                                }
                                self.pos = save;
                            }
                            Obj::Num(n)
                        }
                        None if ops => Obj::Op(w.to_vec()),
                        None => Obj::Null,
                    },
                }
            }
        })
    }
}

/// Where an object lives.
#[derive(Clone, Copy, Debug)]
enum Loc {
    /// Byte offset of `N G obj` in the file.
    File(usize),
    /// Index inside object stream `stream`.
    InStream(u32, usize),
}

/// Decoded object stream: data, `(object number, offset)` index, `/First`.
type ObjStm = (Vec<u8>, Vec<(u32, usize)>, usize);

/// A loaded document.
pub struct Doc<'a> {
    b: &'a [u8],
    locs: HashMap<u32, Loc>,
    objstm: HashMap<u32, ObjStm>,
    trailer: Dict,
}

/// Inflates a FlateDecode stream (zlib, or raw deflate as a fallback).
fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, limit)
        .or_else(|_| miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit))
        .ok()
}

impl<'a> Doc<'a> {
    /// Indexes every `N G obj` in the file and object streams; finds the trailer.
    pub fn open(b: &'a [u8]) -> Option<Doc<'a>> {
        memchr::memmem::find(&b[..b.len().min(1024)], b"%PDF")?;
        let mut locs = HashMap::new();
        for p in memchr::memmem::find_iter(b, b" obj") {
            // Back up over "N G".
            let mut i = p;
            let mut nums = [0u32; 2];
            let mut ok = true;
            for slot in (0..2).rev() {
                let end = i;
                while i > 0 && b[i - 1].is_ascii_digit() {
                    i -= 1;
                }
                if i == end {
                    ok = false;
                    break;
                }
                nums[slot] = std::str::from_utf8(&b[i..end]).ok().and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
                if slot == 1 {
                    if i == 0 || b[i - 1] != b' ' {
                        ok = false;
                        break;
                    }
                    i -= 1;
                }
            }
            if ok && nums[0] != u32::MAX && (i == 0 || is_ws(b[i - 1]) || is_delim(b[i - 1])) {
                locs.insert(nums[0], Loc::File(p + 4));
            }
        }
        let mut doc = Doc { b, locs, objstm: HashMap::new(), trailer: Dict::default() };
        // Object streams.
        let stream_ids: Vec<u32> = doc.locs.keys().copied().collect();
        for id in stream_ids {
            if let Some(Obj::Stream(d, ..)) = doc.direct(id)
                && d.get(b"Type") == Some(&Obj::Name(b"ObjStm".to_vec()))
                && let Some(data) = doc.stream_data(id)
            {
                let n = doc.num(d.get(b"N")).unwrap_or(0.0) as usize;
                let first = doc.num(d.get(b"First")).unwrap_or(0.0) as usize;
                let mut lx = Lexer::new(&data, 0);
                let mut index = Vec::new();
                for k in 0..n.min(100_000) {
                    let (Some(Obj::Num(num)), Some(Obj::Num(off))) = (lx.obj(true), lx.obj(true)) else { break };
                    index.push((num as u32, off as usize));
                    doc.locs.entry(num as u32).or_insert(Loc::InStream(id, k));
                }
                doc.objstm.insert(id, (data, index, first));
            }
        }
        // Trailer: classic `trailer << >>` (last one wins), else the XRef stream dictionary.
        if let Some(p) = memchr::memmem::rfind(b, b"trailer")
            && let Some(Obj::Dict(d)) = Lexer::new(b, p + 7).obj(false)
        {
            doc.trailer = d;
        }
        if doc.trailer.get(b"Root").is_none() {
            let ids: Vec<u32> = doc.locs.keys().copied().collect();
            for id in ids {
                if let Some(Obj::Stream(d, ..)) = doc.direct(id)
                    && d.get(b"Type") == Some(&Obj::Name(b"XRef".to_vec()))
                    && d.get(b"Root").is_some()
                {
                    doc.trailer = d;
                    break;
                }
            }
        }
        Some(doc)
    }

    fn direct(&self, id: u32) -> Option<Obj> {
        match *self.locs.get(&id)? {
            Loc::File(off) => Lexer::new(self.b, off).obj(false),
            Loc::InStream(sid, k) => {
                let (data, index, first) = self.objstm.get(&sid)?;
                let off = first.checked_add(index.get(k)?.1)?;
                Lexer::new(data, off).obj(false)
            }
        }
    }

    /// Resolves references (bounded chain).
    pub fn resolve(&self, o: &Obj) -> Obj {
        let mut cur = o.clone();
        for _ in 0..8 {
            match cur {
                Obj::Ref(id) => cur = self.direct(id).unwrap_or(Obj::Null),
                other => return other,
            }
        }
        Obj::Null
    }

    fn dict(&self, o: Option<&Obj>) -> Option<Dict> {
        match self.resolve(o?) {
            Obj::Dict(d) | Obj::Stream(d, ..) => Some(d),
            _ => None,
        }
    }

    fn num(&self, o: Option<&Obj>) -> Option<f64> {
        match self.resolve(o?) {
            Obj::Num(n) => Some(n),
            _ => None,
        }
    }

    /// Decoded data of stream object `id`.
    fn stream_data(&self, id: u32) -> Option<Vec<u8>> {
        match self.direct(id)? {
            Obj::Stream(d, s, e) => self.decode(&d, s, e),
            _ => None,
        }
    }

    fn decode(&self, d: &Dict, start: usize, end: usize) -> Option<Vec<u8>> {
        let len = self
            .num(d.get(b"Length"))
            .map(|l| l as usize)
            .filter(|&l| start.checked_add(l).is_some_and(|x| x <= self.b.len()));
        let raw = &self.b[start..len.map_or(end, |l| start + l).min(self.b.len()).max(start)];
        let filters: Vec<Vec<u8>> = match d.get(b"Filter").map(|f| self.resolve(f)) {
            None | Some(Obj::Null) => Vec::new(),
            Some(Obj::Name(n)) => vec![n],
            Some(Obj::Array(a)) => {
                a.into_iter().filter_map(|f| if let Obj::Name(n) = f { Some(n) } else { None }).collect()
            }
            _ => return None,
        };
        let mut data = raw.to_vec();
        for f in filters {
            data = match f.as_slice() {
                b"FlateDecode" | b"Fl" => inflate(&data, 64 << 20)?,
                _ => return None,
            };
        }
        Some(data)
    }

    /// The `/Info` dictionary.
    pub fn info(&self) -> Option<Dict> {
        self.dict(self.trailer.get(b"Info"))
    }

    fn root(&self) -> Option<Dict> {
        self.dict(self.trailer.get(b"Root"))
    }

    /// Page count from the page tree root.
    pub fn page_count(&self) -> Option<i64> {
        let pages = self.dict(self.root()?.get(b"Pages"))?;
        self.num(pages.get(b"Count")).map(|n| n as i64).filter(|&n| n >= 0)
    }

    /// Page dictionaries with inherited resources, in order (capped).
    fn pages(&self, max: usize) -> Vec<(Dict, Option<Dict>)> {
        let mut out = Vec::new();
        let Some(root) = self.root() else { return out };
        let mut stack: Vec<(Obj, Option<Dict>, usize)> = match root.get(b"Pages") {
            Some(p) => vec![(p.clone(), None, 0)],
            None => return out,
        };
        let mut visited = 0usize;
        while let Some((node, inherited, depth)) = stack.pop() {
            visited += 1;
            if out.len() >= max || visited > max * 4 + 64 || depth > 32 {
                break;
            }
            let Some(d) = self.dict(Some(&node)) else { continue };
            let res = self.dict(d.get(b"Resources")).or(inherited);
            match self.resolve(d.get(b"Kids").unwrap_or(&Obj::Null)) {
                Obj::Array(kids) => {
                    for k in kids.into_iter().rev() {
                        stack.push((k, res.clone(), depth + 1));
                    }
                }
                _ => out.push((d, res)),
            }
        }
        out
    }

    /// Text of all pages, at most `max_bytes`, within `deadline`.
    pub fn text(&self, max_bytes: usize, deadline: Instant) -> String {
        let mut out = String::new();
        for (page, res) in self.pages(10_000) {
            if out.len() >= max_bytes || Instant::now() > deadline {
                break;
            }
            let fonts = self.fonts(res.as_ref());
            let contents: Vec<Obj> = match page.get(b"Contents") {
                Some(Obj::Array(a)) => a.clone(),
                Some(o) => match self.resolve(o) {
                    Obj::Array(a) => a,
                    _ => vec![o.clone()],
                },
                None => Vec::new(),
            };
            let mut data = Vec::new();
            for c in contents {
                let s = match (&c, self.resolve(&c)) {
                    (_, Obj::Stream(d, s, e)) => self.decode(&d, s, e),
                    _ => None,
                };
                if let Some(s) = s {
                    data.extend_from_slice(&s);
                    data.push(b'\n');
                }
            }
            content_text(&data, &fonts, &mut out, max_bytes);
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
        }
        super::ooxml::truncate_utf8(&mut out, max_bytes);
        out
    }

    fn fonts(&self, res: Option<&Dict>) -> HashMap<Vec<u8>, Font> {
        let mut m = HashMap::new();
        let Some(fd) = res.and_then(|r| self.dict(r.get(b"Font"))) else { return m };
        for (name, f) in &fd.0 {
            let Some(font) = self.dict(Some(f)) else { continue };
            let two_byte = font.get(b"Subtype") == Some(&Obj::Name(b"Type0".to_vec()));
            let cmap = match font.get(b"ToUnicode").map(|t| self.resolve(t)) {
                Some(Obj::Stream(d, s, e)) => self.decode(&d, s, e).map(|data| parse_cmap(&data)),
                _ => None,
            };
            m.insert(name.clone(), Font { two_byte, cmap });
        }
        m
    }
}

/// Font decoding info.
#[derive(Default)]
struct Font {
    two_byte: bool,
    cmap: Option<CMap>,
}

/// `/ToUnicode` mapping: code (1–4 bytes) → text.
#[derive(Default)]
struct CMap {
    map: HashMap<(u8, u32), String>,
    ranges: Vec<(u8, u32, u32, Vec<u16>)>,
    widths: Vec<u8>,
}

fn utf16be(b: &[u8]) -> Vec<u16> {
    b.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect()
}

fn code_of(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0, |a, &x| (a << 8) | u32::from(x))
}

fn parse_cmap(data: &[u8]) -> CMap {
    let mut cm = CMap::default();
    let mut lx = Lexer::new(data, 0);
    let mut stack: Vec<Obj> = Vec::new();
    for _ in 0..1_000_000 {
        let Some(o) = lx.obj(true) else { break };
        match o {
            Obj::Op(op) => {
                match op.as_slice() {
                    b"endbfchar" => {
                        for pair in stack.chunks(2) {
                            if let [Obj::Str(src), Obj::Str(dst)] = pair {
                                cm.map.insert((src.len() as u8, code_of(src)), String::from_utf16_lossy(&utf16be(dst)));
                            }
                        }
                    }
                    b"endbfrange" => {
                        for tri in stack.chunks(3) {
                            match tri {
                                [Obj::Str(lo), Obj::Str(hi), Obj::Str(dst)] => {
                                    cm.ranges.push((lo.len() as u8, code_of(lo), code_of(hi), utf16be(dst)));
                                }
                                [Obj::Str(lo), Obj::Str(_), Obj::Array(dsts)] => {
                                    for (k, d) in dsts.iter().enumerate().take(65_536) {
                                        if let Obj::Str(d) = d {
                                            cm.map.insert(
                                                (lo.len() as u8, code_of(lo) + k as u32),
                                                String::from_utf16_lossy(&utf16be(d)),
                                            );
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    b"endcodespacerange" => {
                        for pair in stack.chunks(2) {
                            if let [Obj::Str(lo), _] = pair
                                && !cm.widths.contains(&(lo.len() as u8))
                            {
                                cm.widths.push(lo.len() as u8);
                            }
                        }
                    }
                    _ => {}
                }
                stack.clear();
            }
            other => stack.push(other),
        }
    }
    cm.widths.sort();
    cm
}

impl CMap {
    fn lookup(&self, width: u8, code: u32) -> Option<String> {
        if let Some(s) = self.map.get(&(width, code)) {
            return Some(s.clone());
        }
        self.ranges.iter().find(|(w, lo, hi, _)| *w == width && code >= *lo && code <= *hi).map(|(_, lo, _, dst)| {
            let mut d = dst.clone();
            if let Some(last) = d.last_mut() {
                *last = last.wrapping_add((code - lo) as u16);
            }
            String::from_utf16_lossy(&d)
        })
    }
}

/// Decodes a shown string with `font`.
fn show(s: &[u8], font: Option<&Font>, out: &mut String) {
    match font {
        Some(f) if f.cmap.is_some() => {
            let Some(cm) = &f.cmap else { return };
            let widths: Vec<u8> =
                if cm.widths.is_empty() { vec![if f.two_byte { 2 } else { 1 }] } else { cm.widths.clone() };
            let mut i = 0;
            while i < s.len() {
                let mut done = false;
                for &w in &widths {
                    let w = w as usize;
                    if let Some(chunk) = s.get(i..i + w)
                        && let Some(t) = cm.lookup(w as u8, code_of(chunk))
                    {
                        out.push_str(&t);
                        i += w;
                        done = true;
                        break;
                    }
                }
                if !done {
                    i += widths.first().copied().unwrap_or(1) as usize;
                }
            }
        }
        Some(f) if f.two_byte => {}
        _ => out.extend(s.iter().map(|&b| pdfdoc_char(b))),
    }
}

/// PDFDocEncoding / WinAnsi approximation (Latin-1 with the common punctuation).
fn pdfdoc_char(b: u8) -> char {
    match b {
        0x91 | 0x92 => '\'',
        0x93 | 0x94 => '"',
        0x96 | 0x97 => '-',
        0x85 => '…',
        0x80 => '€',
        b'\r' | b'\n' | b'\t' => b as char,
        b if b < 0x20 => ' ',
        b => char::from(b),
    }
}

/// Interprets text operators of a content stream.
fn content_text(data: &[u8], fonts: &HashMap<Vec<u8>, Font>, out: &mut String, max: usize) {
    let mut lx = Lexer::new(data, 0);
    let mut stack: Vec<Obj> = Vec::new();
    let mut font: Option<&Font> = None;
    for _ in 0..10_000_000 {
        if out.len() >= max {
            break;
        }
        let Some(o) = lx.obj(true) else { break };
        let Obj::Op(op) = o else {
            if stack.len() < 1024 {
                stack.push(o);
            }
            continue;
        };
        match op.as_slice() {
            b"Tf" => {
                if let Some(Obj::Name(n)) = stack.first() {
                    font = fonts.get(n);
                }
            }
            b"Tj" => {
                if let Some(Obj::Str(s)) = stack.last() {
                    show(s, font, out);
                }
            }
            b"'" | b"\"" => {
                out.push('\n');
                if let Some(Obj::Str(s)) = stack.last() {
                    show(s, font, out);
                }
            }
            b"TJ" => {
                if let Some(Obj::Array(a)) = stack.last() {
                    for item in a {
                        match item {
                            Obj::Str(s) => show(s, font, out),
                            Obj::Num(n) if *n < -200.0 && !out.ends_with(' ') => out.push(' '),
                            _ => {}
                        }
                    }
                }
            }
            b"T*" => out.push('\n'),
            b"Td" | b"TD" => {
                if let (Some(Obj::Num(_)), Some(Obj::Num(ty))) = (stack.first(), stack.get(1))
                    && !out.is_empty()
                    && !out.ends_with('\n')
                {
                    out.push(if ty.abs() > 0.01 { '\n' } else { ' ' });
                }
            }
            b"ET" => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            b"BI" => {
                // Inline image: skip to EI.
                match memchr::memmem::find(&data[lx.pos.min(data.len())..], b"EI") {
                    Some(e) => lx.pos += e + 2,
                    None => break,
                }
            }
            _ => {}
        }
        stack.clear();
    }
}

/// Text string (UTF-16BE with BOM, UTF-8 with BOM, else PDFDocEncoding).
pub fn text_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xfe, 0xff]) {
        String::from_utf16_lossy(&utf16be(rest))
    } else if let Some(rest) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        b.iter().map(|&c| pdfdoc_char(c)).collect()
    }
}

/// `D:YYYYMMDDHHmmSSOHH'mm'` (every part after the year optional).
pub fn pdf_date(s: &str) -> Option<FileTime> {
    let s = s.trim().strip_prefix("D:").unwrap_or(s.trim());
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }
    let part = |a: usize, b: usize, d: u32| digits.get(a..b).and_then(|x| x.parse::<u32>().ok()).unwrap_or(d);
    let year = digits.get(..4)?.parse::<i32>().ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(year, part(4, 6, 1), part(6, 8, 1))?;
    let naive = date.and_hms_opt(part(8, 10, 0), part(10, 12, 0), part(12, 14, 0))?;
    let rest = &s[digits.len()..];
    let offset_min = match rest.chars().next() {
        Some('Z') => Some(0),
        Some(c @ ('+' | '-')) => {
            let t: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
            let h: i64 = t.get(..2).and_then(|x| x.parse().ok()).unwrap_or(0);
            let m: i64 = t.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
            Some(if c == '-' { -(h * 60 + m) } else { h * 60 + m })
        }
        _ => None,
    };
    Some(match offset_min {
        Some(off) => FileTime::from_utc(naive.and_utc() - chrono::Duration::minutes(off)),
        None => FileTime::from_local(naive),
    })
}

/// `/Info` metadata and page count of the PDF at `path`.
pub fn extract_props(path: &Path, limits: ExtractLimits, out: &mut Props) {
    if let Ok(Some(b)) = read_all(path, limits)
        && let Some(doc) = Doc::open(&b)
    {
        props_of(&doc, out);
    }
}

/// Metadata of an open document.
pub fn props_of(doc: &Doc<'_>, out: &mut Props) {
    if let Some(info) = doc.info() {
        let get = |k: &[u8]| match info.get(k).map(|o| doc.resolve(o)) {
            Some(Obj::Str(s)) => Some(text_string(&s)),
            _ => None,
        };
        if let Some(s) = get(b"Title") {
            push_text(out, P::TITLE, &s);
        }
        if let Some(s) = get(b"Subject") {
            push_text(out, P::SUBJECT, &s);
        }
        if let Some(v) = get(b"Author").and_then(|s| Value::multi(s.split(';'))) {
            out.push((P::AUTHOR, v));
        }
        if let Some(v) = get(b"Keywords").and_then(|s| Value::multi(s.split([';', ',']))) {
            out.push((P::TAGS, v));
        }
        if let Some(d) = get(b"CreationDate").and_then(|s| pdf_date(&s)) {
            out.push((P::CONTENT_CREATED, Value::Date(d)));
        }
        if let Some(d) = get(b"ModDate").and_then(|s| pdf_date(&s)) {
            out.push((P::DATE_LAST_SAVED, Value::Date(d)));
        }
    }
    if let Some(n) = doc.page_count() {
        out.push((P::PAGE_COUNT, Value::Int(n)));
    }
}

/// Text of the PDF at `path` (at most `max_bytes`).
pub fn extract_text(path: &Path, limits: ExtractLimits, max_bytes: usize) -> Option<String> {
    let b = read_all(path, limits).ok()??;
    let doc = Doc::open(&b)?;
    Some(doc.text(max_bytes, Instant::now() + limits.time_budget))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::testutil::file;

    fn zlib(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
    }

    /// Builds a PDF from object bodies (object `k+1` = `objs[k]`), with a classic xref
    /// trailer pointing at `/Root 1 0 R /Info 2 0 R`.
    pub(crate) fn build(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (k, o) in objs.iter().enumerate() {
            offsets.push(b.len());
            b.extend_from_slice(format!("{} 0 obj\n", k + 1).as_bytes());
            b.extend_from_slice(o);
            b.extend_from_slice(b"\nendobj\n");
        }
        let xref = b.len();
        b.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            b.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        b.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R /Info 2 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1)
                .as_bytes(),
        );
        b
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    }

    /// Two pages: page 1 uses a simple font, page 2 a Type0 font with a ToUnicode CMap
    /// (CJK); compressed content streams.
    pub(crate) fn sample_pdf(page1_text: &str) -> Vec<u8> {
        let c1 = format!("BT /F1 12 Tf 72 720 Td ({page1_text}) Tj 0 -14 Td [(Second) -300 (line)] TJ ET");
        let c2 = b"BT /F2 12 Tf 72 720 Td <00010002> Tj ET".to_vec();
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n2 beginbfchar <0001> <5B63> <0002> <5EA6> endbfchar\nendcmap end end".to_vec();
        build(&[
            b"<< /Type /Catalog /Pages 3 0 R >>".to_vec(),
            b"<< /Title (Quarterly \\(Q3\\) plan) /Author <FEFF0041006C006900630065> /Subject (Planning) /Keywords (plan; budget) /CreationDate (D:20260115093000Z) /ModDate (D:20260201100000+01'00') >>".to_vec(),
            b"<< /Type /Pages /Kids [4 0 R 5 0 R] /Count 2 /Resources << /Font << /F1 6 0 R /F2 7 0 R >> >> >>".to_vec(),
            b"<< /Type /Page /Parent 3 0 R /Contents 8 0 R >>".to_vec(),
            b"<< /Type /Page /Parent 3 0 R /Contents [9 0 R] >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /CJK /ToUnicode 10 0 R >>".to_vec(),
            stream("/Filter /FlateDecode", &zlib(c1.as_bytes())),
            stream("/Filter /FlateDecode", &zlib(&c2)),
            stream("/Filter /FlateDecode", &zlib(&cmap)),
        ])
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn info_and_pages() {
        let (_d, p) = file("a.pdf", &sample_pdf("Hello"));
        let mut out = Props::new();
        extract_props(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Quarterly (Q3) plan".into())));
        assert_eq!(get(&out, P::AUTHOR), Value::multi(["Alice"]).as_ref());
        assert_eq!(get(&out, P::SUBJECT), Some(&Value::Text("Planning".into())));
        assert_eq!(get(&out, P::TAGS), Value::multi(["plan", "budget"]).as_ref());
        assert_eq!(get(&out, P::PAGE_COUNT), Some(&Value::Int(2)));
        let created = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap().and_hms_opt(9, 30, 0).unwrap().and_utc();
        assert_eq!(get(&out, P::CONTENT_CREATED), Some(&Value::Date(FileTime::from_utc(created))));
        let modified = chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap().and_hms_opt(9, 0, 0).unwrap().and_utc();
        assert_eq!(get(&out, P::DATE_LAST_SAVED), Some(&Value::Date(FileTime::from_utc(modified))));
    }

    #[test]
    fn text_with_simple_and_cid_fonts() {
        let (_d, p) = file("a.pdf", &sample_pdf("Quarterly plan"));
        let t = extract_text(&p, ExtractLimits::default(), 1 << 20).unwrap();
        assert_eq!(t, "Quarterly plan\nSecond line\n季度\n");
        let short = extract_text(&p, ExtractLimits::default(), 5).unwrap();
        assert_eq!(short, "Quart");
    }

    #[test]
    fn object_streams_and_xref_streams() {
        // Catalog, pages and info inside an object stream; trailer in an XRef stream.
        let inner = b"1 0 2 40 3 90 << /Type /Catalog /Pages 3 0 R >>       << /Title (Packed) >>                             << /Type /Pages /Kids [] /Count 5 >>";
        let first = inner.iter().position(|&c| c == b'<').unwrap();
        let objstm = stream(&format!("/Type /ObjStm /N 3 /First {first} /Filter /FlateDecode"), &zlib(inner));
        let mut b = b"%PDF-1.5\n".to_vec();
        b.extend_from_slice(b"4 0 obj\n");
        b.extend_from_slice(&objstm);
        b.extend_from_slice(b"\nendobj\n5 0 obj\n");
        b.extend_from_slice(&stream("/Type /XRef /Root 1 0 R /Info 2 0 R /Size 6", b""));
        b.extend_from_slice(b"\nendobj\nstartxref\n0\n%%EOF\n");
        // Offsets inside the stream are relative to /First.
        let doc = Doc::open(&b).unwrap();
        let mut out = Props::new();
        props_of(&doc, &mut out);
        assert_eq!(get(&out, P::PAGE_COUNT), Some(&Value::Int(5)));
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Packed".into())));
    }

    #[test]
    fn dates_strings_and_garbage() {
        assert!(pdf_date("D:2026").is_some());
        assert!(pdf_date("D:20260230").is_none());
        assert!(pdf_date("x").is_none());
        assert_eq!(text_string(b"caf\xe9"), "café");
        assert_eq!(text_string(&[0xfe, 0xff, 0, b'A']), "A");
        let pdf = sample_pdf("x");
        for cut in (0..pdf.len()).step_by(11) {
            if let Some(d) = Doc::open(&pdf[..cut]) {
                let _ = d.text(1 << 16, Instant::now() + std::time::Duration::from_secs(1));
                props_of(&d, &mut Props::new());
            }
        }
        for i in (0..pdf.len()).step_by(5) {
            let mut b = pdf.clone();
            b[i] ^= 0x5a;
            if let Some(d) = Doc::open(&b) {
                let _ = d.text(1 << 16, Instant::now() + std::time::Duration::from_secs(1));
            }
        }
        assert!(Doc::open(b"not a pdf").is_none());
    }
}

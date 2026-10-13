//! Planning of property, content, hash and set (duplicate) functions (SPEC ch.2 §5.2–5.9,
//! ch.1 §8, §9, §11). Child module of `plan` (shares its private planner context).

use fnc_props::hash::{HashAlgo, is_hex};
use fnc_props::value::float_key;
use fnc_props::{PropType, PropertyId, Unit};

use super::{Cx, NumPred, Pred, Target, TextPred, and_node, numeric_value, or_node};
use crate::ast::{CmpOp, ModSet, Modifier, Term, Value};
use crate::duration::parse_duration;
use crate::funcs::FuncId;
use crate::plan::{Node, PinyinMode, PlanDiag};

/// What a property predicate tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropTest {
    /// Numeric value in ranges (integers, booleans, dates as FILETIME, durations in ms,
    /// floats as [`float_key`] order keys); `unknown` matches entries without a value.
    Num(NumPred),
    /// Text value matches (any value of a multi-text property, or all with `all`).
    Text {
        /// Pattern.
        pred: TextPred,
        /// `allstrings:` — every value must match.
        all: bool,
    },
    /// Length of the text value (`len:album:>20`).
    Len(NumPred),
    /// Number of values (`count:tags:>2`).
    Count(NumPred),
    /// Leading integer of the text (`atoi:`).
    Atoi(NumPred),
    /// Leading number of the text as a float key (`atof:`).
    Atof(NumPred),
    /// `dimensions:` — width and height (images, else video frames).
    Dims(NumPred, NumPred),
    /// Any value (`artist:` with no value).
    Known,
    /// No value (`artist:unknown`).
    Unknown,
}

/// Property predicate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PropPred {
    /// Property.
    pub id: PropertyId,
    /// Test.
    pub test: PropTest,
    /// `notindexed:` — read the file for entries the property index does not cover.
    pub not_indexed: bool,
    /// The property index is configured for this property (cost hint only).
    pub indexed: bool,
}

/// Byte interpretation of a content function.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentEnc {
    /// Detect (BOM, UTF-16 heuristic, UTF-8, `chardetng`).
    Auto,
    /// UTF-8.
    Utf8,
    /// ANSI code page.
    Ansi,
    /// UTF-16 LE.
    Utf16Le,
    /// UTF-16 BE.
    Utf16Be,
}

/// Where content text comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentSource {
    /// `content:` — the content index, else the extractor (documents parsed).
    Indexed,
    /// `textplain:` / `utf8content:` / ... — the file's bytes as text, no parsing.
    Plain(ContentEnc),
    /// `binary:content:` — raw bytes (`\xNN` escapes).
    Binary,
}

/// Content predicate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentPred {
    /// Pattern and options (target is always the content).
    pub text: TextPred,
    /// Source.
    pub source: ContentSource,
    /// Needle bytes for [`ContentSource::Binary`].
    pub bytes: Vec<u8>,
}

/// Hash predicate (`sha256:ab12`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HashPred {
    /// Algorithm.
    pub algo: HashAlgo,
    /// Accepted hex prefixes (OR); empty with `unknown == false` = any computable hash.
    pub prefixes: Vec<String>,
    /// `sha256:unknown` — not in the hash cache (nothing is computed).
    pub unknown: bool,
}

/// Grouping key of duplicate/set functions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DupeKey {
    /// Whole name (folded).
    Name,
    /// Name without extension (`namepartdupe:`).
    NamePart,
    /// Extension.
    Ext,
    /// Size.
    Size,
    /// Attributes.
    Attrib,
    /// Date accessed.
    DateAccessed,
    /// Date created.
    DateCreated,
    /// Date modified.
    DateModified,
    /// Containing folder.
    Path,
    /// File hash (computed only within groups of equal size).
    Hash(HashAlgo),
    /// A property value.
    Prop(PropertyId),
}

/// What a set function does with its groups.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SetKind {
    /// Keep members of groups with ≥ 2 results (`dupe:`).
    Dupe,
    /// Keep the first result of each group (`distinct:`).
    Distinct,
    /// Keep results that are alone in their group (`unique:`).
    Unique,
    /// Keep groups whose size matches (`dupe-count:`).
    DupeCount(NumPred),
    /// Keep at most N results per group (`dupe-max:`).
    DupeMax(u32),
    /// Keep the first N results (`count:<max>`, Everything 1.5 result limit); always
    /// applied after the other set functions.
    Limit(u64),
}

/// Set function applied to the sorted result set (Everything 1.5 semantics).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SetOp {
    /// Function as written (diagnostics).
    pub func: FuncId,
    /// Operation.
    pub kind: SetKind,
    /// Grouping keys (`None` for `dupe-count:`/`dupe-max:` until resolved from the query's
    /// other set function, defaulting to the name).
    pub keys: Option<Vec<DupeKey>>,
    /// `!dupe:` — keep the complement.
    pub negate: bool,
}

/// Parses `name;size;sha256` into grouping keys.
pub fn parse_dupe_keys(v: &Value) -> Result<Vec<DupeKey>, String> {
    let items: Vec<&str> = match v {
        Value::Empty => return Ok(vec![DupeKey::Name]),
        Value::Text(s) => s.split(';').collect(),
        Value::List(l) => l.iter().filter_map(Value::as_text).flat_map(|s| s.split(';')).collect(),
        other => return Err(format!("{other:?}")),
    };
    let mut keys = Vec::new();
    for raw in items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let n = fnc_props::schema::normalize(raw);
        let k = match n.as_str() {
            "name" | "filename" => DupeKey::Name,
            "namepart" | "stem" | "nameextstripped" | "namepartdupe" => DupeKey::NamePart,
            "ext" | "extension" => DupeKey::Ext,
            "size" => DupeKey::Size,
            "attrib" | "attributes" | "a" => DupeKey::Attrib,
            "da" | "dateaccessed" => DupeKey::DateAccessed,
            "dc" | "datecreated" => DupeKey::DateCreated,
            "dm" | "datemodified" => DupeKey::DateModified,
            "path" | "location" | "parent" => DupeKey::Path,
            other => match HashAlgo::lookup(other) {
                Some(a) => DupeKey::Hash(a),
                None => match PropertyId::lookup(other) {
                    Some(p) => DupeKey::Prop(p),
                    None => return Err(raw.to_string()),
                },
            },
        };
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    if keys.is_empty() {
        keys.push(DupeKey::Name);
    }
    Ok(keys)
}

/// `\xNN` escapes → bytes (other characters as UTF-8).
pub fn binary_needle(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1).is_some_and(|c| *c == b'x' || *c == b'X')
            && let Some(v) = s.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 4;
            continue;
        }
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'\\') {
            out.push(b'\\');
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Splits a leading comparison operator off `s`.
fn split_op(s: &str) -> (Option<CmpOp>, &str) {
    let t = s.trim();
    for (op, cmp) in
        [(">=", CmpOp::Ge), ("<=", CmpOp::Le), ("!=", CmpOp::Ne), (">", CmpOp::Gt), ("<", CmpOp::Lt), ("=", CmpOp::Eq)]
    {
        if let Some(rest) = t.strip_prefix(op) {
            return (Some(cmp), rest.trim());
        }
    }
    (None, t)
}

/// `1920x1080`, `1920*1080`, `1920 x 1080`.
fn parse_wh(s: &str) -> Option<(i64, i64)> {
    let lower = s.to_ascii_lowercase();
    let (w, h) = lower.split_once(['x', '*', '×'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// `[x - step/2, x + step/2)` of a decimal number in float-key space.
fn float_round_span(num: &str) -> Option<(i64, i64)> {
    let x: f64 = num.parse().ok()?;
    if !x.is_finite() {
        return None;
    }
    let decimals = num.split_once('.').map_or(0, |(_, f)| f.len()) as i32;
    let half = 0.5 * 10f64.powi(-decimals);
    Some((float_key(x - half), float_key(x + half)))
}

/// Exact float in key space (`[x, next(x))`).
fn float_exact(x: f64) -> (i64, i64) {
    let k = float_key(x);
    (k, k.saturating_add(1))
}

/// Number text of a float property with its unit decoration removed.
fn strip_unit(t: &str, unit: Unit) -> String {
    let mut s = t.trim().to_ascii_lowercase();
    if unit == Unit::FNumber {
        s = s.trim_start_matches("f/").trim_start_matches('f').to_string();
    }
    for suffix in [
        "sec.", "sec", "secs", "seconds", "dpi", "mm", "fps", "°", "deg", "pixels", "px", "bits", "bit", "kbps",
        "stars", "m", "s",
    ] {
        if let Some(r) = s.strip_suffix(suffix)
            && !r.is_empty()
        {
            s = r.trim().to_string();
            break;
        }
    }
    s
}

impl Cx<'_> {
    /// Span of one value of property `p` (`exact` for comparison operands of floats).
    pub(super) fn prop_span(&self, p: PropertyId, text: &str, exact: bool) -> Option<(i64, i64)> {
        let t = text.trim();
        match p.ty() {
            PropType::Date => crate::date::parse_date(t, &self.env.date).map(|s| (s.lo, s.hi)),
            PropType::Duration => parse_duration(t).map(|s| (s.lo, s.hi)),
            PropType::Float => {
                if p == PropertyId::ASPECT_RATIO {
                    return aspect_span(t, exact);
                }
                let s = strip_unit(t, p.unit());
                if let Some((a, b)) = s.split_once('/') {
                    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                    if b == 0.0 {
                        return None;
                    }
                    let x = a / b;
                    return Some(if exact {
                        float_exact(x)
                    } else {
                        (float_key(x * (1.0 - 1e-9)), float_key(x * (1.0 + 1e-9)))
                    });
                }
                if exact {
                    return s.parse::<f64>().ok().filter(|x| x.is_finite()).map(float_exact);
                }
                float_round_span(&s)
            }
            PropType::Bool => {
                let v = match t.to_ascii_lowercase().as_str() {
                    "1" | "yes" | "true" | "on" => 1,
                    "0" | "no" | "false" | "off" => 0,
                    _ => return None,
                };
                Some((v, v + 1))
            }
            PropType::Int => {
                if let Some(v) = p.named_value(t) {
                    return Some((v, v + 1));
                }
                let lower = t.to_ascii_lowercase();
                let (num, mult) = match p.unit() {
                    Unit::Hz => match lower.strip_suffix("khz") {
                        Some(n) => (n.trim().to_string(), 1000.0),
                        None => (lower.trim_end_matches("hz").trim().to_string(), 1.0),
                    },
                    Unit::Kbps => match lower.strip_suffix("mbps") {
                        Some(n) => (n.trim().to_string(), 1000.0),
                        None => (lower.trim_end_matches("kbps").trim().to_string(), 1.0),
                    },
                    u => (strip_unit(t, u), 1.0),
                };
                if let Ok(v) = num.parse::<i64>() {
                    let v = (v as f64 * mult) as i64;
                    return Some((v, v.saturating_add(mult as i64)));
                }
                let f: f64 = num.parse().ok()?;
                let decimals = num.split_once('.').map_or(0, |(_, d)| d.len()) as i32;
                let step = ((mult * 10f64.powi(-decimals)).round() as i64).max(1);
                let v = (f * mult).round() as i64;
                Some((v, v.saturating_add(step)))
            }
            PropType::Text | PropType::MultiText => None,
        }
    }

    /// Numeric predicate over property spans.
    fn prop_num(&self, p: PropertyId, v: &Value, out: &mut NumPred) -> bool {
        match v {
            Value::Empty => {
                out.ranges.push(match p.ty() {
                    PropType::Int | PropType::Bool => (1, i64::MAX),
                    _ => (i64::MIN + 1, i64::MAX),
                });
                true
            }
            Value::Text(s) if s.trim().eq_ignore_ascii_case("unknown") => {
                out.unknown = true;
                true
            }
            Value::Text(s) => match self.prop_span(p, s, false) {
                Some(r) => {
                    out.ranges.push(r);
                    true
                }
                None => {
                    // `a-b` ranges of plain numbers.
                    if let Some((a, b)) = s.split_once('-')
                        && !a.trim().is_empty()
                        && let (Some(lo), Some(hi)) = (self.prop_span(p, a, false), self.prop_span(p, b, false))
                        && !matches!(p.ty(), PropType::Date)
                    {
                        out.ranges.push((lo.0, hi.1));
                        return true;
                    }
                    false
                }
            },
            Value::Range(a, b) => {
                let lo = match a.as_text().map(str::trim) {
                    Some("") => Some(i64::MIN + 1),
                    Some(t) => self.prop_span(p, t, false).map(|r| r.0),
                    None => None,
                };
                let hi = match b.as_text().map(str::trim) {
                    Some("") => Some(i64::MAX),
                    Some(t) => self.prop_span(p, t, false).map(|r| r.1),
                    None => None,
                };
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        out.ranges.push((lo, hi));
                        true
                    }
                    _ => false,
                }
            }
            Value::Cmp(op, inner) => {
                let Some(text) = inner.as_text() else { return false };
                let exact = p.ty() == PropType::Float;
                let Some((lo, hi)) = self.prop_span(p, text, exact) else { return false };
                push_cmp(out, *op, lo, hi);
                true
            }
            Value::List(items) => items.iter().all(|i| self.prop_num(p, i, out)),
        }
    }

    /// Integer predicate for `len:`/`count:`/`atoi:` and `dupe-count:`.
    fn int_num(v: &Value, out: &mut NumPred) -> bool {
        let span = |t: &str| t.trim().parse::<i64>().ok().map(|v| (v, v.saturating_add(1)));
        match v {
            Value::Empty => {
                out.ranges.push((1, i64::MAX));
                true
            }
            Value::Text(s) => match span(s) {
                Some(r) => {
                    out.ranges.push(r);
                    true
                }
                None => false,
            },
            Value::Range(a, b) => {
                let lo = a.as_text().and_then(|t| if t.trim().is_empty() { Some((i64::MIN + 1, 0)) } else { span(t) });
                let hi = b.as_text().and_then(|t| if t.trim().is_empty() { Some((0, i64::MAX)) } else { span(t) });
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        out.ranges.push((lo.0, hi.1));
                        true
                    }
                    _ => false,
                }
            }
            Value::Cmp(op, inner) => {
                let Some((lo, hi)) = inner.as_text().and_then(span) else { return false };
                push_cmp(out, *op, lo, hi);
                true
            }
            Value::List(items) => items.iter().all(|i| Self::int_num(i, out)),
        }
    }

    /// Float-key predicate for `atof:`.
    fn float_num(v: &Value, out: &mut NumPred) -> bool {
        match v {
            Value::Text(s) => match float_round_span(s.trim()) {
                Some(r) => {
                    out.ranges.push(r);
                    true
                }
                None => false,
            },
            Value::Cmp(op, inner) => {
                let Some(x) = inner.as_text().and_then(|t| t.trim().parse::<f64>().ok()) else { return false };
                let (lo, hi) = float_exact(x);
                push_cmp(out, *op, lo, hi);
                true
            }
            Value::Range(a, b) => {
                let lo = a.as_text().and_then(|t| t.trim().parse::<f64>().ok());
                let hi = b.as_text().and_then(|t| t.trim().parse::<f64>().ok());
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        out.ranges.push((float_key(lo), float_key(hi).saturating_add(1)));
                        true
                    }
                    _ => false,
                }
            }
            Value::List(items) => items.iter().all(|i| Self::float_num(i, out)),
            Value::Empty => {
                out.ranges.push((i64::MIN + 1, i64::MAX));
                true
            }
        }
    }

    fn invalid(&mut self, f: FuncId, v: &Value) -> Node {
        let text = format!("{v:?}");
        self.diags.push(PlanDiag::InvalidValue(f, text.clone()));
        Node::Leaf(Pred::Invalid(text))
    }

    /// Text predicate on a property/content value (no path/target switching).
    pub(super) fn value_text_pred(&self, mods: ModSet, text: &str) -> TextPred {
        let mut p = self.text_pred(mods, text, Target::Ext);
        p.target = Target::Name;
        p.pinyin = PinyinMode::Off;
        p.fuzzy = false;
        p
    }

    /// `width:>4000`, `artist:"John Doe"`, `length:1min..5min`, `dimensions:1920x1080`, ...
    pub(super) fn prop(&mut self, p: PropertyId, t: &Term) -> Node {
        let f = FuncId::Prop(p);
        let not_indexed = self.on(t.mods, Modifier::NotIndexed);
        let indexed = fnc_props::config::sources(p).iter().all(|s| self.env.indexed_props.contains(s));
        let leaf = |test| Node::Leaf(Pred::Prop(PropPred { id: p, test, not_indexed, indexed }));
        // `len:` / `count:` compare the value length / number of values.
        for (m, mk) in [(Modifier::Len, PropTest::Len as fn(NumPred) -> PropTest), (Modifier::Count, PropTest::Count)] {
            if self.on(t.mods, m) {
                let v = match &t.value {
                    Value::Text(s) => numeric_value(s),
                    other => other.clone(),
                };
                let mut n = NumPred::default();
                return if Self::int_num(&v, &mut n) { leaf(mk(n)) } else { self.invalid(f, &t.value) };
            }
        }
        if let Value::Text(s) = &t.value
            && s.trim().eq_ignore_ascii_case("unknown")
        {
            return leaf(PropTest::Unknown);
        }
        if p == PropertyId::DIMENSIONS {
            return self.dims(p, t, not_indexed, indexed);
        }
        match p.ty() {
            PropType::Text | PropType::MultiText => {
                for (m, float) in [(Modifier::Atoi, false), (Modifier::Atof, true)] {
                    if self.on(t.mods, m) {
                        let v = match &t.value {
                            Value::Text(s) => numeric_value(s),
                            other => other.clone(),
                        };
                        let mut n = NumPred::default();
                        let ok = if float { Self::float_num(&v, &mut n) } else { Self::int_num(&v, &mut n) };
                        return if !ok {
                            self.invalid(f, &t.value)
                        } else if float {
                            leaf(PropTest::Atof(n))
                        } else {
                            leaf(PropTest::Atoi(n))
                        };
                    }
                }
                let items: Vec<&str> = match &t.value {
                    Value::Empty => return leaf(PropTest::Known),
                    Value::Text(s) => vec![s.as_str()],
                    Value::List(v) => v.iter().filter_map(Value::as_text).collect(),
                    other => return self.invalid(f, other),
                };
                let all = self.on(t.mods, Modifier::AllStrings);
                or_node(
                    items
                        .into_iter()
                        .map(|s| {
                            if s.trim().eq_ignore_ascii_case("unknown") {
                                leaf(PropTest::Unknown)
                            } else {
                                leaf(PropTest::Text { pred: self.value_text_pred(t.mods, s), all })
                            }
                        })
                        .collect(),
                )
            }
            _ => {
                let mut n = NumPred::default();
                if self.prop_num(p, &t.value, &mut n) { leaf(PropTest::Num(n)) } else { self.invalid(f, &t.value) }
            }
        }
    }

    fn dims(&mut self, p: PropertyId, t: &Term, not_indexed: bool, indexed: bool) -> Node {
        let leaf = |test| Node::Leaf(Pred::Prop(PropPred { id: p, test, not_indexed, indexed }));
        let items: Vec<&Value> = match &t.value {
            Value::Empty => return leaf(PropTest::Known),
            Value::List(v) => v.iter().collect(),
            other => vec![other],
        };
        let mut alts = Vec::new();
        for item in items {
            let (mut w, mut h) = (NumPred::default(), NumPred::default());
            let (op, range) = match item {
                Value::Text(s) => match s.split_once("..") {
                    Some((a, b)) => (None, Some((parse_wh(a), parse_wh(b)))),
                    None => {
                        let (op, rest) = split_op(s);
                        (Some((op.unwrap_or(CmpOp::Eq), parse_wh(rest))), None)
                    }
                },
                Value::Cmp(op, inner) => (Some((*op, inner.as_text().and_then(parse_wh))), None),
                Value::Range(a, b) => (None, Some((a.as_text().and_then(parse_wh), b.as_text().and_then(parse_wh)))),
                _ => (None, None),
            };
            match (op, range) {
                (_, Some((Some(lo), Some(hi)))) => {
                    w.ranges.push((lo.0, hi.0.saturating_add(1)));
                    h.ranges.push((lo.1, hi.1.saturating_add(1)));
                }
                (Some((CmpOp::Ne, Some((a, b)))), _) => {
                    alts.push(Node::Not(Box::new(leaf(PropTest::Dims(
                        NumPred { ranges: vec![(a, a + 1)], unknown: false },
                        NumPred { ranges: vec![(b, b + 1)], unknown: false },
                    )))));
                    continue;
                }
                // SPEC-GAP: comparisons apply to both sides (`>1920x1080`: wider and taller).
                (Some((op, Some((a, b)))), _) => {
                    push_cmp(&mut w, op, a, a + 1);
                    push_cmp(&mut h, op, b, b + 1);
                }
                _ => return self.invalid(FuncId::Prop(p), &t.value),
            }
            alts.push(leaf(PropTest::Dims(w, h)));
        }
        or_node(alts)
    }

    /// `content:` and the byte-interpretation variants.
    pub(super) fn content(&mut self, f: FuncId, t: &Term) -> Node {
        let source = match f {
            FuncId::Utf8Content => ContentSource::Plain(ContentEnc::Utf8),
            FuncId::AnsiContent => ContentSource::Plain(ContentEnc::Ansi),
            FuncId::Utf16Content => ContentSource::Plain(ContentEnc::Utf16Le),
            FuncId::Utf16BeContent => ContentSource::Plain(ContentEnc::Utf16Be),
            FuncId::TextPlain => ContentSource::Plain(ContentEnc::Auto),
            _ if self.on(t.mods, Modifier::Binary) => ContentSource::Binary,
            _ if self.on(t.mods, Modifier::Utf8) => ContentSource::Plain(ContentEnc::Utf8),
            _ if self.on(t.mods, Modifier::Ansi) => ContentSource::Plain(ContentEnc::Ansi),
            _ if self.on(t.mods, Modifier::Utf16) => ContentSource::Plain(ContentEnc::Utf16Le),
            _ if self.on(t.mods, Modifier::Utf16Be) => ContentSource::Plain(ContentEnc::Utf16Be),
            _ => ContentSource::Indexed,
        };
        let items: Vec<&str> = match &t.value {
            Value::Empty => return Node::All,
            Value::Text(s) => vec![s.as_str()],
            Value::List(v) => v.iter().filter_map(Value::as_text).collect(),
            other => return self.invalid(f, other),
        };
        // Content is a file property: folders never match.
        let files = Node::Leaf(Pred::IsDir(false));
        let alts = items
            .into_iter()
            .map(|s| {
                let text = self.value_text_pred(t.mods, s);
                let bytes = if source == ContentSource::Binary { binary_needle(s) } else { Vec::new() };
                Node::Leaf(Pred::Content(ContentPred { text, source, bytes }))
            })
            .collect();
        and_node(vec![files, or_node(alts)])
    }

    /// `crc32:` … `sha512:`.
    pub(super) fn hash(&mut self, f: FuncId, algo: HashAlgo, t: &Term) -> Node {
        let items: Vec<&str> = match &t.value {
            Value::Empty => Vec::new(),
            Value::Text(s) => vec![s.as_str()],
            Value::List(v) => v.iter().filter_map(Value::as_text).collect(),
            other => return self.invalid(f, other),
        };
        let files = Node::Leaf(Pred::IsDir(false));
        if items.iter().any(|s| s.trim().eq_ignore_ascii_case("unknown")) {
            return and_node(vec![
                files,
                Node::Leaf(Pred::Hash(HashPred { algo, prefixes: Vec::new(), unknown: true })),
            ]);
        }
        if let Some(bad) = items.iter().find(|s| !is_hex(s)) {
            let v = Value::Text((*bad).to_string());
            return self.invalid(f, &v);
        }
        let prefixes = items.iter().map(|s| s.trim().to_string()).collect();
        and_node(vec![files, Node::Leaf(Pred::Hash(HashPred { algo, prefixes, unknown: false }))])
    }

    /// Duplicate/set functions (hoisted to [`super::PlannedQuery::post`]).
    pub(super) fn set(&mut self, f: FuncId, t: &Term) -> Node {
        let keys = |v: &Value| parse_dupe_keys(v);
        let op = |kind, keys: Option<Vec<DupeKey>>| Node::Leaf(Pred::Set(SetOp { func: f, kind, keys, negate: false }));
        let fixed = |k: DupeKey| op(SetKind::Dupe, Some(vec![k]));
        match f {
            FuncId::SizeDupe => fixed(DupeKey::Size),
            FuncId::NamePartDupe => fixed(DupeKey::NamePart),
            FuncId::AttribDupe => fixed(DupeKey::Attrib),
            FuncId::DaDupe => fixed(DupeKey::DateAccessed),
            FuncId::DcDupe => fixed(DupeKey::DateCreated),
            FuncId::DmDupe => fixed(DupeKey::DateModified),
            FuncId::Dupe | FuncId::Distinct | FuncId::Unique => match keys(&t.value) {
                Ok(k) => op(
                    match f {
                        FuncId::Dupe => SetKind::Dupe,
                        FuncId::Distinct => SetKind::Distinct,
                        _ => SetKind::Unique,
                    },
                    Some(k),
                ),
                Err(e) => {
                    self.diags.push(PlanDiag::InvalidValue(f, e.clone()));
                    Node::Leaf(Pred::Invalid(e))
                }
            },
            FuncId::DupeCount => {
                let mut n = NumPred::default();
                if Self::int_num(&t.value, &mut n) && t.value != Value::Empty {
                    op(SetKind::DupeCount(n), None)
                } else {
                    self.invalid(f, &t.value)
                }
            }
            FuncId::DupeMax => match t.value.as_text().and_then(|s| s.trim().parse::<u32>().ok()) {
                Some(n) => op(SetKind::DupeMax(n), None),
                None => self.invalid(f, &t.value),
            },
            FuncId::Count => match t.value.as_text().and_then(|s| s.trim().parse::<u64>().ok()) {
                Some(n) => op(SetKind::Limit(n), Some(Vec::new())),
                None => self.invalid(f, &t.value),
            },
            _ => self.invalid(f, &t.value),
        }
    }
}

/// `aspect-ratio:` values: `16:9`, `16/9`, `1.78`, `portrait`, `landscape`, `square`.
fn aspect_span(t: &str, exact: bool) -> Option<(i64, i64)> {
    let lower = t.to_ascii_lowercase();
    match lower.as_str() {
        "portrait" | "tall" => return Some((float_key(0.0), float_key(1.0 - 1e-9))),
        "landscape" | "wide" => return Some((float_key(1.0 + 1e-9), i64::MAX)),
        "square" => return Some((float_key(1.0 - 1e-9), float_key(1.0 + 1e-9))),
        _ => {}
    }
    let r = match lower.split_once([':', '/', 'x']) {
        Some((a, b)) => {
            let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            if b <= 0.0 {
                return None;
            }
            a / b
        }
        None => lower.trim().parse().ok()?,
    };
    if !r.is_finite() {
        return None;
    }
    // Ratios match within 1 % (`16:9` covers 1920×1080 and 1366×768).
    Some(if exact { float_exact(r) } else { (float_key(r * 0.99), float_key(r * 1.01)) })
}

/// Adds the ranges of `op` relative to the span `[lo, hi)`.
fn push_cmp(out: &mut NumPred, op: CmpOp, lo: i64, hi: i64) {
    match op {
        CmpOp::Eq => out.ranges.push((lo, hi)),
        CmpOp::Ne => {
            out.ranges.push((i64::MIN + 1, lo));
            out.ranges.push((hi, i64::MAX));
        }
        CmpOp::Gt => out.ranges.push((hi, i64::MAX)),
        CmpOp::Ge => out.ranges.push((lo, i64::MAX)),
        CmpOp::Lt => out.ranges.push((i64::MIN + 1, lo)),
        CmpOp::Le => out.ranges.push((i64::MIN + 1, hi)),
    }
}

/// Content predicates anywhere in `n` (for `contenthits:`).
pub fn collect_content(n: &Node, out: &mut Vec<ContentPred>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| collect_content(c, out)),
        Node::Not(c) => collect_content(c, out),
        Node::Leaf(Pred::Content(c)) if !out.contains(c) => out.push(c.clone()),
        _ => {}
    }
}

/// `true` if `n` contains a predicate whose matches can change without an index change
/// (properties, content, hashes, file reads: link targets, signatures, recycle bin
/// records) — the executor gives such plans the slow-path state (file budget, caches).
pub fn uses_stores(n: &Node) -> bool {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().any(uses_stores),
        Node::Not(c) => uses_stores(c),
        Node::Leaf(
            Pred::Prop(_)
            | Pred::Content(_)
            | Pred::ContentHits(..)
            | Pred::Hash(_)
            | Pred::LinkTarget(_)
            | Pred::Signature(_)
            | Pred::Recycled(Some(_)),
        ) => true,
        _ => false,
    }
}

/// Moves top-level set functions out of `root` into `post`; nested ones become invalid.
pub fn hoist_sets(root: Node, post: &mut Vec<SetOp>, diags: &mut Vec<PlanDiag>) -> Node {
    fn conjuncts(n: Node, out: &mut Vec<Node>) {
        match n {
            Node::And(v) => v.into_iter().for_each(|c| conjuncts(c, out)),
            other => out.push(other),
        }
    }
    fn nested(n: Node, diags: &mut Vec<PlanDiag>) -> Node {
        match n {
            Node::And(v) => and_node(v.into_iter().map(|c| nested(c, diags)).collect()),
            Node::Or(v) => or_node(v.into_iter().map(|c| nested(c, diags)).collect()),
            Node::Not(c) => Node::Not(Box::new(nested(*c, diags))),
            Node::Leaf(Pred::Set(op)) => {
                let msg = format!("{}: only at the top level of a search", op.func.name());
                diags.push(PlanDiag::InvalidValue(op.func, msg.clone()));
                Node::Leaf(Pred::Invalid(msg))
            }
            other => other,
        }
    }
    let mut parts = Vec::new();
    conjuncts(root, &mut parts);
    let mut rest = Vec::new();
    for p in parts {
        match p {
            Node::Leaf(Pred::Set(op)) => post.push(op),
            Node::Not(inner) if matches!(*inner, Node::Leaf(Pred::Set(_))) => {
                if let Node::Leaf(Pred::Set(mut op)) = *inner {
                    op.negate = true;
                    post.push(op);
                }
            }
            other => rest.push(nested(other, diags)),
        }
    }
    // The result limit (`count:`) applies last, whatever its position in the search.
    let (mut limits, sets): (Vec<SetOp>, Vec<SetOp>) =
        std::mem::take(post).into_iter().partition(|o| matches!(o.kind, SetKind::Limit(_)));
    *post = sets;
    // `dupe-count:` / `dupe-max:` group like the query's other set function.
    let default_keys = post.iter().find_map(|o| o.keys.clone()).unwrap_or_else(|| vec![DupeKey::Name]);
    for op in post.iter_mut() {
        if op.keys.is_none() {
            op.keys = Some(default_keys.clone());
        }
    }
    for l in &mut limits {
        l.negate = false;
    }
    post.append(&mut limits);
    and_node(rest)
}

/// `contenthits:` needs the query's content patterns; fills them in (or invalidates).
pub fn resolve_content_hits(n: Node, contents: &[ContentPred], diags: &mut Vec<PlanDiag>) -> Node {
    match n {
        Node::And(v) => Node::And(v.into_iter().map(|c| resolve_content_hits(c, contents, diags)).collect()),
        Node::Or(v) => Node::Or(v.into_iter().map(|c| resolve_content_hits(c, contents, diags)).collect()),
        Node::Not(c) => Node::Not(Box::new(resolve_content_hits(*c, contents, diags))),
        Node::Leaf(Pred::ContentHits(n, _)) => {
            if contents.is_empty() {
                let msg = "contenthits: needs a content: term".to_string();
                diags.push(PlanDiag::InvalidValue(FuncId::ContentHits, msg.clone()));
                Node::Leaf(Pred::Invalid(msg))
            } else {
                Node::Leaf(Pred::ContentHits(n, contents.to_vec()))
            }
        }
        other => other,
    }
}

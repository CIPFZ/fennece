//! Query planning: AST → typed predicate tree with costs (SPEC ch.2 §8).
//!
//! The planner resolves modifiers against the defaults, interprets function values
//! (sizes, dates, attribute letters, ...), orders AND children by estimated cost so
//! cheap, selective column predicates run before string matching, and decides whether
//! the previous result set can be reused as input (SPEC ch.1 §6.1).

use fnc_core::fold::{FoldOptions, fold_str_into};

use crate::ast::{CmpOp, Expr, ModSet, Modifier, Term, Value};
use crate::date::{DateEnv, parse_date};
use crate::funcs::{FuncId, MacroId, builtin_macro_exts};
use crate::size::parse_size;

/// How literal text is matched.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PatternKind {
    /// Substring anywhere (default).
    Substring,
    /// `*`/`?` wildcards (whole-string match).
    Wildcard,
    /// Regular expression (`regex:`).
    Regex,
    /// Whole string equals (`wfn:` / `exact:`).
    Exact,
    /// Starts with (`^abc`, `startwith:`).
    StartsWith,
    /// Ends with (`abc$`, `endwith:`).
    EndsWith,
}

/// What the text is matched against.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// File name.
    Name,
    /// Full path (`path:`, terms containing `\`).
    FullPath,
    /// Folder part of the path (`pathpart:`).
    PathPart,
    /// Name without extension (`stem:`).
    Stem,
    /// Extension (`ext:` with wildcards).
    Ext,
}

/// Text predicate options resolved from modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextPred {
    /// Pattern as typed (not folded).
    pub pattern: String,
    /// Matching style.
    pub kind: PatternKind,
    /// Match target.
    pub target: Target,
    /// `case:`
    pub case: bool,
    /// `diacritics:`
    pub diacritics: bool,
    /// `ww:`
    pub whole_word: bool,
    /// `prefix:` (match at word start)
    pub prefix: bool,
    /// `suffix:` (match at word end)
    pub suffix: bool,
    /// `ignorepunctuation:`
    pub ignore_punct: bool,
    /// `ignorewhitespace:`
    pub ignore_ws: bool,
    /// `*` also matches `\`.
    pub star_all: bool,
    /// `wildcards:` (enables `[set]` syntax).
    pub wildcard_sets: bool,
    /// `dotall:` / `multiline:` for regex.
    pub regex_flags: (bool, bool),
    /// Participates in highlighting.
    pub highlight: bool,
}

impl TextPred {
    /// Fold options implied by `case:`/`diacritics:`.
    pub fn fold_options(&self) -> FoldOptions {
        FoldOptions { fold_case: !self.case, strip_diacritics: !self.diacritics }
    }

    /// Pattern folded with [`TextPred::fold_options`].
    pub fn folded_pattern(&self) -> String {
        let mut s = String::with_capacity(self.pattern.len());
        fold_str_into(&self.pattern, self.fold_options(), &mut s);
        s
    }
}

/// Union of half-open ranges over an `i64` domain (sizes, FILETIMEs, counts).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct NumPred {
    /// `[lo, hi)` ranges.
    pub ranges: Vec<(i64, i64)>,
    /// Match entries whose value is unknown.
    pub unknown: bool,
}

impl NumPred {
    /// `true` if `v` (known) matches.
    #[inline]
    pub fn matches(&self, v: i64) -> bool {
        self.ranges.iter().any(|&(lo, hi)| v >= lo && v < hi)
    }
}

/// Date column selector.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DateCol {
    /// Date modified.
    Modified,
    /// Date created.
    Created,
    /// Date accessed.
    Accessed,
    /// Date run.
    Run,
    /// Date recently changed.
    RecentlyChanged,
}

/// Numeric per-entry property.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum NumProp {
    /// File size.
    Size,
    /// A date column.
    Date(DateCol),
    /// Depth (root = 0).
    Depth,
    /// Name length (UTF-16 units).
    NameLength,
    /// Folder-part length.
    PathLength,
    /// Full path length.
    FullPathLength,
    /// Run count.
    RunCount,
    /// FRN.
    Frn,
    /// Direct children.
    ChildCount,
    /// Direct child files.
    ChildFileCount,
    /// Direct child folders.
    ChildFolderCount,
}

/// Leaf predicate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pred {
    /// Text match.
    Text(TextPred),
    /// Extension in set (folded, no dot); `none` = entries without extension.
    Ext {
        /// Extensions.
        exts: Vec<String>,
        /// Also match entries with no extension.
        none: bool,
    },
    /// Folders (`true`) or files (`false`).
    IsDir(bool),
    /// Numeric property in ranges.
    Num(NumProp, NumPred),
    /// Attribute bits: all of `mask` set (`exact`: attributes equal `mask` within the
    /// letters' universe).
    Attrib {
        /// Required bits.
        mask: u32,
        /// Exact comparison.
        exact: bool,
    },
    /// Volume roots.
    Root,
    /// Empty folders or zero-byte files.
    Empty,
    /// Parent folder's full path matches.
    Parent(TextPred),
    /// Some ancestor folder's full path matches.
    Ancestor(TextPred),
    /// Folder has a direct child whose name matches.
    Child(TextPred),
    /// Volume root path starts with (folded) text.
    Volume(String),
    /// Volume kind name (`ntfs`, `folder`, ...).
    VolumeKind(String),
    /// Volume online (`true`) / offline.
    Online(bool),
    /// Reparse point (any / folder junction / file symlink).
    Reparse(ReparseKind),
    /// Category (`kind:`): extensions of the category, or folders.
    Kind(KindClass),
    /// Function not implemented yet: matches nothing, reported in diagnostics.
    Unsupported(FuncId),
    /// Value could not be interpreted: matches nothing.
    Invalid(String),
}

/// Reparse point flavour.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReparseKind {
    /// Any reparse point.
    Any,
    /// Directory reparse point.
    // SPEC-GAP: true junction/symlink distinction needs the reparse tag (not indexed);
    // folders are reported as junctions and files as symlinks.
    Junction,
    /// File reparse point.
    Symlink,
}

/// `kind:` categories.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KindClass {
    /// Folders.
    Folder,
    /// Extension set.
    Exts(Vec<String>),
}

/// Plan node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// Everything.
    All,
    /// Nothing.
    Nothing,
    /// Conjunction (children ordered by cost).
    And(Vec<Node>),
    /// Disjunction.
    Or(Vec<Node>),
    /// Negation.
    Not(Box<Node>),
    /// Leaf.
    Leaf(Pred),
}

/// Where evaluation starts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputSet {
    /// Scan the whole index.
    All,
    /// Filter the previous result set (the new query only narrows it).
    Previous,
}

/// Planner diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanDiag {
    /// A known function that this build cannot evaluate yet.
    Unsupported(FuncId),
    /// A value that could not be parsed.
    InvalidValue(FuncId, String),
}

/// Defaults and context for planning.
#[derive(Clone, Debug)]
pub struct PlanEnv {
    /// Default modifiers (from settings: match case, whole word, path, diacritics).
    pub defaults: ModSet,
    /// Settings `wildcards_star_all`.
    pub star_all: bool,
    /// Date context.
    pub date: DateEnv,
}

impl Default for PlanEnv {
    fn default() -> Self {
        Self { defaults: ModSet::EMPTY, star_all: false, date: DateEnv::now() }
    }
}

/// Output of [`plan`].
#[derive(Clone, Debug)]
pub struct PlannedQuery {
    /// Predicate tree.
    pub root: Node,
    /// Input set.
    pub input: InputSet,
    /// Problems found while planning.
    pub diagnostics: Vec<PlanDiag>,
}

/// Plans `ast`. If `prev` is given and the new plan only narrows it, the result's input
/// is [`InputSet::Previous`].
pub fn plan(ast: &Expr, env: &PlanEnv, prev: Option<&PlannedQuery>) -> PlannedQuery {
    let mut cx = Cx { env, diags: Vec::new() };
    let mut root = cx.node(ast);
    order_by_cost(&mut root);
    let input = match prev {
        Some(p) if narrows(&root, &p.root) => InputSet::Previous,
        _ => InputSet::All,
    };
    PlannedQuery { root, input, diagnostics: cx.diags }
}

struct Cx<'e> {
    env: &'e PlanEnv,
    diags: Vec<PlanDiag>,
}

fn and_node(mut v: Vec<Node>) -> Node {
    v.retain(|n| *n != Node::All);
    if v.contains(&Node::Nothing) {
        return Node::Nothing;
    }
    match v.len() {
        0 => Node::All,
        1 => v.pop().unwrap_or(Node::All),
        _ => Node::And(v),
    }
}

fn or_node(mut v: Vec<Node>) -> Node {
    v.retain(|n| *n != Node::Nothing);
    if v.contains(&Node::All) {
        return Node::All;
    }
    match v.len() {
        0 => Node::Nothing,
        1 => v.pop().unwrap_or(Node::Nothing),
        _ => Node::Or(v),
    }
}

const ATTRIB_LETTERS: &[(char, u32)] = &[
    ('r', 0x1),
    ('h', 0x2),
    ('s', 0x4),
    ('d', 0x10),
    ('a', 0x20),
    ('v', 0x40),
    ('n', 0x80),
    ('t', 0x100),
    ('p', 0x200),
    ('l', 0x400),
    ('c', 0x800),
    ('o', 0x1000),
    ('i', 0x2000),
    ('e', 0x4000),
    ('u', 0x8000),
];

/// Universe of attribute bits addressable by letters (for `exact:attrib:`).
pub const ATTRIB_UNIVERSE: u32 = 0xFFF7;

impl Cx<'_> {
    fn node(&mut self, e: &Expr) -> Node {
        match e {
            Expr::All => Node::All,
            Expr::And(v) => and_node(v.iter().map(|e| self.node(e)).collect()),
            Expr::Or(v) => or_node(v.iter().map(|e| self.node(e)).collect()),
            Expr::Not(inner) => match self.node(inner) {
                Node::All => Node::Nothing,
                Node::Nothing => Node::All,
                Node::Not(n) => *n,
                n => Node::Not(Box::new(n)),
            },
            Expr::Macro(_, body) => self.node(body),
            Expr::Term(t) => self.term(t),
        }
    }

    fn on(&self, mods: ModSet, m: Modifier) -> bool {
        mods.get(m).or_else(|| self.env.defaults.get(m)).unwrap_or(false)
    }

    fn text_pred(&self, mods: ModSet, text: &str, target: Target) -> TextPred {
        let mut pattern = text.to_string();
        let mut target = target;
        let regex = self.on(mods, Modifier::Regex);
        let wildcard_sets = self.on(mods, Modifier::Wildcards);
        let mut kind = PatternKind::Substring;
        if regex {
            kind = PatternKind::Regex;
        } else {
            if target == Target::Name && (pattern.contains('\\') || pattern.contains('/')) {
                target = Target::FullPath;
            }
            if target == Target::FullPath || target == Target::PathPart {
                pattern = pattern.replace('/', "\\");
            }
            if pattern.contains(['*', '?']) || (wildcard_sets && pattern.contains('[')) {
                kind = PatternKind::Wildcard;
            } else if self.on(mods, Modifier::WholeFilename) || self.on(mods, Modifier::WholePath) {
                kind = PatternKind::Exact;
                if self.on(mods, Modifier::WholePath) {
                    target = Target::FullPath;
                }
            } else if self.on(mods, Modifier::StartWith) {
                kind = PatternKind::StartsWith;
            } else if self.on(mods, Modifier::EndWith) {
                kind = PatternKind::EndsWith;
            } else if pattern.len() > 1 && pattern.starts_with('^') && !pattern.ends_with('$') {
                kind = PatternKind::StartsWith;
                pattern.remove(0);
            } else if pattern.len() > 1 && pattern.ends_with('$') && !pattern.starts_with('^') {
                kind = PatternKind::EndsWith;
                pattern.pop();
            } else if pattern.len() > 2 && pattern.starts_with('^') && pattern.ends_with('$') {
                kind = PatternKind::Exact;
                pattern.remove(0);
                pattern.pop();
            }
        }
        if target == Target::Name && self.on(mods, Modifier::Path) {
            target = Target::FullPath;
        }
        TextPred {
            pattern,
            kind,
            target,
            case: self.on(mods, Modifier::Case),
            diacritics: self.on(mods, Modifier::Diacritics),
            whole_word: self.on(mods, Modifier::WholeWord),
            prefix: self.on(mods, Modifier::Prefix),
            suffix: self.on(mods, Modifier::Suffix),
            ignore_punct: self.on(mods, Modifier::IgnorePunctuation),
            ignore_ws: self.on(mods, Modifier::IgnoreWhitespace),
            star_all: self.env.star_all || self.on(mods, Modifier::WildcardsStarAll),
            wildcard_sets,
            regex_flags: (self.on(mods, Modifier::DotAll), self.on(mods, Modifier::Multiline)),
            highlight: mods.get(Modifier::Highlight).unwrap_or(true),
        }
    }

    fn term(&mut self, t: &Term) -> Node {
        let n = match t.func {
            None => self.plain(t),
            Some(f) => self.function(f, t),
        };
        let restrict = if self.on(t.mods, Modifier::FileOnly) {
            Some(false)
        } else if self.on(t.mods, Modifier::FolderOnly) {
            Some(true)
        } else {
            None
        };
        match restrict {
            Some(dir) => and_node(vec![Node::Leaf(Pred::IsDir(dir)), n]),
            None => n,
        }
    }

    fn plain(&mut self, t: &Term) -> Node {
        let text = match &t.value {
            Value::Text(s) => s.as_str(),
            Value::Empty => "",
            _ => return Node::All,
        };
        if self.on(t.mods, Modifier::Len) {
            return self.num_node(FuncId::NameLength, NumProp::NameLength, &t.value);
        }
        if text == "\\" && !self.on(t.mods, Modifier::Regex) {
            // A lone backslash is a fast "folders only" search (SPEC ch.2 §2.3).
            return Node::Leaf(Pred::IsDir(true));
        }
        if text.is_empty() {
            return Node::All;
        }
        Node::Leaf(Pred::Text(self.text_pred(t.mods, text, Target::Name)))
    }

    /// Text predicates for every list item of a text function, OR-ed.
    fn text_items(&mut self, t: &Term, target: Target, wrap: impl Fn(TextPred) -> Pred) -> Node {
        let items: Vec<&str> = match &t.value {
            Value::Text(s) => vec![s.as_str()],
            Value::List(v) => v.iter().filter_map(Value::as_text).collect(),
            Value::Empty => return Node::All,
            other => {
                self.diags.push(PlanDiag::InvalidValue(t.func.unwrap_or(FuncId::Name), format!("{other:?}")));
                return Node::Nothing;
            }
        };
        or_node(items.into_iter().map(|s| Node::Leaf(wrap(self.text_pred(t.mods, s, target)))).collect())
    }

    /// Folder-path predicates (`parent:`, `ancestor:`): exact path, trailing separator
    /// optional; wildcards allowed.
    fn folder_items(&mut self, t: &Term, wrap: impl Fn(TextPred) -> Pred) -> Node {
        let items: Vec<String> = match &t.value {
            Value::Text(s) => vec![s.clone()],
            Value::List(v) => v.iter().filter_map(Value::as_text).map(String::from).collect(),
            _ => return Node::Nothing,
        };
        or_node(
            items
                .into_iter()
                .map(|s| {
                    let trimmed = s.trim_end_matches(['\\', '/']);
                    let trimmed = if trimmed.is_empty() { s.as_str() } else { trimmed };
                    let mut p = self.text_pred(t.mods, trimmed, Target::FullPath);
                    if p.kind == PatternKind::Substring {
                        p.kind = PatternKind::Exact;
                    }
                    Node::Leaf(wrap(p))
                })
                .collect(),
        )
    }

    fn function(&mut self, f: FuncId, t: &Term) -> Node {
        use FuncId as F;
        let leaf = |p: Pred| Node::Leaf(p);
        match f {
            F::File | F::Folder => {
                let dir = f == F::Folder;
                let base = leaf(Pred::IsDir(dir));
                match &t.value {
                    Value::Empty => base,
                    _ => and_node(vec![base, self.text_items(t, Target::Name, Pred::Text)]),
                }
            }
            F::Name => self.text_items(t, Target::Name, Pred::Text),
            F::Stem => self.text_items(t, Target::Stem, Pred::Text),
            F::FullPath => self.text_items(t, Target::FullPath, Pred::Text),
            F::PathPart => self.text_items(t, Target::PathPart, Pred::Text),
            F::Ext => self.ext(t),
            F::Parent => self.folder_items(t, Pred::Parent),
            F::Ancestor => self.folder_items(t, Pred::Ancestor),
            F::Child => self.text_items(t, Target::Name, Pred::Child),
            F::ChildCount => self.num_node(f, NumProp::ChildCount, &t.value),
            F::ChildFileCount => self.num_node(f, NumProp::ChildFileCount, &t.value),
            F::ChildFolderCount => self.num_node(f, NumProp::ChildFolderCount, &t.value),
            F::Depth => self.num_node(f, NumProp::Depth, &t.value),
            F::Empty => leaf(Pred::Empty),
            F::Root => leaf(Pred::Root),
            F::NameLength => self.num_node(f, NumProp::NameLength, &t.value),
            F::PathLength => self.num_node(f, NumProp::PathLength, &t.value),
            F::FullPathLength => self.num_node(f, NumProp::FullPathLength, &t.value),
            F::Size => self.num_node(f, NumProp::Size, &t.value),
            F::DateModified => self.num_node(f, NumProp::Date(DateCol::Modified), &t.value),
            F::DateCreated => self.num_node(f, NumProp::Date(DateCol::Created), &t.value),
            F::DateAccessed => self.num_node(f, NumProp::Date(DateCol::Accessed), &t.value),
            F::DateRun => self.num_node(f, NumProp::Date(DateCol::Run), &t.value),
            F::DateRecentlyChanged => self.num_node(f, NumProp::Date(DateCol::RecentlyChanged), &t.value),
            F::RunCount => self.num_node(f, NumProp::RunCount, &t.value),
            F::Frn => self.num_node(f, NumProp::Frn, &t.value),
            F::Attrib => self.attrib(t),
            F::Volume => match &t.value {
                Value::Text(s) => {
                    let mut v = String::new();
                    fold_str_into(s.trim_end_matches([':', '\\', '/']), FoldOptions::DEFAULT, &mut v);
                    leaf(Pred::Volume(v))
                }
                _ => Node::Nothing,
            },
            F::VolumeKind => match &t.value {
                Value::Text(s) => leaf(Pred::VolumeKind(s.to_ascii_lowercase())),
                _ => Node::Nothing,
            },
            F::Online => leaf(Pred::Online(true)),
            F::Offline => leaf(Pred::Online(false)),
            F::Reparse => leaf(Pred::Reparse(ReparseKind::Any)),
            F::Junction => leaf(Pred::Reparse(ReparseKind::Junction)),
            F::Symlink => leaf(Pred::Reparse(ReparseKind::Symlink)),
            F::Kind => self.kind(t),
            other => {
                self.diags.push(PlanDiag::Unsupported(other));
                leaf(Pred::Unsupported(other))
            }
        }
    }

    fn ext(&mut self, t: &Term) -> Node {
        let items: Vec<&str> = match &t.value {
            Value::Empty => return Node::Leaf(Pred::Ext { exts: Vec::new(), none: true }),
            Value::Text(s) => vec![s.as_str()],
            Value::List(v) => v.iter().filter_map(Value::as_text).collect(),
            _ => return Node::Nothing,
        };
        let mut exts = Vec::new();
        let mut wild = Vec::new();
        for item in items {
            let item = item.trim_start_matches('.');
            if item.contains(['*', '?']) {
                wild.push(Node::Leaf(Pred::Text(self.text_pred(t.mods, item, Target::Ext))));
            } else {
                let mut f = String::new();
                fold_str_into(item, FoldOptions::DEFAULT, &mut f);
                exts.push(f);
            }
        }
        exts.sort();
        exts.dedup();
        let mut alts = wild;
        if !exts.is_empty() {
            alts.insert(0, Node::Leaf(Pred::Ext { exts, none: false }));
        }
        or_node(alts)
    }

    fn kind(&mut self, t: &Term) -> Node {
        let Some(name) = t.value.as_text() else { return Node::Nothing };
        let m = match name.to_ascii_lowercase().as_str() {
            "folder" | "folders" | "directory" => return Node::Leaf(Pred::Kind(KindClass::Folder)),
            "audio" | "music" => MacroId::Audio,
            "video" | "movie" => MacroId::Video,
            "image" | "picture" | "pic" | "photo" => MacroId::Image,
            "document" | "doc" => MacroId::Doc,
            "executable" | "exe" | "program" => MacroId::Exe,
            "archive" | "compressed" | "zip" => MacroId::Zip,
            "shortcut" | "link" => {
                return Node::Leaf(Pred::Kind(KindClass::Exts(vec!["lnk".into(), "url".into()])));
            }
            _ => {
                self.diags.push(PlanDiag::InvalidValue(FuncId::Kind, name.to_string()));
                return Node::Nothing;
            }
        };
        let exts = builtin_macro_exts(m).split(';').map(String::from).collect();
        Node::Leaf(Pred::Kind(KindClass::Exts(exts)))
    }

    fn attrib(&mut self, t: &Term) -> Node {
        let Some(letters) = t.value.as_text() else {
            return match t.value {
                Value::Empty => Node::All,
                _ => Node::Nothing,
            };
        };
        let mut mask = 0u32;
        for c in letters.chars() {
            match ATTRIB_LETTERS.iter().find(|(l, _)| *l == c.to_ascii_lowercase()) {
                Some(&(_, bit)) => mask |= bit,
                None => {
                    self.diags.push(PlanDiag::InvalidValue(FuncId::Attrib, letters.to_string()));
                    return Node::Leaf(Pred::Invalid(letters.to_string()));
                }
            }
        }
        Node::Leaf(Pred::Attrib { mask, exact: self.on(t.mods, Modifier::WholeFilename) })
    }

    /// Span of a scalar value for numeric property `prop`.
    fn span(&self, prop: NumProp, text: &str) -> Option<(i64, i64)> {
        match prop {
            NumProp::Size => parse_size(text).map(|s| (clamp_u64(s.lo), clamp_u64(s.hi))),
            NumProp::Date(_) => parse_date(text, &self.env.date).map(|s| (s.lo, s.hi)),
            _ => {
                let t = text.trim();
                let v: i64 = t.parse().ok()?;
                Some((v, v.saturating_add(1)))
            }
        }
    }

    fn num_pred(&self, prop: NumProp, v: &Value, out: &mut NumPred) -> bool {
        match v {
            Value::Empty => {
                // `size:` / `dm:` with no value: non-zero / known.
                out.ranges.push(match prop {
                    NumProp::Date(_) => (i64::MIN + 1, i64::MAX),
                    _ => (1, i64::MAX),
                });
                true
            }
            Value::Text(s) if s.trim().eq_ignore_ascii_case("unknown") => {
                out.unknown = true;
                true
            }
            Value::Text(s) => {
                if let Some(r) = self.span(prop, s) {
                    out.ranges.push(r);
                    return true;
                }
                // `a-b` range for sizes and numbers.
                if !matches!(prop, NumProp::Date(_))
                    && let Some((a, b)) = s.split_once('-')
                    && let (Some(lo), Some(hi)) = (self.span(prop, a), self.span(prop, b))
                {
                    out.ranges.push((lo.0, hi.1));
                    return true;
                }
                false
            }
            Value::Range(a, b) => {
                let lo = match a.as_text().map(str::trim) {
                    Some("") => Some(i64::MIN + 1),
                    Some(t) => self.span(prop, t).map(|r| r.0),
                    None => None,
                };
                let hi = match b.as_text().map(str::trim) {
                    Some("") => Some(i64::MAX),
                    Some(t) => self.span(prop, t).map(|r| r.1),
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
                let Some((lo, hi)) = self.span(prop, text) else { return false };
                // SPEC-GAP: sizes compare against the exact value (`size:>1mb` is
                // > 1048576 bytes); dates and counts compare against the whole span.
                let (lo, hi) = if prop == NumProp::Size { (lo, lo.saturating_add(1)) } else { (lo, hi) };
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
                true
            }
            Value::List(items) => items.iter().all(|i| self.num_pred(prop, i, out)),
        }
    }

    fn num_node(&mut self, f: FuncId, prop: NumProp, v: &Value) -> Node {
        let mut pred = NumPred::default();
        if self.num_pred(prop, v, &mut pred) {
            Node::Leaf(Pred::Num(prop, pred))
        } else {
            let text = format!("{v:?}");
            self.diags.push(PlanDiag::InvalidValue(f, text.clone()));
            Node::Leaf(Pred::Invalid(text))
        }
    }
}

fn clamp_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Estimated evaluation cost per entry (lower runs first).
pub fn cost(n: &Node) -> u32 {
    match n {
        Node::All | Node::Nothing => 0,
        Node::And(v) | Node::Or(v) => v.iter().map(cost).sum(),
        Node::Not(c) => cost(c),
        Node::Leaf(p) => match p {
            Pred::IsDir(_) | Pred::Root | Pred::Unsupported(_) | Pred::Invalid(_) => 1,
            Pred::Ext { .. } | Pred::Attrib { .. } | Pred::Kind(_) | Pred::Online(_) | Pred::VolumeKind(_) => 2,
            Pred::Reparse(_) | Pred::Volume(_) | Pred::Empty => 3,
            Pred::Num(prop, _) => match prop {
                NumProp::Size | NumProp::Date(_) | NumProp::Frn | NumProp::NameLength => 3,
                NumProp::RunCount | NumProp::ChildCount | NumProp::ChildFileCount | NumProp::ChildFolderCount => 6,
                NumProp::Depth | NumProp::PathLength | NumProp::FullPathLength => 12,
            },
            Pred::Text(t) => {
                let base = match t.kind {
                    PatternKind::Exact | PatternKind::StartsWith | PatternKind::EndsWith => 4,
                    PatternKind::Substring => 5,
                    PatternKind::Wildcard => 7,
                    PatternKind::Regex => 20,
                };
                let target = match t.target {
                    Target::Name | Target::Stem | Target::Ext => 0,
                    Target::FullPath | Target::PathPart => 10,
                };
                base + target + if t.case || t.diacritics || t.ignore_punct || t.ignore_ws { 6 } else { 0 }
            }
            Pred::Parent(_) | Pred::Ancestor(_) | Pred::Child(_) => 9,
        },
    }
}

fn order_by_cost(n: &mut Node) {
    match n {
        Node::And(v) => {
            v.iter_mut().for_each(order_by_cost);
            v.sort_by_key(cost);
        }
        Node::Or(v) => v.iter_mut().for_each(order_by_cost),
        Node::Not(c) => order_by_cost(c),
        _ => {}
    }
}

fn conjuncts(n: &Node) -> &[Node] {
    match n {
        Node::And(v) => v,
        Node::All => &[],
        other => std::slice::from_ref(other),
    }
}

/// `true` if every entry matching `a` also matches `b` (conservative).
fn implies(a: &Node, b: &Node) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Node::Leaf(Pred::Text(x)), Node::Leaf(Pred::Text(y))) => {
            x.kind == PatternKind::Substring
                && y.kind == PatternKind::Substring
                && x.target == y.target
                && x.case == y.case
                && x.diacritics == y.diacritics
                && x.whole_word == y.whole_word
                && !x.whole_word
                && x.prefix == y.prefix
                && x.suffix == y.suffix
                && !x.prefix
                && !x.suffix
                && x.ignore_punct == y.ignore_punct
                && x.ignore_ws == y.ignore_ws
                && x.folded_pattern().contains(&y.folded_pattern())
        }
        (Node::Leaf(Pred::Ext { exts: xa, none: na }), Node::Leaf(Pred::Ext { exts: xb, none: nb })) => {
            (!na || *nb) && xa.iter().all(|e| xb.contains(e))
        }
        _ => false,
    }
}

/// `true` if `new` selects a subset of `old` (used for result-set reuse).
pub fn narrows(new: &Node, old: &Node) -> bool {
    if *old == Node::All {
        return true;
    }
    let new_c = conjuncts(new);
    conjuncts(old).iter().all(|o| new_c.iter().any(|n| implies(n, o)))
}

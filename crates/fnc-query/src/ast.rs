//! Abstract syntax tree of a search.

use std::fmt;
use std::ops::Range;

use crate::funcs::{FuncId, MacroId};

/// Search modifiers (SPEC ch.2 §3). Each has an on and an off (`no…:`) form.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Modifier {
    /// `case:`
    Case,
    /// `diacritics:`
    Diacritics,
    /// `path:` (as a modifier: match the full path)
    Path,
    /// `wholeword:` / `ww:`
    WholeWord,
    /// `wholefilename:` / `wfn:` / `exact:`
    WholeFilename,
    /// `wholepath:` / `wp:`
    WholePath,
    /// `regex:`
    Regex,
    /// `wildcards:`
    Wildcards,
    /// `wildcardsstarall:`
    WildcardsStarAll,
    /// `prefix:`
    Prefix,
    /// `suffix:`
    Suffix,
    /// `startwith:` / `sw:`
    StartWith,
    /// `endwith:` / `ew:`
    EndWith,
    /// `ignorepunctuation:`
    IgnorePunctuation,
    /// `ignorewhitespace:`
    IgnoreWhitespace,
    /// `utf8:`
    Utf8,
    /// `ansi:`
    Ansi,
    /// `utf16:`
    Utf16,
    /// `utf16be:`
    Utf16Be,
    /// `binary:` / `bytestream:`
    Binary,
    /// `ascii:`
    Ascii,
    /// `dotall:`
    DotAll,
    /// `multiline:`
    Multiline,
    /// `atoi:` / `tointeger:`
    Atoi,
    /// `atof:` / `tonumber:`
    Atof,
    /// `allstrings:`
    AllStrings,
    /// `anystrings:`
    AnyStrings,
    /// `fileonly:`
    FileOnly,
    /// `folderonly:`
    FolderOnly,
    /// `highlight:`
    Highlight,
    /// `len:` (compare value length)
    Len,
    /// `count:` (compare number of values)
    Count,
    /// `notindexed:`
    NotIndexed,
    /// `pinyin:` / `py:`
    Pinyin,
    /// `pinyinquan:` (full pinyin)
    PinyinQuan,
    /// `pinyinjian:` (initials)
    PinyinJian,
    /// `fuzzy:` (Fennec extension)
    Fuzzy,
}

impl Modifier {
    /// Number of modifiers.
    pub const COUNT: usize = Modifier::Fuzzy as usize + 1;

    /// Canonical spelling (without colon).
    pub fn name(self) -> &'static str {
        use Modifier::*;
        match self {
            Case => "case",
            Diacritics => "diacritics",
            Path => "path",
            WholeWord => "ww",
            WholeFilename => "wfn",
            WholePath => "wholepath",
            Regex => "regex",
            Wildcards => "wildcards",
            WildcardsStarAll => "wildcardsstarall",
            Prefix => "prefix",
            Suffix => "suffix",
            StartWith => "startwith",
            EndWith => "endwith",
            IgnorePunctuation => "ignorepunctuation",
            IgnoreWhitespace => "ignorewhitespace",
            Utf8 => "utf8",
            Ansi => "ansi",
            Utf16 => "utf16",
            Utf16Be => "utf16be",
            Binary => "binary",
            Ascii => "ascii",
            DotAll => "dotall",
            Multiline => "multiline",
            Atoi => "atoi",
            Atof => "atof",
            AllStrings => "allstrings",
            AnyStrings => "anystrings",
            FileOnly => "fileonly",
            FolderOnly => "folderonly",
            Highlight => "highlight",
            Len => "len",
            Count => "count",
            NotIndexed => "notindexed",
            Pinyin => "pinyin",
            PinyinQuan => "pinyinquan",
            PinyinJian => "pinyinjian",
            Fuzzy => "fuzzy",
        }
    }

    /// All modifiers.
    pub fn all() -> impl Iterator<Item = Modifier> {
        (0..Self::COUNT as u8).filter_map(Self::from_u8)
    }

    fn from_u8(v: u8) -> Option<Modifier> {
        use Modifier::*;
        const ALL: [Modifier; Modifier::COUNT] = [
            Case,
            Diacritics,
            Path,
            WholeWord,
            WholeFilename,
            WholePath,
            Regex,
            Wildcards,
            WildcardsStarAll,
            Prefix,
            Suffix,
            StartWith,
            EndWith,
            IgnorePunctuation,
            IgnoreWhitespace,
            Utf8,
            Ansi,
            Utf16,
            Utf16Be,
            Binary,
            Ascii,
            DotAll,
            Multiline,
            Atoi,
            Atof,
            AllStrings,
            AnyStrings,
            FileOnly,
            FolderOnly,
            Highlight,
            Len,
            Count,
            NotIndexed,
            Pinyin,
            PinyinQuan,
            PinyinJian,
            Fuzzy,
        ];
        ALL.get(v as usize).copied()
    }
}

/// Tri-state modifier set: each modifier is unset (use the default), on or off.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct ModSet {
    set: u64,
    on: u64,
}

impl ModSet {
    /// No modifier specified.
    pub const EMPTY: ModSet = ModSet { set: 0, on: 0 };

    /// Sets `m` to `on`.
    pub fn set(&mut self, m: Modifier, on: bool) {
        let bit = 1u64 << (m as u8);
        self.set |= bit;
        if on {
            self.on |= bit;
        } else {
            self.on &= !bit;
        }
    }

    /// Builder form of [`ModSet::set`].
    #[must_use]
    pub fn with(mut self, m: Modifier, on: bool) -> Self {
        self.set(m, on);
        self
    }

    /// `Some(on)` if specified.
    pub fn get(&self, m: Modifier) -> Option<bool> {
        let bit = 1u64 << (m as u8);
        (self.set & bit != 0).then_some(self.on & bit != 0)
    }

    /// Effective value with fallback `default`.
    pub fn is(&self, m: Modifier, default: bool) -> bool {
        self.get(m).unwrap_or(default)
    }

    /// `true` if nothing is specified.
    pub fn is_empty(&self) -> bool {
        self.set == 0
    }

    /// Overlays `other` on `self` (`other` wins where both specify).
    #[must_use]
    pub fn overlay(self, other: ModSet) -> ModSet {
        ModSet { set: self.set | other.set, on: (self.on & !other.set) | (other.on & other.set) }
    }

    /// Specified modifiers with their state.
    pub fn iter(&self) -> impl Iterator<Item = (Modifier, bool)> + '_ {
        Modifier::all().filter_map(|m| self.get(m).map(|on| (m, on)))
    }
}

impl fmt::Debug for ModSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut l = f.debug_list();
        for (m, on) in self.iter() {
            if on {
                l.entry(&m.name());
            } else {
                l.entry(&format!("no{}", m.name()));
            }
        }
        l.finish()
    }
}

/// Comparison operator in a function value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    /// `=`
    Eq,
    /// `!=` / `!`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl CmpOp {
    /// Operator text.
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

/// Value of a term.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// Text (quotes removed, entities decoded).
    Text(String),
    /// `a;b;c`
    List(Vec<Value>),
    /// `a..b`
    Range(Box<Value>, Box<Value>),
    /// `>v`, `<=v`, ...
    Cmp(CmpOp, Box<Value>),
    /// Nothing after the colon (`ext:`, `folder:`).
    Empty,
}

impl Value {
    /// Text of a plain text value.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// One search term: `[mods:]* [func:] value`.
#[derive(Clone, Debug)]
pub struct Term {
    /// Modifiers (term-local overlaid on global ones).
    pub mods: ModSet,
    /// Function, `None` for plain text.
    pub func: Option<FuncId>,
    /// Value.
    pub value: Value,
    /// Byte range of the term in the input.
    pub raw: Range<usize>,
}

impl PartialEq for Term {
    /// Structural equality (ignores `raw`).
    fn eq(&self, other: &Self) -> bool {
        self.mods == other.mods && self.func == other.func && self.value == other.value
    }
}

/// Search expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Matches everything (empty search).
    All,
    /// All children must match.
    And(Vec<Expr>),
    /// Any child matches.
    Or(Vec<Expr>),
    /// Negation.
    Not(Box<Expr>),
    /// Leaf.
    Term(Term),
    /// A macro reference and its expansion.
    Macro(MacroRef, Box<Expr>),
}

/// How a macro was written (kept for display and highlighting).
#[derive(Clone, Debug)]
pub struct MacroRef {
    /// Which macro.
    pub id: MacroId,
    /// Name as written (normalised).
    pub name: String,
    /// Term-local modifiers.
    pub mods: ModSet,
    /// Argument after the colon.
    pub value: Value,
    /// Byte range in the input.
    pub raw: Range<usize>,
}

impl PartialEq for MacroRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.mods == other.mods && self.value == other.value
    }
}

impl Expr {
    /// Visits every term.
    pub fn for_each_term<'a>(&'a self, f: &mut impl FnMut(&'a Term)) {
        match self {
            Expr::All => {}
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.for_each_term(f)),
            Expr::Not(e) | Expr::Macro(_, e) => e.for_each_term(f),
            Expr::Term(t) => f(t),
        }
    }
}

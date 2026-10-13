//! Lexer and recursive-descent parser (SPEC ch.2 §1).
//!
//! Two phases:
//! 1. **Tokenise** into terms and the operators `|`, `!`, `<`, `>`. Quoting, character
//!    entities (`&quot:`) and environment variables (`%TEMP%`) are resolved here; every
//!    resulting character remembers whether it was literal (quoted/escaped) so later
//!    stages never treat it as syntax.
//! 2. **Parse** tokens into an [`Expr`]. Whitespace is AND, `|` is OR; by default OR
//!    binds tighter (`a b|c` = `a (b|c)`, Everything's default), switchable with
//!    `and-precedence:`. Each term is split into `[modifier:]* [function: | macro:] value`.
//!
//! Error tolerance follows Everything: unknown `name:` prefixes are literal text, an
//! unclosed `"` runs to the end, an unclosed `<` is closed at the end and a stray `>` is
//! ignored.

use std::ops::Range;

use fnc_core::fold::is_query_space;
use fnc_core::settings::Precedence;

use crate::ast::{CmpOp, Expr, MacroRef, ModSet, Modifier, Term, Value};
use crate::funcs::{
    FuncId, MacroId, Switch, ValueKind, builtin_macro_exts, lookup_entity, lookup_function, lookup_macro,
    lookup_modifier, lookup_switch, normalize_name,
};

/// Maximum macro expansion depth (SPEC ch.2 §6).
pub const MAX_MACRO_DEPTH: usize = 8;

/// Maximum `< >` nesting; deeper `<` are ignored (keeps recursion bounded).
pub const MAX_GROUP_DEPTH: usize = 64;

/// A user-defined macro (from a filter's "macro" field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMacro {
    /// Name (any case; matched normalised).
    pub name: String,
    /// Search it expands to; `#param:` is replaced by the macro argument.
    pub search: String,
}

/// Environment variable lookup used for `%VAR%` expansion.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Parsing context.
pub struct ParseEnv<'a> {
    /// Operator precedence (settings `operator_precedence`).
    pub precedence: Precedence,
    /// User macros.
    pub user_macros: &'a [UserMacro],
    /// Environment variable lookup (`None` disables `%VAR%` expansion).
    pub env_var: Option<EnvLookup<'a>>,
}

impl Default for ParseEnv<'_> {
    fn default() -> Self {
        Self { precedence: Precedence::Or, user_macros: &[], env_var: None }
    }
}

/// Syntax-highlighting class of a span.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SpanKind {
    /// `|`
    Or,
    /// `!`
    Not,
    /// `<` or `>`
    Group,
    /// `case:` etc.
    Modifier,
    /// `ext:` etc.
    Function,
    /// `audio:` etc.
    Macro,
    /// Term value.
    Text,
}

/// A highlighted range of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte range.
    pub range: Range<usize>,
    /// Class.
    pub kind: SpanKind,
}

/// Parser diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagKind {
    /// `"` without a closing quote.
    UnclosedQuote,
    /// `<` without `>`.
    UnclosedGroup,
    /// `>` without `<`.
    StrayClose,
    /// Macro recursion deeper than [`MAX_MACRO_DEPTH`].
    MacroTooDeep(String),
    /// `<` nesting deeper than [`MAX_GROUP_DEPTH`] (the extra `<` was ignored).
    TooDeep,
}

/// A diagnostic with its location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    /// Byte range.
    pub range: Range<usize>,
    /// What went wrong.
    pub kind: DiagKind,
}

/// Parse result.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// Expression tree.
    pub ast: Expr,
    /// Highlighting spans, in input order.
    pub spans: Vec<Span>,
    /// Problems found (the AST is still usable).
    pub diagnostics: Vec<Diag>,
    /// Effective precedence (after `and-precedence:` switches).
    pub precedence: Precedence,
    /// `and-filter:` (`Some(true)`) / `or-filter:` (`Some(false)`) if present.
    pub and_filter: Option<bool>,
}

/// Parses `input`.
pub fn parse(input: &str, env: &ParseEnv<'_>) -> Parsed {
    parse_depth(input, env, 0)
}

fn parse_depth(input: &str, env: &ParseEnv<'_>, depth: usize) -> Parsed {
    let mut lx =
        Lexer { src: input, chars: input.char_indices().collect(), i: 0, env, diags: Vec::new(), spans: Vec::new() };
    let tokens = lx.tokenize();
    let mut p = Parser {
        tokens,
        j: 0,
        env,
        depth,
        global: ModSet::EMPTY,
        precedence: env.precedence,
        and_filter: None,
        spans: lx.spans,
        diags: lx.diags,
        input_len: input.len(),
    };
    p.scan_switches();
    let ast = p.parse_top();
    let mut spans = p.spans;
    spans.sort_by_key(|s| (s.range.start, s.range.end));
    Parsed { ast, spans, diagnostics: p.diags, precedence: p.precedence, and_filter: p.and_filter }
}

// ---------------------------------------------------------------- lexer

/// One character of a term.
#[derive(Copy, Clone, Debug)]
struct Ch {
    c: char,
    /// Quoted, entity or environment text: never syntax.
    lit: bool,
    /// Byte offset of the source text this char came from.
    pos: usize,
}

#[derive(Clone, Debug)]
enum Tok {
    Term { chars: Vec<Ch>, range: Range<usize> },
    Or,
    Not,
    Open(usize),
    Close(usize),
}

struct Lexer<'s, 'e> {
    src: &'s str,
    chars: Vec<(usize, char)>,
    i: usize,
    env: &'e ParseEnv<'e>,
    diags: Vec<Diag>,
    spans: Vec<Span>,
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '#'
}

impl Lexer<'_, '_> {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).map(|&(_, c)| c)
    }

    fn pos(&self) -> usize {
        self.chars.get(self.i).map_or(self.src.len(), |&(p, _)| p)
    }

    fn tokenize(&mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        while let Some(c) = self.peek(0) {
            let pos = self.pos();
            if is_query_space(c) {
                self.i += 1;
                continue;
            }
            let single = match c {
                '|' => Some((Tok::Or, SpanKind::Or)),
                '!' => Some((Tok::Not, SpanKind::Not)),
                '<' => Some((Tok::Open(pos), SpanKind::Group)),
                '>' => Some((Tok::Close(pos), SpanKind::Group)),
                _ => None,
            };
            if let Some((tok, kind)) = single {
                self.spans.push(Span { range: pos..pos + 1, kind });
                out.push(tok);
                self.i += 1;
                continue;
            }
            out.push(self.read_term());
        }
        out
    }

    /// Tries `&name:` at the cursor; on success pushes the char and advances.
    fn try_entity(&mut self, out: &mut Vec<Ch>) -> bool {
        let pos = self.pos();
        let mut k = 1;
        let mut name = String::new();
        while let Some(c) = self.peek(k) {
            if c == ':' {
                break;
            }
            if !(c.is_ascii_alphanumeric() || c == '#') || name.len() > 10 {
                return false;
            }
            name.push(c);
            k += 1;
        }
        if self.peek(k) != Some(':') {
            return false;
        }
        match lookup_entity(&name.to_ascii_lowercase()) {
            Some(c) => {
                out.push(Ch { c, lit: true, pos });
                self.i += k + 1;
                true
            }
            None => false,
        }
    }

    /// Tries `%NAME%` at the cursor.
    // SPEC-GAP: expanded text is treated as literal (not re-parsed for operators).
    fn try_env(&mut self, out: &mut Vec<Ch>) -> bool {
        let Some(lookup) = self.env.env_var else { return false };
        let pos = self.pos();
        let mut k = 1;
        let mut name = String::new();
        while let Some(c) = self.peek(k) {
            if c == '%' {
                break;
            }
            if !(c.is_alphanumeric() || c == '_' || c == '(' || c == ')') {
                return false;
            }
            name.push(c);
            k += 1;
        }
        if self.peek(k) != Some('%') || name.is_empty() {
            return false;
        }
        match lookup(&name) {
            Some(v) => {
                out.extend(v.chars().map(|c| Ch { c, lit: true, pos }));
                self.i += k + 1;
                true
            }
            None => false,
        }
    }

    /// `true` if `<`/`>` at this point belongs to a function value's comparison
    /// operator (`size:>1mb`, `dm:<=2026`, `len:>20`): the term so far must end in a
    /// numeric function or a value-comparing modifier, a colon and only operator
    /// characters. Elsewhere (`<folder:>`) the bracket is a group delimiter.
    fn operator_position(out: &[Ch]) -> bool {
        let Some(colon) = out.iter().rposition(|ch| ch.c == ':' && !ch.lit) else { return false };
        if !out[colon + 1..].iter().all(|ch| !ch.lit && matches!(ch.c, '<' | '>' | '=' | '!')) {
            return false;
        }
        let start =
            out[..colon].iter().rposition(|ch| ch.lit || !(is_ident_char(ch.c) || ch.c == '*')).map_or(0, |p| p + 1);
        let ident: String = out[start..colon].iter().map(|ch| ch.c).filter(|&c| c != '*').collect();
        let mut norm = String::new();
        normalize_name(&ident, &mut norm);
        let numeric_fn = lookup_function(&norm).is_some_and(|f| {
            matches!(f.kind(), ValueKind::Number | ValueKind::Size | ValueKind::Date | ValueKind::Duration)
                // `sem:>0.6 text` (similarity threshold, SPEC ch.5 §3.2).
                || crate::semantic::func_kinds(f).is_some()
        });
        let known = lookup_function(&norm).is_some() || lookup_modifier(&norm).is_some();
        // A value-comparing modifier anywhere in the prefix chain (`len:>20`,
        // `len:album:>20`, `count:tags:>2`).
        let prefix: String = out[..colon].iter().filter(|ch| !ch.lit).map(|ch| ch.c).collect();
        let mut seg_norm = String::new();
        let cmp_mod = prefix.split(':').any(|seg| {
            normalize_name(seg.trim_start_matches('?'), &mut seg_norm);
            lookup_modifier(&seg_norm)
                .is_some_and(|(m, _)| matches!(m, Modifier::Len | Modifier::Count | Modifier::Atoi | Modifier::Atof))
        });
        !norm.is_empty() && known && (numeric_fn || cmp_mod)
    }

    fn read_term(&mut self) -> Tok {
        let start = self.pos();
        let mut out: Vec<Ch> = Vec::new();
        let mut in_quote = false;
        let mut quote_len = 0usize;
        let mut quote_start = start;
        let mut literal_rest = false;
        while let Some(c) = self.peek(0) {
            let pos = self.pos();
            if in_quote {
                match c {
                    '"' => {
                        if self.peek(1) == Some('"') && quote_len > 0 {
                            out.push(Ch { c: '"', lit: true, pos });
                            self.i += 2;
                            quote_len += 1;
                        } else {
                            in_quote = false;
                            self.i += 1;
                        }
                        continue;
                    }
                    '&' if self.try_entity(&mut out) => {
                        quote_len += 1;
                        continue;
                    }
                    '%' if self.try_env(&mut out) => {
                        quote_len += 1;
                        continue;
                    }
                    _ => {
                        out.push(Ch { c, lit: true, pos });
                        quote_len += 1;
                        self.i += 1;
                        continue;
                    }
                }
            }
            if is_query_space(c) {
                break;
            }
            match c {
                '"' => {
                    in_quote = true;
                    quote_len = 0;
                    quote_start = pos;
                    self.i += 1;
                    continue;
                }
                '|' if !literal_rest => break,
                '<' | '>' if !literal_rest && !Self::operator_position(&out) => break,
                '&' if self.try_entity(&mut out) => continue,
                '%' if self.try_env(&mut out) => continue,
                '*' if self.peek(1) == Some(':') && !literal_rest => {
                    let ident: String = out.iter().map(|ch| ch.c).collect();
                    let mut norm = String::new();
                    normalize_name(ident.trim_start_matches(['?', ':']), &mut norm);
                    if out.iter().all(|ch| !ch.lit && (is_ident_char(ch.c) || ch.c == '?' || ch.c == ':'))
                        && lookup_function(&norm).is_some()
                    {
                        literal_rest = true;
                    }
                }
                _ => {}
            }
            out.push(Ch { c, lit: false, pos });
            self.i += 1;
        }
        if in_quote {
            self.diags.push(Diag { range: quote_start..self.pos(), kind: DiagKind::UnclosedQuote });
        }
        Tok::Term { chars: out, range: start..self.pos() }
    }
}

// ---------------------------------------------------------------- parser

struct Parser<'e> {
    tokens: Vec<Tok>,
    j: usize,
    env: &'e ParseEnv<'e>,
    depth: usize,
    global: ModSet,
    precedence: Precedence,
    and_filter: Option<bool>,
    spans: Vec<Span>,
    diags: Vec<Diag>,
    input_len: usize,
}

/// Result of splitting a term's prefixes.
struct Prefix {
    mods: ModSet,
    global: ModSet,
    head: Head,
    /// Index of the first value char.
    value_start: usize,
    literal_value: bool,
}

enum Head {
    Plain,
    Func(FuncId),
    Macro(MacroId, String),
}

fn and_of(mut items: Vec<Expr>) -> Expr {
    items.retain(|e| *e != Expr::All);
    let mut flat = Vec::with_capacity(items.len());
    for e in items {
        match e {
            Expr::And(v) => flat.extend(v),
            e => flat.push(e),
        }
    }
    match flat.len() {
        0 => Expr::All,
        1 => flat.pop().unwrap_or(Expr::All),
        _ => Expr::And(flat),
    }
}

fn or_of(items: Vec<Expr>) -> Expr {
    if items.contains(&Expr::All) {
        return Expr::All;
    }
    let mut flat = Vec::with_capacity(items.len());
    for e in items {
        match e {
            Expr::Or(v) => flat.extend(v),
            e => flat.push(e),
        }
    }
    match flat.len() {
        0 => Expr::All,
        1 => flat.pop().unwrap_or(Expr::All),
        _ => Expr::Or(flat),
    }
}

fn text_of(chars: &[Ch]) -> String {
    chars.iter().map(|ch| ch.c).collect()
}

fn char_end(chars: &[Ch], k: usize, fallback: usize) -> usize {
    chars.get(k).map_or(fallback, |ch| ch.pos)
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.j)
    }

    /// Applies and removes `and-precedence:`-style switch terms (query-wide).
    fn scan_switches(&mut self) {
        let mut keep = Vec::with_capacity(self.tokens.len());
        let mut norm = String::new();
        for tok in std::mem::take(&mut self.tokens) {
            if let Tok::Term { chars, range } = &tok {
                let text = text_of(chars);
                let body = text.strip_prefix('?').unwrap_or(&text);
                if chars.iter().all(|c| !c.lit) && body.ends_with(':') && body.len() > 1 {
                    normalize_name(&body[..body.len() - 1], &mut norm);
                    if let Some(sw) = lookup_switch(&norm) {
                        match sw {
                            Switch::AndPrecedence => self.precedence = Precedence::And,
                            Switch::OrPrecedence => self.precedence = Precedence::Or,
                            Switch::AndFilter => self.and_filter = Some(true),
                            Switch::OrFilter => self.and_filter = Some(false),
                        }
                        self.spans.push(Span { range: range.clone(), kind: SpanKind::Modifier });
                        continue;
                    }
                }
            }
            keep.push(tok);
        }
        self.tokens = keep;
    }

    fn parse_top(&mut self) -> Expr {
        let e = self.parse_seq(0);
        // Anything left is a stray `>` sequence at top level; parse_seq skips them.
        e
    }

    fn at_seq_end(&self, depth: usize) -> bool {
        match self.peek() {
            None => true,
            Some(Tok::Close(_)) => depth > 0,
            _ => false,
        }
    }

    fn skip_stray_closes(&mut self, depth: usize) {
        if depth > 0 {
            return;
        }
        while let Some(Tok::Close(pos)) = self.peek() {
            let pos = *pos;
            self.diags.push(Diag { range: pos..pos + 1, kind: DiagKind::StrayClose });
            self.j += 1;
        }
    }

    fn parse_seq(&mut self, depth: usize) -> Expr {
        match self.precedence {
            Precedence::Or => self.parse_and_of_ors(depth),
            Precedence::And => self.parse_or_of_ands(depth),
        }
    }

    /// OR binds tighter: `seq := or_group (WS or_group)*`.
    fn parse_and_of_ors(&mut self, depth: usize) -> Expr {
        let mut items = Vec::new();
        loop {
            self.skip_stray_closes(depth);
            if self.at_seq_end(depth) {
                break;
            }
            if let Some(Tok::Or) = self.peek() {
                self.j += 1; // dangling `|`
                continue;
            }
            let mut alts = Vec::new();
            if let Some(e) = self.parse_unary(depth) {
                alts.push(e);
            }
            while let Some(Tok::Or) = self.peek() {
                while let Some(Tok::Or) = self.peek() {
                    self.j += 1;
                }
                self.skip_stray_closes(depth);
                if self.at_seq_end(depth) {
                    break;
                }
                if let Some(e) = self.parse_unary(depth) {
                    alts.push(e);
                }
            }
            if !alts.is_empty() {
                items.push(or_of(alts));
            }
        }
        and_of(items)
    }

    /// AND binds tighter: `seq := and_group ('|' and_group)*`.
    fn parse_or_of_ands(&mut self, depth: usize) -> Expr {
        let mut alts = Vec::new();
        loop {
            let mut seq = Vec::new();
            loop {
                self.skip_stray_closes(depth);
                if self.at_seq_end(depth) || matches!(self.peek(), Some(Tok::Or)) {
                    break;
                }
                if let Some(e) = self.parse_unary(depth) {
                    seq.push(e);
                }
            }
            if !seq.is_empty() {
                alts.push(and_of(seq));
            }
            if let Some(Tok::Or) = self.peek() {
                self.j += 1;
                continue;
            }
            break;
        }
        or_of(alts)
    }

    fn parse_unary(&mut self, depth: usize) -> Option<Expr> {
        let mut negations = 0;
        while let Some(Tok::Not) = self.peek() {
            negations += 1;
            self.j += 1;
        }
        if negations > 0 && (self.at_seq_end(depth) || matches!(self.peek(), Some(Tok::Or))) {
            return None;
        }
        let e = self.parse_primary(depth)?;
        Some(match (negations % 2 == 1, e) {
            (true, Expr::Not(inner)) => *inner,
            (true, e) => Expr::Not(Box::new(e)),
            (false, e) => e,
        })
    }

    fn parse_primary(&mut self, depth: usize) -> Option<Expr> {
        match self.peek()?.clone() {
            Tok::Open(pos) if depth >= MAX_GROUP_DEPTH => {
                // Pathologically deep nesting: ignore further `<` instead of recursing
                // (bounded stack usage for any input).
                self.j += 1;
                self.diags.push(Diag { range: pos..pos + 1, kind: DiagKind::TooDeep });
                None
            }
            Tok::Open(pos) => {
                self.j += 1;
                let e = self.parse_seq(depth + 1);
                match self.peek() {
                    Some(Tok::Close(_)) => self.j += 1,
                    _ => self.diags.push(Diag { range: pos..self.input_len, kind: DiagKind::UnclosedGroup }),
                }
                Some(e)
            }
            Tok::Term { chars, range } => {
                self.j += 1;
                self.build_term(&chars, range)
            }
            Tok::Close(_) | Tok::Or | Tok::Not => {
                self.j += 1;
                None
            }
        }
    }

    fn split_prefix(&mut self, chars: &[Ch], range_end: usize) -> Prefix {
        let n = chars.len();
        let unq = |k: usize, c: char| chars.get(k).is_some_and(|ch| !ch.lit && ch.c == c);
        let mut p = 0;
        let mut mods = ModSet::EMPTY;
        let mut global = ModSet::EMPTY;
        let mut builtin_only = false;
        let mut norm = String::new();
        if unq(0, ':') && unq(1, ':') {
            builtin_only = true;
            p = 2;
        }
        loop {
            let save = p;
            let is_global = unq(p, '?');
            if is_global {
                p += 1;
            }
            let s = p;
            while p < n && !chars[p].lit && is_ident_char(chars[p].c) {
                p += 1;
            }
            let ident_end = p;
            let star = unq(p, '*') && unq(p + 1, ':');
            if star {
                p += 1;
            }
            if ident_end == s || !unq(p, ':') {
                p = save;
                break;
            }
            // `C:\path`: a single letter followed by `:\` is a drive, not a function.
            if ident_end - s == 1
                && chars.get(p + 1).is_some_and(|ch| ch.c == '\\' || ch.c == '/')
                && chars[s].c.is_ascii_alphabetic()
            {
                p = save;
                break;
            }
            let ident = text_of(&chars[s..ident_end]);
            normalize_name(&ident, &mut norm);
            let span = chars[save].pos..char_end(chars, p + 1, range_end);
            p += 1; // colon
            if !star && let Some((m, on)) = lookup_modifier(&norm) {
                if is_global {
                    global.set(m, on);
                } else {
                    mods.set(m, on);
                }
                self.spans.push(Span { range: span, kind: SpanKind::Modifier });
                continue;
            }
            if !builtin_only
                && let Some(k) = self.env.user_macros.iter().position(|m| {
                    let mut mn = String::new();
                    normalize_name(&m.name, &mut mn);
                    mn == norm
                })
            {
                self.spans.push(Span { range: span, kind: SpanKind::Macro });
                return Prefix {
                    mods,
                    global,
                    head: Head::Macro(MacroId::User(k as u16), norm),
                    value_start: p,
                    literal_value: star,
                };
            }
            if let Some(m) = lookup_macro(&norm) {
                self.spans.push(Span { range: span, kind: SpanKind::Macro });
                return Prefix { mods, global, head: Head::Macro(m, norm), value_start: p, literal_value: star };
            }
            if let Some(f) = lookup_function(&norm) {
                self.spans.push(Span { range: span, kind: SpanKind::Function });
                return Prefix { mods, global, head: Head::Func(f), value_start: p, literal_value: star };
            }
            p = save;
            break;
        }
        Prefix { mods, global, head: Head::Plain, value_start: p, literal_value: false }
    }

    fn build_term(&mut self, chars: &[Ch], range: Range<usize>) -> Option<Expr> {
        let prefix = self.split_prefix(chars, range.end);
        self.global = self.global.overlay(prefix.global);
        let mods = self.global.overlay(prefix.mods);
        let value_chars = &chars[prefix.value_start..];
        if !value_chars.is_empty() {
            self.spans.push(Span { range: value_chars[0].pos..range.end, kind: SpanKind::Text });
        }
        match prefix.head {
            Head::Plain => {
                if value_chars.is_empty() && prefix.value_start > 0 && chars.iter().all(|c| !c.lit) {
                    // Only modifiers (`case:` / `?case:`): nothing to match.
                    return None;
                }
                Some(Expr::Term(Term { mods, func: None, value: Value::Text(text_of(value_chars)), raw: range }))
            }
            Head::Func(func) => {
                let value = if prefix.literal_value {
                    Value::Text(text_of(value_chars))
                } else {
                    parse_value(value_chars, func.kind())
                };
                Some(Expr::Term(Term { mods, func: Some(func), value, raw: range }))
            }
            Head::Macro(id, name) => {
                let value = if value_chars.is_empty() { Value::Empty } else { Value::Text(text_of(value_chars)) };
                let body = self.expand_macro(id, mods, &value, &range);
                Some(Expr::Macro(MacroRef { id, name, mods, value, raw: range }, Box::new(body)))
            }
        }
    }

    fn expand_macro(&mut self, id: MacroId, mods: ModSet, value: &Value, range: &Range<usize>) -> Expr {
        let arg = value.as_text().unwrap_or("");
        let arg_term = || {
            (!arg.is_empty())
                .then(|| Expr::Term(Term { mods, func: None, value: Value::Text(arg.to_string()), raw: range.clone() }))
        };
        match id {
            MacroId::User(k) => {
                let Some(m) = self.env.user_macros.get(k as usize) else { return Expr::All };
                if self.depth + 1 > MAX_MACRO_DEPTH {
                    self.diags.push(Diag { range: range.clone(), kind: DiagKind::MacroTooDeep(m.name.clone()) });
                    return Expr::All;
                }
                let has_param = m.search.to_ascii_lowercase().contains("#param:");
                let search = if has_param { replace_param(&m.search, arg) } else { m.search.clone() };
                let inner = parse_depth(&search, self.env, self.depth + 1);
                self.diags.extend(
                    inner
                        .diagnostics
                        .into_iter()
                        .filter(|d| matches!(d.kind, DiagKind::MacroTooDeep(_)))
                        .map(|d| Diag { range: range.clone(), kind: d.kind }),
                );
                if has_param { inner.ast } else { and_of(std::iter::once(inner.ast).chain(arg_term()).collect()) }
            }
            builtin => {
                let exts = builtin_macro_exts(builtin).split(';').map(|e| Value::Text(e.to_string())).collect();
                let ext = Expr::Term(Term {
                    mods: ModSet::EMPTY,
                    func: Some(FuncId::Ext),
                    value: Value::List(exts),
                    raw: range.clone(),
                });
                and_of(std::iter::once(ext).chain(arg_term()).collect())
            }
        }
    }
}

/// Replaces every `#param:` (case-insensitive) in `search` with `arg`.
fn replace_param(search: &str, arg: &str) -> String {
    let lower = search.to_ascii_lowercase();
    let mut out = String::with_capacity(search.len() + arg.len());
    let mut last = 0;
    for (i, _) in lower.match_indices("#param:") {
        out.push_str(&search[last..i]);
        out.push_str(arg);
        last = i + "#param:".len();
    }
    out.push_str(&search[last..]);
    out
}

/// Parses a function value according to its kind: `;` lists, comparison operators and
/// `..` ranges (numeric kinds only).
fn parse_value(chars: &[Ch], kind: ValueKind) -> Value {
    if chars.is_empty() {
        return Value::Empty;
    }
    let numeric = matches!(kind, ValueKind::Number | ValueKind::Size | ValueKind::Date | ValueKind::Duration);
    let parts: Vec<&[Ch]> = chars.split(|ch| ch.c == ';' && !ch.lit).collect();
    if parts.len() > 1 {
        return Value::List(parts.into_iter().filter(|p| !p.is_empty()).map(|p| parse_item(p, numeric)).collect());
    }
    parse_item(chars, numeric)
}

fn parse_item(chars: &[Ch], numeric: bool) -> Value {
    if !numeric {
        return Value::Text(text_of(chars));
    }
    let unq = |k: usize, c: char| chars.get(k).is_some_and(|ch| !ch.lit && ch.c == c);
    let op = if unq(0, '>') && unq(1, '=') {
        Some((CmpOp::Ge, 2))
    } else if unq(0, '<') && unq(1, '=') {
        Some((CmpOp::Le, 2))
    } else if unq(0, '!') && unq(1, '=') {
        Some((CmpOp::Ne, 2))
    } else if unq(0, '>') {
        Some((CmpOp::Gt, 1))
    } else if unq(0, '<') {
        Some((CmpOp::Lt, 1))
    } else if unq(0, '=') {
        Some((CmpOp::Eq, 1))
    } else if unq(0, '!') {
        Some((CmpOp::Ne, 1))
    } else {
        None
    };
    if let Some((op, skip)) = op {
        return Value::Cmp(op, Box::new(Value::Text(text_of(&chars[skip..]))));
    }
    if let Some(k) = (0..chars.len().saturating_sub(1)).find(|&k| unq(k, '.') && unq(k + 1, '.')) {
        return Value::Range(
            Box::new(Value::Text(text_of(&chars[..k]))),
            Box::new(Value::Text(text_of(&chars[k + 2..]))),
        );
    }
    Value::Text(text_of(chars))
}

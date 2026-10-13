//! Formulas (`eval:` and custom columns; SPEC ch.2 §5.2, ch.3 §4): Excel-style
//! expressions over the candidate's properties.
//!
//! * Variables: `$name:` `$stem:` `$ext:` `$path:` `$parent:` `$fullpath:` `$size:`
//!   `$dm:` `$dc:` `$da:` `$dr:` `$attrib:` `$depth:` `$runcount:` and every property
//!   name (`$width:`, `$artist:`), resolved by the caller through [`Vars`].
//! * Literals: numbers (with size units, `1.5mb`), `"text"` (`""` escapes a quote),
//!   `TRUE`/`FALSE`.
//! * Operators, loosest first: `= <> != < <= > >=`, `&` (concatenate), `+ -`, `* /`,
//!   `^`, unary `-`.
//! * Functions: see [`call`] (Excel names, case-insensitive).
//!
//! Errors evaluate to [`Val::Error`] (`#VALUE!`, `#DIV/0!`, `#NAME?`), which is false
//! for `eval:` and shown as text in a column.

use std::fmt;

use chrono::{Datelike, NaiveDate, Timelike};
use fnc_core::FileTime;

/// A value.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// Number.
    Num(f64),
    /// Text.
    Text(String),
    /// Boolean.
    Bool(bool),
    /// Date (FILETIME ticks).
    Date(i64),
    /// Unknown / empty.
    Empty,
    /// Error code (`#VALUE!`).
    Error(&'static str),
}

impl Val {
    /// Numeric value (booleans 1/0, dates as ticks, numeric text, empty 0).
    pub fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            Val::Bool(b) => Some(f64::from(u8::from(*b))),
            Val::Date(t) => Some(*t as f64),
            Val::Empty => Some(0.0),
            Val::Text(t) => t.trim().parse().ok(),
            Val::Error(_) => None,
        }
    }

    /// Truth (`eval:` matches when true).
    pub fn truthy(&self) -> bool {
        match self {
            Val::Text(t) => !t.is_empty() && !t.eq_ignore_ascii_case("false") && t != "0",
            Val::Error(_) | Val::Empty => false,
            v => v.num().is_some_and(|n| n != 0.0),
        }
    }

    fn text(&self) -> String {
        match self {
            Val::Text(t) => t.clone(),
            v => v.to_string(),
        }
    }

    fn is_error(&self) -> bool {
        matches!(self, Val::Error(_))
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Val::Num(n) => write!(f, "{}", (n * 1e9).round() / 1e9),
            Val::Text(t) => f.write_str(t),
            Val::Bool(b) => f.write_str(if *b { "TRUE" } else { "FALSE" }),
            Val::Date(t) => match FileTime(*t).to_local() {
                Some(d) => write!(f, "{}", d.format("%Y-%m-%d %H:%M:%S")),
                None => Ok(()),
            },
            Val::Empty => Ok(()),
            Val::Error(e) => f.write_str(e),
        }
    }
}

/// Variable lookup for one candidate (names are lowercase, without `$`, `-` and `:`).
pub trait Vars {
    /// Value of `name`; unknown names are [`Val::Error`]`("#NAME?")`.
    fn var(&self, name: &str) -> Val;
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Lit(Val),
    Var(String),
    Neg(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Cat,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

/// An `eval:` term: the formula, compared and hashed by its source.
#[derive(Clone, Debug)]
pub struct EvalPred(pub std::sync::Arc<Formula>);

impl PartialEq for EvalPred {
    fn eq(&self, other: &Self) -> bool {
        self.0.src == other.0.src
    }
}

impl Eq for EvalPred {}

impl std::hash::Hash for EvalPred {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.0.src.hash(h);
    }
}

/// A parsed formula.
#[derive(Clone, Debug, PartialEq)]
pub struct Formula {
    src: String,
    root: Node,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Var(String),
    Ident(String),
    Op(Op),
    LParen,
    RParen,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' | ';' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('"') if chars.get(i + 1) == Some(&'"') => {
                            s.push('"');
                            i += 2;
                        }
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1;
                        }
                        None => return Err("unclosed quote".into()),
                    }
                }
                out.push(Tok::Str(s));
            }
            '$' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                    j += 1;
                }
                if j == start {
                    return Err("variable name expected after $".into());
                }
                let name: String =
                    chars[start..j].iter().filter(|c| **c != '-' && **c != '_').collect::<String>().to_lowercase();
                // The trailing colon of `$name:` is optional.
                i = if chars.get(j) == Some(&':') { j + 1 } else { j };
                out.push(Tok::Var(name));
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let num: String = chars[start..i].iter().collect();
                let mut v: f64 = num.parse().map_err(|_| format!("bad number {num}"))?;
                // Size units (`1.5mb`) and exponents (`1e6`).
                let ustart = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let unit: String = chars[ustart..i].iter().collect::<String>().to_ascii_lowercase();
                let mult = match unit.as_str() {
                    "" => 1.0,
                    "b" => 1.0,
                    "k" | "kb" => 1024.0,
                    "m" | "mb" => 1024.0 * 1024.0,
                    "g" | "gb" => 1024.0f64.powi(3),
                    "t" | "tb" => 1024.0f64.powi(4),
                    "e" if chars.get(i).is_some_and(|c| c.is_ascii_digit() || *c == '-' || *c == '+') => {
                        let estart = i;
                        i += 1;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                        let e: i32 = chars[estart..i].iter().collect::<String>().parse().map_err(|_| "bad exponent")?;
                        10f64.powi(e)
                    }
                    u => return Err(format!("unknown unit {u}")),
                };
                v *= mult;
                out.push(Tok::Num(v));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect::<String>().to_ascii_uppercase()));
            }
            _ => {
                let two: String = chars[i..chars.len().min(i + 2)].iter().collect();
                let (op, n) = match two.as_str() {
                    "<>" | "!=" => (Op::Ne, 2),
                    "<=" => (Op::Le, 2),
                    ">=" => (Op::Ge, 2),
                    "==" => (Op::Eq, 2),
                    _ => match c {
                        '=' => (Op::Eq, 1),
                        '<' => (Op::Lt, 1),
                        '>' => (Op::Gt, 1),
                        '&' => (Op::Cat, 1),
                        '+' => (Op::Add, 1),
                        '-' => (Op::Sub, 1),
                        '*' => (Op::Mul, 1),
                        '/' => (Op::Div, 1),
                        '^' => (Op::Pow, 1),
                        other => return Err(format!("unexpected {other}")),
                    },
                };
                out.push(Tok::Op(op));
                i += n;
            }
        }
    }
    Ok(out)
}

struct P {
    toks: Vec<Tok>,
    i: usize,
}

fn prec(op: Op) -> u8 {
    match op {
        Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => 1,
        Op::Cat => 2,
        Op::Add | Op::Sub => 3,
        Op::Mul | Op::Div => 4,
        Op::Pow => 5,
    }
}

impl P {
    fn expr(&mut self, min: u8) -> Result<Node, String> {
        let mut lhs = self.unary()?;
        while let Some(Tok::Op(op)) = self.toks.get(self.i).cloned() {
            let p = prec(op);
            if p < min {
                break;
            }
            self.i += 1;
            // `^` is right-associative.
            let rhs = self.expr(if op == Op::Pow { p } else { p + 1 })?;
            lhs = Node::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Node, String> {
        match self.toks.get(self.i) {
            Some(Tok::Op(Op::Sub)) => {
                self.i += 1;
                Ok(Node::Neg(Box::new(self.expr(5)?)))
            }
            Some(Tok::Op(Op::Add)) => {
                self.i += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<Node, String> {
        let t = self.toks.get(self.i).cloned().ok_or("unexpected end")?;
        self.i += 1;
        Ok(match t {
            Tok::Num(n) => Node::Lit(Val::Num(n)),
            Tok::Str(s) => Node::Lit(Val::Text(s)),
            Tok::Var(v) => Node::Var(v),
            Tok::LParen => {
                let e = self.expr(0)?;
                if self.toks.get(self.i) != Some(&Tok::RParen) {
                    return Err("missing )".into());
                }
                self.i += 1;
                e
            }
            Tok::Ident(name) => {
                if self.toks.get(self.i) == Some(&Tok::LParen) {
                    self.i += 1;
                    let mut args = Vec::new();
                    if self.toks.get(self.i) == Some(&Tok::RParen) {
                        self.i += 1;
                    } else {
                        loop {
                            args.push(self.expr(0)?);
                            match self.toks.get(self.i) {
                                Some(Tok::Comma) => self.i += 1,
                                Some(Tok::RParen) => {
                                    self.i += 1;
                                    break;
                                }
                                _ => return Err(format!("missing ) after {name}(")),
                            }
                        }
                    }
                    Node::Call(name, args)
                } else {
                    match name.as_str() {
                        "TRUE" => Node::Lit(Val::Bool(true)),
                        "FALSE" => Node::Lit(Val::Bool(false)),
                        _ => return Err(format!("unknown name {name}")),
                    }
                }
            }
            other => return Err(format!("unexpected {other:?}")),
        })
    }
}

impl Formula {
    /// Parses `src` (a leading `=` is allowed, as in Excel).
    pub fn parse(src: &str) -> Result<Self, String> {
        let body = src.trim().strip_prefix('=').unwrap_or(src.trim());
        let toks = lex(body)?;
        if toks.is_empty() {
            return Err("empty formula".into());
        }
        let mut p = P { toks, i: 0 };
        let root = p.expr(0)?;
        if p.i != p.toks.len() {
            return Err(format!("unexpected {:?}", p.toks[p.i]));
        }
        Ok(Self { src: src.to_string(), root })
    }

    /// The source text.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// Variables used (deduplicated).
    pub fn vars(&self) -> Vec<String> {
        fn walk(n: &Node, out: &mut Vec<String>) {
            match n {
                Node::Var(v) if !out.contains(v) => out.push(v.clone()),
                Node::Neg(a) => walk(a, out),
                Node::Bin(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Node::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
                _ => {}
            }
        }
        let mut v = Vec::new();
        walk(&self.root, &mut v);
        v
    }

    /// Evaluates for one candidate.
    pub fn eval(&self, vars: &dyn Vars) -> Val {
        eval(&self.root, vars)
    }
}

fn cmp(a: &Val, b: &Val) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Val::Text(x), Val::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        // Text against a number compares as numbers when the text is numeric.
        (Val::Text(_), _) | (_, Val::Text(_)) => match (a.num(), b.num()) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => Some(a.text().to_lowercase().cmp(&b.text().to_lowercase())),
        },
        _ => a.num()?.partial_cmp(&b.num()?),
    }
}

fn eval(n: &Node, vars: &dyn Vars) -> Val {
    match n {
        Node::Lit(v) => v.clone(),
        Node::Var(name) => vars.var(name),
        Node::Neg(a) => match eval(a, vars) {
            e @ Val::Error(_) => e,
            v => v.num().map_or(Val::Error("#VALUE!"), |x| Val::Num(-x)),
        },
        Node::Bin(op, a, b) => {
            let (x, y) = (eval(a, vars), eval(b, vars));
            if let Val::Error(e) = x {
                return Val::Error(e);
            }
            if let Val::Error(e) = y {
                return Val::Error(e);
            }
            use std::cmp::Ordering as O;
            match op {
                Op::Cat => Val::Text(x.text() + &y.text()),
                Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let Some(o) = cmp(&x, &y) else { return Val::Error("#VALUE!") };
                    Val::Bool(match op {
                        Op::Eq => o == O::Equal,
                        Op::Ne => o != O::Equal,
                        Op::Lt => o == O::Less,
                        Op::Le => o != O::Greater,
                        Op::Gt => o == O::Greater,
                        _ => o != O::Less,
                    })
                }
                _ => {
                    let (Some(p), Some(q)) = (x.num(), y.num()) else { return Val::Error("#VALUE!") };
                    match op {
                        Op::Add => Val::Num(p + q),
                        Op::Sub => Val::Num(p - q),
                        Op::Mul => Val::Num(p * q),
                        Op::Div if q == 0.0 => Val::Error("#DIV/0!"),
                        Op::Div => Val::Num(p / q),
                        _ => Val::Num(p.powf(q)),
                    }
                }
            }
        }
        Node::Call(name, args) => call(name, args, vars),
    }
}

fn date_of(v: &Val) -> Option<chrono::NaiveDateTime> {
    match v {
        Val::Date(t) => FileTime(*t).to_local(),
        v => v.num().and_then(|n| FileTime(n as i64).to_local()),
    }
}

/// Built-in functions.
fn call(name: &str, args: &[Node], vars: &dyn Vars) -> Val {
    // Lazily evaluated: IF, IFS, IFERROR, SWITCH, AND, OR.
    let ev = |i: usize| args.get(i).map_or(Val::Empty, |a| eval(a, vars));
    let num = |i: usize| ev(i).num();
    let text = |i: usize| match ev(i) {
        Val::Error(e) => Err(e),
        v => Ok(v.text()),
    };
    let n = args.len();
    let need = |k: usize| if n < k { Some(Val::Error("#VALUE!")) } else { None };
    let numf = |f: fn(f64) -> f64| num(0).map_or(Val::Error("#VALUE!"), |x| Val::Num(f(x)));
    let all = || args.iter().map(|a| eval(a, vars)).collect::<Vec<_>>();
    match name {
        "IF" => {
            if let Some(e) = need(2) {
                return e;
            }
            match ev(0) {
                Val::Error(e) => Val::Error(e),
                c if c.truthy() => ev(1),
                _ => {
                    if n > 2 {
                        ev(2)
                    } else {
                        Val::Bool(false)
                    }
                }
            }
        }
        "IFS" => {
            for k in (0..n.saturating_sub(1)).step_by(2) {
                if ev(k).truthy() {
                    return ev(k + 1);
                }
            }
            Val::Error("#N/A")
        }
        "IFERROR" => match ev(0) {
            Val::Error(_) => ev(1),
            v => v,
        },
        "SWITCH" => {
            let v = ev(0);
            let mut k = 1;
            while k + 1 < n {
                if cmp(&v, &ev(k)) == Some(std::cmp::Ordering::Equal) {
                    return ev(k + 1);
                }
                k += 2;
            }
            if k < n { ev(k) } else { Val::Error("#N/A") }
        }
        "AND" => Val::Bool((0..n).all(|i| ev(i).truthy())),
        "OR" => Val::Bool((0..n).any(|i| ev(i).truthy())),
        "XOR" => Val::Bool((0..n).filter(|&i| ev(i).truthy()).count() % 2 == 1),
        "NOT" => Val::Bool(!ev(0).truthy()),
        "TRUE" => Val::Bool(true),
        "FALSE" => Val::Bool(false),
        "ISNUMBER" => Val::Bool(matches!(ev(0), Val::Num(_) | Val::Date(_))),
        "ISTEXT" => Val::Bool(matches!(ev(0), Val::Text(_))),
        "ISBLANK" => Val::Bool(matches!(ev(0), Val::Empty) || matches!(ev(0), Val::Text(ref t) if t.is_empty())),
        "ISERROR" => Val::Bool(ev(0).is_error()),
        "ISEVEN" => num(0).map_or(Val::Error("#VALUE!"), |x| Val::Bool((x.trunc() as i64) % 2 == 0)),
        "ISODD" => num(0).map_or(Val::Error("#VALUE!"), |x| Val::Bool((x.trunc() as i64) % 2 != 0)),
        // Numbers.
        "ABS" => numf(f64::abs),
        "INT" => numf(f64::floor),
        "SQRT" => num(0).filter(|x| *x >= 0.0).map_or(Val::Error("#NUM!"), |x| Val::Num(x.sqrt())),
        "EXP" => numf(f64::exp),
        "LN" => num(0).filter(|x| *x > 0.0).map_or(Val::Error("#NUM!"), |x| Val::Num(x.ln())),
        "LOG10" => num(0).filter(|x| *x > 0.0).map_or(Val::Error("#NUM!"), |x| Val::Num(x.log10())),
        "LOG" => match (num(0), if n > 1 { num(1) } else { Some(10.0) }) {
            (Some(x), Some(b)) if x > 0.0 && b > 0.0 && b != 1.0 => Val::Num(x.log(b)),
            _ => Val::Error("#NUM!"),
        },
        "SIGN" => numf(|x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }),
        "PI" => Val::Num(std::f64::consts::PI),
        "POWER" => match (num(0), num(1)) {
            (Some(x), Some(y)) => Val::Num(x.powf(y)),
            _ => Val::Error("#VALUE!"),
        },
        "MOD" => match (num(0), num(1)) {
            (Some(_), Some(0.0)) => Val::Error("#DIV/0!"),
            (Some(x), Some(y)) => Val::Num(x - y * (x / y).floor()),
            _ => Val::Error("#VALUE!"),
        },
        "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
            let (Some(x), d) = (num(0), num(1).unwrap_or(0.0)) else { return Val::Error("#VALUE!") };
            let m = 10f64.powi(d as i32);
            let v = x * m;
            Val::Num(
                match name {
                    "ROUND" => v.round(),
                    "ROUNDUP" => {
                        if v >= 0.0 {
                            v.ceil()
                        } else {
                            v.floor()
                        }
                    }
                    _ => v.trunc(),
                } / m,
            )
        }
        "FLOOR" | "CEILING" => {
            let (Some(x), s) = (num(0), num(1).unwrap_or(1.0)) else { return Val::Error("#VALUE!") };
            if s == 0.0 {
                return Val::Num(0.0);
            }
            Val::Num(if name == "FLOOR" { (x / s).floor() * s } else { (x / s).ceil() * s })
        }
        "MIN" | "MAX" | "SUM" | "AVERAGE" | "COUNT" => {
            let v: Vec<f64> = all().iter().filter_map(|v| if v.is_error() { None } else { v.num() }).collect();
            match name {
                "COUNT" => Val::Num(v.len() as f64),
                "SUM" => Val::Num(v.iter().sum()),
                "AVERAGE" if v.is_empty() => Val::Error("#DIV/0!"),
                "AVERAGE" => Val::Num(v.iter().sum::<f64>() / v.len() as f64),
                "MIN" => Val::Num(v.iter().copied().fold(f64::INFINITY, f64::min)).clamp_inf(),
                _ => Val::Num(v.iter().copied().fold(f64::NEG_INFINITY, f64::max)).clamp_inf(),
            }
        }
        "VALUE" => ev(0).num().map_or(Val::Error("#VALUE!"), Val::Num),
        // Text.
        "LEN" => text(0).map_or_else(Val::Error, |t| Val::Num(t.chars().count() as f64)),
        "LOWER" => text(0).map_or_else(Val::Error, |t| Val::Text(t.to_lowercase())),
        "UPPER" => text(0).map_or_else(Val::Error, |t| Val::Text(t.to_uppercase())),
        "PROPER" => text(0).map_or_else(Val::Error, |t| {
            let mut out = String::new();
            let mut start = true;
            for c in t.chars() {
                if start {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
                start = !c.is_alphanumeric();
            }
            Val::Text(out)
        }),
        "TRIM" => text(0).map_or_else(Val::Error, |t| Val::Text(t.split_whitespace().collect::<Vec<_>>().join(" "))),
        "LEFT" | "RIGHT" => match text(0) {
            Ok(t) => {
                let k = if n > 1 { num(1).unwrap_or(1.0).max(0.0) as usize } else { 1 };
                let c: Vec<char> = t.chars().collect();
                let k = k.min(c.len());
                Val::Text(if name == "LEFT" { c[..k].iter().collect() } else { c[c.len() - k..].iter().collect() })
            }
            Err(e) => Val::Error(e),
        },
        "MID" => match (text(0), num(1), num(2)) {
            (Ok(t), Some(s), Some(l)) if s >= 1.0 && l >= 0.0 => {
                Val::Text(t.chars().skip(s as usize - 1).take(l as usize).collect())
            }
            _ => Val::Error("#VALUE!"),
        },
        "REPT" => match (text(0), num(1)) {
            (Ok(t), Some(k)) if (0.0..10_000.0).contains(&k) => Val::Text(t.repeat(k as usize)),
            _ => Val::Error("#VALUE!"),
        },
        "CONCAT" | "CONCATENATE" => {
            let mut s = String::new();
            for v in all() {
                if let Val::Error(e) = v {
                    return Val::Error(e);
                }
                s.push_str(&v.text());
            }
            Val::Text(s)
        }
        "EXACT" => match (text(0), text(1)) {
            (Ok(a), Ok(b)) => Val::Bool(a == b),
            _ => Val::Error("#VALUE!"),
        },
        "FIND" | "SEARCH" => match (text(0), text(1)) {
            (Ok(needle), Ok(hay)) => {
                let start = num(2).unwrap_or(1.0).max(1.0) as usize - 1;
                let (h, nd) =
                    if name == "SEARCH" { (hay.to_lowercase(), needle.to_lowercase()) } else { (hay, needle) };
                let hc: String = h.chars().skip(start).collect();
                match hc.find(&nd) {
                    Some(b) => Val::Num((start + hc[..b].chars().count() + 1) as f64),
                    None => Val::Error("#VALUE!"),
                }
            }
            _ => Val::Error("#VALUE!"),
        },
        "SUBSTITUTE" => match (text(0), text(1), text(2)) {
            (Ok(t), Ok(old), Ok(new)) if !old.is_empty() => match num(3) {
                Some(k) if n > 3 && k >= 1.0 => {
                    let mut out = String::new();
                    let mut rest = t.as_str();
                    let mut seen = 0;
                    while let Some(p) = rest.find(&old) {
                        seen += 1;
                        out.push_str(&rest[..p]);
                        out.push_str(if seen == k as usize { &new } else { &old });
                        rest = &rest[p + old.len()..];
                    }
                    out.push_str(rest);
                    Val::Text(out)
                }
                _ => Val::Text(t.replace(&old, &new)),
            },
            (Ok(t), Ok(_), Ok(_)) => Val::Text(t),
            _ => Val::Error("#VALUE!"),
        },
        "REPLACE" => match (text(0), num(1), num(2), text(3)) {
            (Ok(t), Some(s), Some(l), Ok(new)) if s >= 1.0 && l >= 0.0 => {
                let c: Vec<char> = t.chars().collect();
                let s = (s as usize - 1).min(c.len());
                let e = (s + l as usize).min(c.len());
                Val::Text(c[..s].iter().collect::<String>() + &new + &c[e..].iter().collect::<String>())
            }
            _ => Val::Error("#VALUE!"),
        },
        "STARTSWITH" | "ENDSWITH" | "CONTAINS" => match (text(0), text(1)) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (a.to_lowercase(), b.to_lowercase());
                Val::Bool(match name {
                    "STARTSWITH" => a.starts_with(&b),
                    "ENDSWITH" => a.ends_with(&b),
                    _ => a.contains(&b),
                })
            }
            _ => Val::Error("#VALUE!"),
        },
        "TEXT" => ev(0).to_string().into_text(),
        // Dates.
        "NOW" => Val::Date(FileTime::now().0),
        "TODAY" => {
            let now = FileTime::now().to_local().and_then(|d| d.date().and_hms_opt(0, 0, 0));
            now.map_or(Val::Error("#VALUE!"), |d| Val::Date(FileTime::from_local(d).0))
        }
        "DATE" => match (num(0), num(1), num(2)) {
            (Some(y), Some(m), Some(d)) => NaiveDate::from_ymd_opt(y as i32, m as u32, d as u32)
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map_or(Val::Error("#VALUE!"), |d| Val::Date(FileTime::from_local(d).0)),
            _ => Val::Error("#VALUE!"),
        },
        "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" | "WEEKDAY" => match date_of(&ev(0)) {
            Some(d) => Val::Num(f64::from(match name {
                "YEAR" => d.year() as u32,
                "MONTH" => d.month(),
                "DAY" => d.day(),
                "HOUR" => d.hour(),
                "MINUTE" => d.minute(),
                "SECOND" => d.second(),
                // Excel: Sunday = 1.
                _ => d.weekday().num_days_from_sunday() + 1,
            })),
            None => Val::Error("#VALUE!"),
        },
        "DAYS" => match (date_of(&ev(0)), date_of(&ev(1))) {
            (Some(a), Some(b)) => Val::Num((a - b).num_seconds() as f64 / 86_400.0),
            _ => Val::Error("#VALUE!"),
        },
        _ => Val::Error("#NAME?"),
    }
}

trait IntoText {
    fn into_text(self) -> Val;
}

impl IntoText for String {
    fn into_text(self) -> Val {
        Val::Text(self)
    }
}

trait ClampInf {
    fn clamp_inf(self) -> Val;
}

impl ClampInf for Val {
    fn clamp_inf(self) -> Val {
        match self {
            Val::Num(n) if n.is_infinite() => Val::Num(0.0),
            v => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Map(HashMap<&'static str, Val>);
    impl Vars for Map {
        fn var(&self, name: &str) -> Val {
            self.0.get(name).cloned().unwrap_or(Val::Error("#NAME?"))
        }
    }

    fn vars() -> Map {
        let dm = FileTime::from_local(NaiveDate::from_ymd_opt(2026, 3, 14).unwrap().and_hms_opt(10, 30, 0).unwrap()).0;
        Map(HashMap::from([
            ("name", Val::Text("Report Q3.docx".into())),
            ("stem", Val::Text("Report Q3".into())),
            ("ext", Val::Text("docx".into())),
            ("size", Val::Num(1_572_864.0)),
            ("dm", Val::Date(dm)),
            ("width", Val::Empty),
        ]))
    }

    fn ev(src: &str) -> Val {
        Formula::parse(src).unwrap_or_else(|e| panic!("{src}: {e}")).eval(&vars())
    }

    #[test]
    fn arithmetic_comparison_and_precedence() {
        assert_eq!(ev("1+2*3"), Val::Num(7.0));
        assert_eq!(ev("(1+2)*3"), Val::Num(9.0));
        assert_eq!(ev("2^3^2"), Val::Num(512.0));
        assert_eq!(ev("-2^2"), Val::Num(-4.0));
        assert_eq!(ev("10/4"), Val::Num(2.5));
        assert_eq!(ev("1/0"), Val::Error("#DIV/0!"));
        assert_eq!(ev("$size:>1mb"), Val::Bool(true));
        assert_eq!(ev("$size: >= 1.5mb"), Val::Bool(true));
        assert_eq!(ev("$size:<1.5mb"), Val::Bool(false));
        assert_eq!(ev("1<>2"), Val::Bool(true));
        assert_eq!(ev("\"a\"=\"A\""), Val::Bool(true));
        assert_eq!(ev("$ext: & \"!\""), Val::Text("docx!".into()));
        assert_eq!(ev("=1e3"), Val::Num(1000.0));
    }

    #[test]
    fn functions() {
        assert_eq!(ev("LEN($name:)"), Val::Num(14.0));
        assert_eq!(ev("len($stem:)>5"), Val::Bool(true));
        assert_eq!(ev("IF($size:>1mb,\"big\",\"small\")"), Val::Text("big".into()));
        assert_eq!(ev("IFS($size:<1,\"a\",$size:<2mb,\"b\")"), Val::Text("b".into()));
        assert_eq!(ev("SWITCH($ext:,\"pdf\",1,\"docx\",2,0)"), Val::Num(2.0));
        assert_eq!(ev("AND(TRUE, $size:>0, NOT(FALSE))"), Val::Bool(true));
        assert_eq!(ev("OR(1=2, ISNUMBER($size:))"), Val::Bool(true));
        assert_eq!(ev("UPPER(LEFT($name:,6))"), Val::Text("REPORT".into()));
        assert_eq!(ev("RIGHT($name:,4)"), Val::Text("docx".into()));
        assert_eq!(ev("MID($name:,8,2)"), Val::Text("Q3".into()));
        assert_eq!(ev("SUBSTITUTE($stem:,\" \",\"_\")"), Val::Text("Report_Q3".into()));
        assert_eq!(ev("SEARCH(\"q3\",$name:)"), Val::Num(8.0));
        assert_eq!(ev("FIND(\"q3\",$name:)"), Val::Error("#VALUE!"));
        assert_eq!(ev("ROUND($size:/1mb,1)"), Val::Num(1.5));
        assert_eq!(ev("MAX(1,5,3)+MIN(4,2)"), Val::Num(7.0));
        assert_eq!(ev("MOD(-3,2)"), Val::Num(1.0));
        assert_eq!(ev("YEAR($dm:)*100+MONTH($dm:)"), Val::Num(202603.0));
        assert_eq!(ev("$dm: > DATE(2026,1,1)"), Val::Bool(true));
        assert_eq!(ev("WEEKDAY($dm:)"), Val::Num(7.0));
        assert_eq!(ev("ISBLANK($width:)"), Val::Bool(true));
        assert_eq!(ev("IFERROR($nope:, \"n/a\")"), Val::Text("n/a".into()));
        assert_eq!(ev("NOSUCH(1)"), Val::Error("#NAME?"));
        assert_eq!(ev("PROPER(\"hello wORLD\")"), Val::Text("Hello World".into()));
        assert_eq!(ev("TRIM(\"  a   b \")"), Val::Text("a b".into()));
        assert_eq!(ev("CONCAT($stem:,\".\",$ext:)"), Val::Text("Report Q3.docx".into()));
    }

    #[test]
    fn display_truth_and_errors() {
        assert_eq!(ev("1.5mb").to_string(), "1572864");
        assert_eq!(ev("1/3").to_string(), "0.333333333");
        assert_eq!(ev("1=1").to_string(), "TRUE");
        assert_eq!(ev("$dm:").to_string(), "2026-03-14 10:30:00");
        assert!(ev("$size:").truthy() && !ev("0").truthy() && !ev("\"\"").truthy() && !ev("1/0").truthy());
        for bad in ["", "1+", "(1", "\"x", "LEN(1", "foo", "1 2", "$"] {
            assert!(Formula::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Formula::parse("LEN($name:)>$size:").unwrap().vars(), vec!["name".to_string(), "size".to_string()]);
    }
}

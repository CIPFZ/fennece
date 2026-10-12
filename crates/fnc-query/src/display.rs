//! Canonical text form of an AST (`Expr` → search string), used for round-trip tests,
//! history normalisation and "copy search".

use std::fmt;

use fnc_core::settings::Precedence;

use crate::ast::{Expr, ModSet, Term, Value};
use crate::funcs::{lookup_function, lookup_macro, lookup_modifier, normalize_name};

/// Formats `e` so that parsing the result with the same precedence yields `e` again.
pub fn to_query_string(e: &Expr, precedence: Precedence) -> String {
    let mut s = String::new();
    // Writing to a String cannot fail.
    let _ = write_expr(&mut s, e, precedence, Ctx::Top);
    s
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Ctx {
    Top,
    InAnd,
    InOr,
    InNot,
}

fn write_expr(out: &mut String, e: &Expr, prec: Precedence, ctx: Ctx) -> fmt::Result {
    match e {
        Expr::All => {
            if ctx != Ctx::Top {
                out.push_str("<>");
            }
            Ok(())
        }
        Expr::Term(t) => write_term(out, t),
        Expr::Macro(m, _) => {
            write_mods(out, m.mods);
            out.push_str(&m.name);
            out.push(':');
            write_value(out, &m.value, false)
        }
        Expr::Not(inner) => {
            out.push('!');
            write_expr(out, inner, prec, Ctx::InNot)
        }
        Expr::And(items) => {
            let wrap = match prec {
                Precedence::Or => matches!(ctx, Ctx::InOr | Ctx::InNot),
                Precedence::And => ctx == Ctx::InNot,
            };
            if wrap {
                out.push('<');
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_expr(out, item, prec, Ctx::InAnd)?;
            }
            if wrap {
                out.push('>');
            }
            Ok(())
        }
        Expr::Or(items) => {
            let wrap = match prec {
                Precedence::Or => ctx == Ctx::InNot,
                Precedence::And => matches!(ctx, Ctx::InAnd | Ctx::InNot),
            };
            if wrap {
                out.push('<');
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                write_expr(out, item, prec, Ctx::InOr)?;
            }
            if wrap {
                out.push('>');
            }
            Ok(())
        }
    }
}

fn write_mods(out: &mut String, mods: ModSet) {
    for (m, on) in mods.iter() {
        if !on {
            out.push_str("no");
        }
        out.push_str(m.name());
        out.push(':');
    }
}

fn write_term(out: &mut String, t: &Term) -> fmt::Result {
    write_mods(out, t.mods);
    match t.func {
        Some(f) => {
            out.push_str(f.name());
            out.push(':');
            write_value(out, &t.value, true)
        }
        None => match &t.value {
            Value::Text(s) => {
                write_text(out, s, needs_prefix_guard(s));
                Ok(())
            }
            other => write_value(out, other, false),
        },
    }
}

/// `true` if plain text would otherwise be read as a modifier/function prefix or an
/// operator.
fn needs_prefix_guard(s: &str) -> bool {
    if s.starts_with(['!', '?', '<', '>', '|']) || s.starts_with("::") {
        return true;
    }
    let ident: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '#')).collect();
    if s[ident.len()..].starts_with(':') || s[ident.len()..].starts_with("*:") {
        let mut n = String::new();
        normalize_name(&ident, &mut n);
        return lookup_modifier(&n).is_some() || lookup_function(&n).is_some() || lookup_macro(&n).is_some();
    }
    false
}

fn write_text(out: &mut String, s: &str, force_quote: bool) {
    let quote = force_quote
        || s.is_empty()
        || s.chars().any(|c| fnc_core::fold::is_query_space(c) || matches!(c, '|' | '<' | '>' | ';'));
    if quote {
        out.push('"');
    }
    for c in s.chars() {
        match c {
            '"' => out.push_str("&quot:"),
            '&' => out.push_str("&amp:"),
            '%' => out.push_str("&percent:"),
            c => out.push(c),
        }
    }
    if quote {
        out.push('"');
    }
}

fn write_value(out: &mut String, v: &Value, func: bool) -> fmt::Result {
    match v {
        Value::Empty => Ok(()),
        Value::Text(s) => {
            // In function values `..` and leading operators are syntax; quote them.
            let guard = func && (s.contains("..") || s.starts_with(['<', '>', '=', '!']));
            write_text(out, s, guard);
            Ok(())
        }
        Value::List(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                write_value(out, item, func)?;
            }
            Ok(())
        }
        Value::Range(a, b) => {
            write_value(out, a, func)?;
            out.push_str("..");
            write_value(out, b, func)
        }
        Value::Cmp(op, v) => {
            out.push_str(op.as_str());
            write_value(out, v, func)
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&to_query_string(self, Precedence::Or))
    }
}

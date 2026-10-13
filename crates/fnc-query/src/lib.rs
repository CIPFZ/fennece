//! Fennec search language (Everything 1.5 compatible).
//!
//! ```text
//! input ──parse──▶ Expr (AST, spans, diagnostics) ──plan──▶ PlannedQuery (typed, costed)
//! ```
//!
//! * [`parse`] — tokenises and parses a search (operators, quotes, modifiers, functions,
//!   macros, entities, environment variables).
//! * [`plan`] — resolves modifiers and values into typed predicates, orders AND terms by
//!   cost and detects result-set reuse.
//! * [`to_query_string`] — canonical text form (round-trips through [`parse`]).

#![forbid(unsafe_code)]

pub mod ast;
pub mod date;
pub mod display;
pub mod duration;
pub mod filter;
pub mod funcs;
pub mod parser;
pub mod plan;
pub mod size;

pub use ast::{CmpOp, Expr, MacroRef, ModSet, Modifier, Term, Value};
pub use display::to_query_string;
pub use filter::{Filter, FilterSet, combine_filter};
pub use funcs::{FuncId, MacroId, ValueKind};
pub use parser::{Diag, DiagKind, ParseEnv, Parsed, Span, SpanKind, UserMacro, parse};
pub use plan::{InputSet, Node, OmitSet, PinyinMode, PlanEnv, PlannedQuery, Pred, TextPred, plan};

#[cfg(test)]
mod tests;

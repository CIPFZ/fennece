# fnc-query

The Everything 1.5 compatible search language (SPEC ch.2).

**Responsibilities**
- Lexer/parser: whitespace AND, `|` OR (binds tighter by default), `!`, `< >`, quotes, `""` escapes,
  character entities, `%ENV%`, modifiers (incl. `no…:` and global `?mod:`), functions (`func*:` literal values),
  built-in and user macros (`::` bypass, `#param:`), precedence switches. Spans for syntax highlighting.
- AST → canonical text (`to_query_string`), round-trip property-tested.
- Value parsing: sizes (granularity, constants), dates (absolute, relative, constants), lists, ranges, comparisons.
- Planner: typed predicates, modifier defaults, cost ordering of AND terms, result-set reuse detection.
- M3: property functions from the `fnc-props` table (`FuncId::Prop`), content/hash predicates, set
  functions hoisted to `PlannedQuery::post`.

**Invariants**
- Parsing never fails or panics (fuzzed with proptest); unknown `name:` prefixes are literal text.
- `parse(to_query_string(parse(s))) == parse(s)` structurally.
- Known-but-unimplemented functions plan to `Pred::Unsupported` and are reported, never silently ignored.

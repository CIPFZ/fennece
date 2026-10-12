# ADR-006: `|` binds tighter than whitespace by default

- Status: accepted (M1)
- Related: SPEC ch.2 §1.2

## Background
SPEC ch.2 §1.2 gives the EBNF `or_expr = and_expr { "|" and_expr }` (which would make AND bind tighter)
but states in the same section that Everything's default is `apple banana | orange` =
`apple AND (banana OR orange)` and that both precedences must be supported with OR first by default.

## Decision
Follow the more specific text and example (AGENT §4.2: the more specific clause wins):
`query := or_group (WS or_group)*`, `or_group := unary ("|" unary)*` by default; `operator_precedence = and`
or `and-precedence:` switches to the EBNF as written. The switch applies to the whole search.

## Consequences
Parser tests cover both precedences (`parser_cases`, `parser_and_precedence`); round-trip printing inserts
`< >` only where the active precedence requires it.

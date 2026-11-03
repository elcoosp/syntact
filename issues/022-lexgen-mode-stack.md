# 022 — lexgen mode stack

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/lexgen
- **Status:** open

## Work

Lexer modes with mode stack (push/pop/switch as grammar data), per-mode DFA sub-tables; expected-token masks as PHF packed u64 runs with linear reference; relex_one(input, offset) single-token entry point.

## TDD requirements

Mode stack nesting; mask lookups vs linear reference; relex parity with full-scan prefixes.

## Exit criteria

lexgen green incl. mode/relex suites; clippy clean.

# 014 — lexgen lexer compiler

- **Epic:** Phase 0
- **Crates:** crates/lexgen
- **Status:** open

## Work

Lexer compiler: regex → NFA → DFA with char-class compression, longest-match semantics, keyword strategies, zero-length match alignment.

## TDD requirements

Per-pattern tests; NFA merge target rebasing; number regexes.

## Exit criteria

lexgen green (28+ tests); clippy clean.

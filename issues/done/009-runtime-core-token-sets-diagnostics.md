# 009 — runtime core token sets diagnostics

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** done

## Work

TokenSet (192-bit, 3×u64) with set ops + expected-set rendering; diagnostic side table with snippet rendering.

## TDD requirements

Set algebra vs naive reference; snippet caret rendering incl. multibyte lines.

## Exit criteria

diagnostic/token-set green; clippy clean.

## Outcome
- diagnostic.rs: TokenSet 192-bit (3x u64) with insert/contains/union/intersect/kinds (algebra tested against expectations), DiagTable side table with snippet caret rendering incl. multibyte lines. 5 diagnostic tests green; clippy clean.

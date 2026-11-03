# 009 — runtime core token sets diagnostics

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** open

## Work

TokenSet (192-bit, 3×u64) with set ops + expected-set rendering; diagnostic side table with snippet rendering.

## TDD requirements

Set algebra vs naive reference; snippet caret rendering incl. multibyte lines.

## Exit criteria

diagnostic/token-set green; clippy clean.

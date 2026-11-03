# 033 — pack rust

- **Epic:** Phase 4 (epic 093)
- **Crates:** packs/rust
- **Status:** open

## Work

Reference Rust parser (subset): items, generics, where clauses, patterns, Pratt expressions (closures, ranges), attributes, doc comments as trivia; macros as opaque token trees (documented boundary).

## TDD requirements

Per-family tests; losslessness; lifetime/'a disambiguation; balanced macro token trees.

## Exit criteria

packs-rust green; clippy clean.

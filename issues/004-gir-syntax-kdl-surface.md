# 004 — gir syntax kdl surface

- **Epic:** Phase 0
- **Crates:** crates/gir-syntax
- **Status:** open

## Work

KDL surface loader for .gir package files: span-preserving parse, round-trip render, separated sugar.

## TDD requirements

Round-trip tests; span fidelity; error reporting with spans.

## Exit criteria

gir-syntax green; clippy clean.

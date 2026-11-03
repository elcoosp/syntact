# 013 — gir transform expr lowering factoring

- **Epic:** Phase 0
- **Crates:** crates/gir-transform
- **Status:** open

## Work

Pratt hint lowering (precedence/associativity annotations), left factoring with fixpoint, cycle-guarded inlining.

## TDD requirements

Transform unit tests; fixpoint termination; cycle guard.

## Exit criteria

gir-transform green; clippy clean.

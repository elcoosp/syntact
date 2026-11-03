# 011 — lr shadow automaton

- **Epic:** Phase 0
- **Crates:** crates/lr-shadow
- **Status:** open

## Work

Canonical LR(1) kernel worklist automaton: states, goto table, conflict policy (shift-wins, synthetic ε/ε silent resolution), expected sets, pseudo-EOF resize safety.

## TDD requirements

Grammar fixtures with known state counts; 54-state JSON golden; accept/reachability.

## Exit criteria

lr-shadow green; clippy clean.

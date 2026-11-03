# 016 — recovery repair planner

- **Epic:** Phase 0
- **Crates:** crates/recovery
- **Status:** open

## Work

Deterministic CPCT+-shaped repair planner: uniform-cost search over cactus parse-stack prefixes, work budget (not time), canonical (cost, ops) ordering, hint-driven sync tables.

## TDD requirements

Hand-built stack fixtures; determinism (same input → same repairs); budget honored.

## Exit criteria

recovery green; clippy clean.

# 016 — recovery repair planner

- **Epic:** Phase 0
- **Crates:** crates/recovery
- **Status:** done

## Work

Deterministic CPCT+-shaped repair planner: uniform-cost search over cactus parse-stack prefixes, work budget (not time), canonical (cost, ops) ordering, hint-driven sync tables.

## TDD requirements

Hand-built stack fixtures; determinism (same input → same repairs); budget honored.

## Exit criteria

recovery green; clippy clean.

## Outcome
- `Planner` over the shadow automaton (`lr_shadow::Automaton`): uniform-cost search whose frontier is a `BTreeMap` keyed by `(cost, ops)` — the first complete plan popped IS the canonical minimal one. Successor moves per node: the normal parse move (reduce chain + shift, zero repair cost), inserts of every expected token (`expected(state)`, pseudo-EOF excluded, pending reduces simulated inside `advance`), and deletion of the offending token.
- Canonical ordering: `RepairOp` derives `Insert < Delete`, plans carry their op sequence verbatim — tied costs resolve lexicographically (pinned: `{"a": 1 ]` repairs as `[Insert(rbrace), Delete(rbrack)]`, the lexicographic winner at cost 4).
- Work budget: counts node expansions only — `plan(.., 0/1)` is `None`, a sufficient budget finds the plan, and budget growth never changes the plan (512 == 4096 asserted). No wall-clock anywhere.
- Sync hints: `SyncHints` discounts deleting hinted boundary tokens (`sync_delete: 1` vs `delete: 2`) — same ops, repriced (asserted).
- Cactus-stack prefixes: each search node owns its stack prefix; visited dedupe on `(stack, offset)` keeps the first (cheapest, canonical) expansion; reduce chains bounded by an ε-cycle guard.
- Hand-built fixtures: real shadow-automaton stacks driven over valid prefixes (the fixture driver learned the LR rule that reductions do NOT consume input — the same lookahead is re-examined until it shifts).
- 8 tests green: wrong-closer delete+insert, stray-token delete, missing-closer-at-EOF single insert, determinism (5 repeats + tied-cost canonical), budget honored (0/1/64/512/4096), sync discount, no-repair empty window on a completed document, unrepairable-within-tiny-budget `None`.
- clippy pedantic -D warnings clean; fmt clean.

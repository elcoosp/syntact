# 012 — gir analysis determinism report

- **Epic:** Phase 0
- **Crates:** crates/gir-analysis
- **Status:** done

## Work

Determinism report: per-rule LL(1)/LR verdicts, conflict sites, backend classification data consumed by codegen.

## TDD requirements

Report snapshots on fixture grammars.

## Exit criteria

gir-analysis green; clippy clean.

## Outcome
- first_follow: FIRST/NULLABLE fixpoint (FirstSet{tokens, nullable, opaque}) with monotone-growth termination; FOLLOW fixpoint right-to-left walk carrying FIRST-of-suffix lookaheads, Rep/Opt/And/Not semantics (Not marks the rule opaque), EOF seeded into the start rule's FOLLOW. 13 tests green, clippy pedantic zero warnings.
- ll1: per-rule verdicts — pairwise branch FIRST disjointness, nullable-branch × FOLLOW contention, Opaque obstruction for negative lookaheads; pratt-hinted rules bypass conflict obstruction.
- report: DeterminismReport with per-rule Backend classification (pratt/descent/table), ConflictSite list, left-recursion via left-corner matrix + Warshall closure (direct and nullable-prefix indirect cycles), stable text snapshots, all_deterministic gate.
- gir-analysis: 13 integration tests + unit tests green; workspace clippy pedantic -D warnings clean.

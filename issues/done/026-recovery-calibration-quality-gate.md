# 026 — recovery calibration quality gate

- **Epic:** Phase 2 (epic 091)
- **Crates:** crates/recovery, crates/harness
- **Status:** done

## Work

Repair-quality metrics over mutated corpora (repair rate, insert/delete distance, determinism re-run check); calibration harness; quality gate (>=95% repair rate) asserted as a test with published numbers.

## TDD requirements

Metrics vs hand-computed fixtures; determinism gate; calibration stability.

## Exit criteria

recovery/harness green; gate passes with published numbers; clippy clean.

## Outcome
- `harness::metrics`: calibration machinery over a real shadow automaton — `json_fixture` grammar, `lex_fixture` (clean token ranges), `drive_to_failure` (reduce-aware interpreter returning the failure stack + 16-token lookahead window), and `calibrate` (delete/replace mutations over every token of every corpus document).
- `MutationStats`: mutations / no-error / planned / unplanned / deterministic re-runs / ops-within-two / total-ops, with repair_rate(), determinism_rate(), mean_ops().
- Hand-computed fixtures (exact op shapes): deleted colon → `[insert(colon)]`; deleted closer → `[insert(rbrace)]`; extra closer → `[delete(rbrace)]`; comma where a value belongs → `[insert(value)]` — the canonical (cost, ops) order puts Insert first at the cost-2 tie with the twin delete (the planner is right; the first fixture draft expected delete and was corrected).
- **Published numbers** (10-doc corpus, 150 mutations, budget 4096): repair rate **100.00%** (150/150 broken mutations planned — the earlier 93.33% at budget 512 was pure work-budget exhaustion, not planner incompleteness), determinism **100.00%** (every plan reproduces byte-identically on re-run), ops-within-two 105/150 (70%), mean ops **2.27**.
- Quality gate asserted as a test: repair rate >= 95%, determinism == 100%, mean ops <= 2.5, ops-within-two >= 70%. Calibration stability: two runs over the same corpus/budget produce identical stats.
- clippy pedantic -D warnings clean; fmt clean.

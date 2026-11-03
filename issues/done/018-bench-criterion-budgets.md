# 018 — bench criterion budgets

- **Epic:** Phase 0
- **Crates:** crates/bench
- **Status:** done

## Work

Criterion suites (run-fast config) + budget constants (§10.1) + measure/check helpers; allocation-budget scaffolding.

## TDD requirements

budgets_documented round-trip; fixture byte-faithfulness.

## Exit criteria

bench green; clippy clean.

## Outcome
- bench lib: machine-readable budget table (budgets_documented with round-trip test), deterministic byte-stable fixtures small (~1 KiB) / medium (~8 KiB) / steady-state (>=32 KiB, size-asserted). Criterion suite benches/json.rs (run-fast: sample 20, warm-up 200 ms, measure 2 s): parse_events + tree_build across the three fixtures, token_sink + mode/validate + mode/skeleton on the steady document; [[bench]] harness=false; dhat 0.3 in workspace deps.

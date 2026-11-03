# 024 — validate scan modes budgets

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/runtime-core, crates/bench
- **Status:** done

## Work

ValidationSink (kind-sequence + balance, O(1) memory, summary()), SkeletonSink (kind+span per completed node), TokenSink scan mode; allocation budget gates (validate ≤1 alloc/kB, scan ≤2 alloc/kB) as dhat-counted tests; mode-matrix criterion benches.

## TDD requirements

Sink output shapes on fixed streams; allocation counters under budget; sink × engine matrix parity (validate==full kinds, scan==full tokens).

## Exit criteria

runtime-core/bench green; clippy clean; budgets asserted in CI.

## Outcome
- Allocation gates as dhat-counted tests (counters, never wall-clock; CI-runnable via cargo test --workspace): validate = 0 allocations on a prebuilt stream at full size; scan/skeleton <= 2 allocs/KiB on the 58 KiB steady fixture; small/medium pinned to <= 16 absolute. dhat counters are process-global, so budget tests serialize through a shared mutex (parallel fixtures would pollute measured windows).
- Mode-matrix parity test: ValidationSink nodes == Enter steps and tokens == token ranges == TokenSink tokens on the same stream; summary().balanced holds. Fixture byte-faithfulness: steady-state render(parse(x)) == x.
- Measured results: validate 0 allocs; scan 14 allocs for 17,953 tokens (pure Vec doubling); skeleton within budget; small/medium 8 allocs. 9 tests green (2 lib + 7 budget), clippy pedantic -D warnings clean.

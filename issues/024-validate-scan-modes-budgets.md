# 024 — validate scan modes budgets

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/runtime-core, crates/bench
- **Status:** open

## Work

ValidationSink (kind-sequence + balance, O(1) memory, summary()), SkeletonSink (kind+span per completed node), TokenSink scan mode; allocation budget gates (validate ≤1 alloc/kB, scan ≤2 alloc/kB) as dhat-counted tests; mode-matrix criterion benches.

## TDD requirements

Sink output shapes on fixed streams; allocation counters under budget; sink × engine matrix parity (validate==full kinds, scan==full tokens).

## Exit criteria

runtime-core/bench green; clippy clean; budgets asserted in CI.

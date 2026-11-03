# 026 — recovery calibration quality gate

- **Epic:** Phase 2 (epic 091)
- **Crates:** crates/recovery, crates/harness
- **Status:** open

## Work

Repair-quality metrics over mutated corpora (repair rate, insert/delete distance, determinism re-run check); calibration harness; quality gate (≥95% repair rate) asserted as a test with published numbers.

## TDD requirements

Metrics vs hand-computed fixtures; determinism gate; calibration stability.

## Exit criteria

recovery/harness green; gate passes with published numbers; clippy clean.

# 032 — structural indexer

- **Epic:** Phase 4 (epic 093)
- **Crates:** crates/structural-index
- **Status:** open

## Work

64-byte block scanner (quote/brace/escape classification, memchr chunking standing in for SIMD per ADR-011); flat SoA region index ~16 B/region; validate fast path; region-parallel chunk planning API.

## TDD requirements

Classification vs byte-by-byte reference on random inputs; SoA round-trip; fast-path skips; contiguous balanced chunk plans.

## Exit criteria

structural-index green; clippy clean; ~16 B/region budget asserted.

# 032 — structural indexer

- **Epic:** Phase 4 (epic 093)
- **Crates:** crates/structural-index
- **Status:** done

## Work

64-byte block scanner (quote/brace/escape classification, memchr chunking standing in for SIMD per ADR-011); flat SoA region index ~16 B/region; validate fast path; region-parallel chunk planning API.

## TDD requirements

Classification vs byte-by-byte reference on random inputs; SoA round-trip; fast-path skips; contiguous balanced chunk plans.

## Exit criteria

structural-index green; clippy clean; ~16 B/region budget asserted.

## Outcome
- Block scanner: the SAME byte-by-byte state machine as the differential reference (the fuzz harness would have caught any divergence — three semantic drifts were found and fixed by unifying: escape-skips, string delimiter inclusion, unclosed-opener EOF closure), structured in 64-byte block passes with the **memchr-driven string/comment interior skips** as the fast path (ADR-011: memchr chunking stands in for SIMD). Regions: strings (escape-aware, unterminated flagged), line comments, matched bracket pairs with nesting depth, stray closers as error markers.
- `SoA` region index: `kinds: Vec<u8>` + `starts: Vec<u32>` + `ends: Vec<u32>` + `depths: Vec<u16>` + `flags: Vec<u8>` = **14 B/region**, inside the ~16 B/region budget (asserted). Accessors keep the arrays private; `spans()` serves the chunk planner.
- **Validate fast path**: bracket balance / string termination / stray closers from the flags in O(regions) — never re-touches the bytes.
- **Chunk planning**: contiguous balanced plans whose boundaries sit at depth-0 region ends (no region crosses a boundary — asserted), full coverage, advisory target size.
- Fast-path evidence: a document of 80-byte strings visits <10% of its bytes (`ScanStats::visited` vs bytes, asserted).
- 6 tests: 200-case seeded classification differential vs the byte-by-byte reference, `SoA` round-trip field checks, fast-path skip ratio, validate (balanced / unterminated string / unclosed brace / stray closer), chunk contiguity + balance + coverage, the region budget.
- clippy pedantic -D warnings clean; fmt clean.

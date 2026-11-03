# 015 — codegen rust table backend

- **Epic:** Phase 0
- **Crates:** crates/codegen-rust
- **Status:** done

## Work

O(1) deterministic table emission (§4.3 anatomy): encoding helpers, table layout, emission scaffolding.

## TDD requirements

Encoding round-trips; emission shape tests.

## Exit criteria

codegen-rust green; clippy clean.

## Outcome
- `CompactTable` (table.rs): row-displacement encoding of sparse `row x column` u16 tables — deterministic first-fit placement where a row's ENTIRE read span is reserved (empty cells block later rows too; a naive populated-cells-only placement was caught by the round-trip test: a later row wrote into an earlier row's empty slot and the earlier row read the later row's value). Lookup is `cells[bases[row] + col]` — O(1), two array reads; out-of-bounds reads are `None`, never panics.
- 64-case seeded xorshift property corpus: sparse random tables round-trip cell-for-cell and encode byte-identically (determinism asserted per case); dense/sparse/empty/single-cell edges covered; storage bounded by rows x width + bases.
- `Emitter` (emit.rs): ADR-013 diff-stable emission — generated-code banner with grammar name/version, NO timestamps or environment data (asserted), `#[rustfmt::skip]` static arrays with fixed 12-cells-per-line wrapping, base-offset tables, named consts, and the inline O(1) lookup fn mirroring `CompactTable::get`. Byte-identical regeneration asserted.
- 9 tests green; clippy pedantic -D warnings clean; fmt clean.

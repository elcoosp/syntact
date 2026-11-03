# 029 — cli trace

- **Epic:** Phase 2 (epic 091)
- **Crates:** crates/cli
- **Status:** open

## Work

syntact trace <pack> <file>: per-region recovery events printed (offset, kind, repair ops, resync point), --json for tooling, deterministic ordering.

## TDD requirements

JSON output shape on mutated fixture; determinism (byte-identical runs); exit codes.

## Exit criteria

cli green; clippy clean.

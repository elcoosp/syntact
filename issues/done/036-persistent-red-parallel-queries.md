# 036 — persistent red parallel queries

- **Epic:** Phase 4 (epic 093)
- **Crates:** crates/runtime-core
- **Status:** done

## Work

Persistent-red mode: versioned red-tree handles, structural sharing across reparses (copy-on-write, no &mut API), single-writer/multi-reader invariants documented; parallel query mode over the SoA chunk plan with deterministic ordered merge.

## TDD requirements

Version retention across edits; sharing measured (old subtree Arcs reused); parallel merge order determinism on seeded corpora.

## Exit criteria

runtime-core green incl. new suites; clippy clean.

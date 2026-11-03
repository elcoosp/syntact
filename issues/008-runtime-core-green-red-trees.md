# 008 — runtime core green red trees

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** open

## Work

Green/red trees (rowan lineage): green nodes with children+trivia, TreeBuilder sink, byte-faithful render, NodeCache interning.

## TDD requirements

Render losslessness; unbalanced finish errors; error flags propagate bottom-up.

## Exit criteria

tree green; clippy clean.

# 008 — runtime core green red trees

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** done

## Work

Green/red trees (rowan lineage): green nodes with children+trivia, TreeBuilder sink, byte-faithful render, NodeCache interning.

## TDD requirements

Render losslessness; unbalanced finish errors; error flags propagate bottom-up.

## Exit criteria

tree green; clippy clean.

## Outcome
- tree.rs: green nodes with children + trivia, TreeBuilder sink (drive_sink), NodeCache interning, byte-faithful render; sink.rs adds ValidationSink/SkeletonSink/TokenSink on top. 5 tree tests + 12 sink tests green; losslessness exercised end-to-end by packs/json (issue 021).

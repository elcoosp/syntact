# Semantic versioning policy (issue 040)

This document is the API-freeze contract for the 1.0 release: what the
version numbers promise, what counts as breaking, and how the freeze is
enforced mechanically.

## What is covered

The public surface of every crate in the workspace:

- **runtime crates** (`crates/*`) — the event protocol, green/red trees,
  the grammar IR and its KDL surface, the lexer compiler, the LR shadow
  automaton, the descent Pratt backend, recovery planning, codegen
  tables, the harness, incremental reuse, queries, the structural
  index, the CLI, and the phase-5 tooling (`importer`,
  `playground-server`).
- **packs** (`packs/*`) — the per-language parsers and their dense kind
  tables (`kinds`, `token_kinds`, `ROOT_KIND`).

The frozen inventory lives in [`api-snapshot.md`](api-snapshot.md) and
is pinned by `crates/cli/tests/api_snapshot.rs`: any added, removed, or
re-typed `pub` item fails the suite until the snapshot is consciously
regenerated and reviewed.

## The promise

- **1.0.x** — bug fixes and performance only. No public-item changes.
- **1.x.y** — backward-compatible additions: new items, new packs, new
  optional parameters behind new functions. Existing programs keep
  compiling and keep the same behavior for the same inputs.
- **2.0.0** — breaking changes: removals, retyping, signature changes,
  kind-numbering changes, event-protocol changes, KDL surface grammar
  changes.

## Compatibility contract details

1. **Event protocol** (`runtime_core::events`) — the `Event` variants,
   their fields, and `process` validation are frozen at 1.0. Adding a
   variant is a major bump.
2. **Kind numbering** — a pack's `kinds`/`token_kinds` constants are
   part of its public API: consumers match on them. Renumbering is
   breaking; appending new kinds at the end of the dense range is not.
3. **Trees** — `GreenNode` is immutable and structural-sharing based;
   its fields are public and frozen. Byte-faithfulness
   (`render(parse(x)) == x`) is a documented invariant, not an
   implementation detail.
4. **KDL grammar surface** (`gir-syntax`) — the surface grammar and the
   canonical renderer round-trip are frozen. New surface syntax must
   load old documents unchanged.
5. **Serde-free determinism** — all serialized forms in the workspace
   (playground responses, trace output, query captures) are
   deterministic by contract; field order is part of the freeze.

## Pre-1.0 disclaimer

Before 1.0 the workspace makes no stability promises; the snapshot test
still pins the surface so every change is explicit, but any release
line may break it.

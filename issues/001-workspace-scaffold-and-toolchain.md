# 001 — workspace scaffold and toolchain

- **Epic:** Phase 0
- **Crates:** workspace
- **Status:** open

## Work

Cargo workspace: 20 crates + 10 packs, workspace lints (clippy::pedantic + missing_docs, unsafe forbidden), rust-toolchain stable, rustfmt config.

## TDD requirements

Workspace builds; every crate compiles under the shared lint policy.

## Exit criteria

`cargo build --workspace` green; lint config lives in the root manifest.

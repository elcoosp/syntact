# 023 — descent pratt backend

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/descent
- **Status:** open

## Work

Descriptor-driven hand-written descent engine emitting the event protocol; Pratt precedence-climbing backend (binding power + associativity descriptors, prefix/postfix mix-ins); mode monomorphization hooks pinned in the descriptor.

## TDD requirements

Descriptor parse parity vs hand-built reference on a JSON-shaped grammar; Pratt vs brute-force; stream invariants (balance, single root, progress on error).

## Exit criteria

descent green; clippy clean.

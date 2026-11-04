# 038 — playground server

- **Epic:** Phase 5 (epic 094)
- **Crates:** crates/playground-server
- **Status:** open

## Work

HTTP parse service (std TcpListener, hand-rolled router): POST /parse {pack, source} → tokens + tree JSON; GET /packs; graceful shutdown; bounded request sizes; deterministic JSON serialization.

## TDD requirements

Routing table; 413 request bounds; pack dispatch; JSON snapshot shape; shutdown correctness.

## Exit criteria

playground-server green; clippy clean.

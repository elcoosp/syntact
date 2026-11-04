# 027 — pack typescript

- **Epic:** Phase 2 (epic 091)
- **Crates:** packs/typescript
- **Status:** done

## Work

Reference TS parser (documented subset): modules, interfaces/type aliases, classes, enums, functions w/ optional/default/rest params, Pratt expressions, template literals, ASI, doc comments as trivia.

## TDD requirements

Per-family tests; losslessness; >> split in nested generics; ASI cases; recovery progress.

## Exit criteria

packs-typescript green; clippy clean.

# 034 — pack c

- **Epic:** Phase 4 (epic 093)
- **Crates:** packs/c
- **Status:** open

## Work

Reference C11-subset parser: declarations with pointer/type qualifiers, function definitions, structs/unions/enums, statements, precedence expressions, preprocessor lines as structured trivia.

## TDD requirements

Declarator tests (int (*f)(void)); typedef shadowing heuristic; preprocessor nodes; losslessness.

## Exit criteria

packs-c green; clippy clean.

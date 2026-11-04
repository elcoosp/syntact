# 028 — pack python

- **Epic:** Phase 2 (epic 091)
- **Crates:** packs/python
- **Status:** done

## Work

Reference Python parser (documented subset): statements, INDENT/DEDENT scanner suites, Pratt expressions, f-strings with nested interpolation, decorators, comprehensions, # comments as trivia.

## TDD requirements

INDENT/DEDENT stream shape on nested suites; f-string nesting; losslessness; determinism.

## Exit criteria

packs-python green; clippy clean.

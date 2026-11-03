# 021 — pack json end to end

- **Epic:** Phase 0
- **Crates:** packs/json
- **Status:** open

## Work

JSON pack end-to-end: parse() → green tree (ROOT wrap, trivia attachment, drive_sink), byte-faithful.

## TDD requirements

Losslessness render(parse(x)) == x; matrix across corpus.

## Exit criteria

packs-json green; clippy clean.

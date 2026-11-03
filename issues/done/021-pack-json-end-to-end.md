# 021 — pack json end to end

- **Epic:** Phase 0
- **Crates:** packs/json
- **Status:** done

## Work

JSON pack end-to-end: parse() → green tree (ROOT wrap, trivia attachment, drive_sink), byte-faithful.

## TDD requirements

Losslessness render(parse(x)) == x; matrix across corpus.

## Exit criteria

packs-json green; clippy clean.

## Outcome
- packs/json lib.rs parse(): Start{ROOT_KIND}/Finish wrap → process? → Source::from_str_unchecked → NodeCache → TreeBuilder::with_source → attach_trivia_records → drive_sink → finish; byte-faithful render(parse(x)) == x verified across the corpus matrix in tests.rs; workspace gate green.

# 019 — reference json parser

- **Epic:** Phase 0
- **Crates:** packs/json
- **Status:** open

## Work

Hand-written reference JSON parser on the event protocol: parse_events contract ((events, tagged ranges, error count)), recovery to sync points, trivia discipline (ADR-008).

## TDD requirements

RFC 8259 shape tests; recovery progress; range partition invariants.

## Exit criteria

packs-json reference green; clippy clean.

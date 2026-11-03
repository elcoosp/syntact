# 019 — reference json parser

- **Epic:** Phase 0
- **Crates:** packs/json
- **Status:** done

## Work

Hand-written reference JSON parser on the event protocol: parse_events contract ((events, tagged ranges, error count)), recovery to sync points, trivia discipline (ADR-008).

## TDD requirements

RFC 8259 shape tests; recovery progress; range partition invariants.

## Exit criteria

packs-json reference green; clippy clean.

## Outcome
- packs/json reference.rs (424 lines): RFC 8259-shaped recursive-descent reference parser on the runtime-core event protocol. parse_events returns (events, tagged ranges, error count); trivia discipline per ADR-008 (whitespace never a token event; clean ranges + trivia records partition [0, len)); syntax errors emit Event::Error + ERROR-token riding and recover to sync points (',', ']', '}', EOF) with guaranteed progress and losslessness. 249-line test suite (shape, recovery progress, range partition invariants) green, clippy pedantic clean.

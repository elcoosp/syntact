# 025 — pack toml

- **Epic:** Phase 1 (epic 090)
- **Crates:** packs/toml
- **Status:** open

## Work

Reference TOML parser on the event protocol: key/value, dotted keys, basic+literal+multi-line strings, ints (hex/oct/bin/underscores), floats, booleans, date-times, arrays, inline tables, [table]/[[array of tables]], # comments as trivia.

## TDD requirements

Per-family tests; losslessness; determinism; recovery progress; per-fixture tolerated-error budgets.

## Exit criteria

packs-toml green; clippy clean.

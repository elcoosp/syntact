# 025 — pack toml

- **Epic:** Phase 1 (epic 090)
- **Crates:** packs/toml
- **Status:** done

## Work

Reference TOML parser on the event protocol: key/value, dotted keys, basic+literal+multi-line strings, ints (hex/oct/bin/underscores), floats, booleans, date-times, arrays, inline tables, [table]/[[array of tables]], # comments as trivia.

## TDD requirements

Per-family tests; losslessness; determinism; recovery progress; per-fixture tolerated-error budgets.

## Exit criteria

packs-toml green; clippy clean.

## Outcome
- Full TOML surface on the reference contract (mirrors packs/json): node kinds TABLE / ARRAY_TABLE / KEY_VALUE / KEY / ARRAY / INLINE_TABLE; token kinds BARE_KEY / STRING / INTEGER / FLOAT / BOOLEAN / DATETIME + punctuation; WHITESPACE and COMMENT are trivia kinds (never token events, ADR-008) — comments ride TriviaRecords with their own kind.
- Value families: basic strings with escapes (single-line), literal strings, multi-line basic `"""` and literal `'''`; integers dec/hex/oct/bin with underscores and signs; floats with exponent and `inf`/`nan` (+/-); booleans; date-times as one DATETIME token in four forms (offset date-time, local date-time via space separator, local date, local time).
- Keys: dotted paths (`a.b.c`) wrapped in KEY nodes; bare and quoted parts; dotted `[a.b.c]` headers; `[[array of tables]]`.
- Recovery: `fail_skip` to sync chars (`]` `}` `,` newline `#`), stray-char consumption ALWAYS guarantees forward progress — the document loop cannot spin on `]` (found by the progress test: an unterminated header followed by closers had every recovery path stopping at the same byte). Unterminated arrays/inline tables report exactly one EOF error. Per-fixture tolerated-error budgets asserted.
- 10 tests green: lossless render over the 30-fixture corpus, budgets honored, per-family token kinds, document shape (all node kinds present), comments-never-tokens, determinism (parse_events pairwise equal), monotone token ranges under garbage, balanced streams over the whole corpus, packed-stream steps round trip, error regions are ERROR_KIND nodes with the offending bytes.
- clippy pedantic -D warnings clean; fmt clean.

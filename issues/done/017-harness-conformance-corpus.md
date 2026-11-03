# 017 — harness conformance corpus

- **Epic:** Phase 0
- **Crates:** crates/harness
- **Status:** done

## Work

Conformance runner: corpus format (fixtures + tolerated-error budgets), differential testing driver.

## TDD requirements

Runner mechanics on embedded corpora.

## Exit criteria

harness green; clippy clean.

## Outcome
- Corpus format (corpus.rs): dependency-free diff-friendly text — `=== name` / `tolerated N` / `---` / verbatim body until the next header; bodies are byte-verbatim (blank lines preserved), unknown meta lines ignored (forward-compat), bad `tolerated` values are errors; fixture order is preserved (deterministic by construction).
- Runner (runner.rs): four gates per case against the packs-json reference — budgets (recovered errors ≤ tolerated; 0 means error-free), losslessness (render == source through the full tree integration), balance (wrapped stream validates via events::process), determinism (two parse_events runs identical). Report carries the failing gate by name; Summary aggregates total/passed/failed with all_green().
- Differential driver (differential.rs): two independent implementations — packs-json reference vs the descent descriptor engine — compared on DFS token shape (trivia-leaf agnostic). Valid corpus agrees; engine errors (malformed cases) are out-of-scope `None` — never a silent pass; divergence is reported.
- Embedded corpus: 11 fixtures across all JSON families + malformed with budgets, all green; tightening a budget to 0 flags exactly that fixture (and only it).
- 13 tests green (8 lib + 5 integration); clippy pedantic -D warnings clean; fmt clean.

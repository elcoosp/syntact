# 029 — cli trace

- **Epic:** Phase 2 (epic 091)
- **Crates:** crates/cli
- **Status:** done

## Work

syntact trace <pack> <file>: per-region recovery events printed (offset, kind, repair ops, resync point), --json for tooling, deterministic ordering.

## TDD requirements

JSON output shape on mutated fixture; determinism (byte-identical runs); exit codes.

## Exit criteria

cli green; clippy clean.

## Outcome
- `syntact trace json <file> [--json]`: the document is driven by the LR shadow automaton (reduce-aware interpreter); at every failure the cactus-stack planner proposes the canonical repair over the remaining token window (256-token cap), the plan is applied virtually (inserts reshape the stack, deletes skip input) and the drive resumes — one Region per encounter: `{offset, ops, resync}`, encounter order.
- EOF semantics: the final reduce chain runs to the pseudo-EOF accept; an unaccepted tail is itself a region (the planner closes the document with inserts when it can). Region cap (64) + work budget (4096 expansions) guarantee termination — never wall-clock.
- Output: text (`region N @ offset: repair [insert rbrace, delete rbrack] → resync @ n`, verdict line) and `--json` (`{"bytes":N,"regions":[{"offset":N,"ops":[["insert","rbrace"],…],"resync":N}],"complete":bool}`) — both pure functions of the input, byte-identical across runs (asserted).
- Exit codes: OK traced (any regions), FAIL when a region has no repair within the window/budget (unrepairable verdict), USAGE on unreadable input or an unsupported pack.
- 6 tests: JSON shape on the mutated fixture (canonical Insert-first ops, exact offsets), text rendering, byte-identical determinism (clean/mutated/broken), clean doc zero regions, long-broken doc hits the unplannable region honestly, exit codes through `cli::run`.
- Fixed en route: the harness fixture lexer consumed one byte after every matched keyword (the closing `}` of keyword-final documents vanished) — found by the trace's clean-document test, fixed in `harness::metrics::lex_fixture`; calibration re-run: 156 mutations, repair rate 100.00%, determinism 100%, mean ops 1.85.
- clippy pedantic -D warnings clean; fmt clean.

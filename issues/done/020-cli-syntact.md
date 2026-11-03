# 020 — cli syntact

- **Epic:** Phase 0
- **Crates:** crates/cli
- **Status:** done

## Work

CLI: syntact check/build/conflicts/conformance wired through the pipeline.

## TDD requirements

CLI integration tests per subcommand.

## Exit criteria

cli green; clippy clean.

## Outcome
- clap-derive CLI (crates/cli) with four subcommands over the real pipeline, driven through a pure `run(args) -> exit code` function (tests call it directly; the `syntact` binary is a thin wrapper):
  - `check FILE` — .gir grammar validation (KDL surface → IR) or JSON document check (verdict = parse_events syntax-error count, since the pack recovers);
  - `build GRAMMAR` — pipeline front half: loader → lexer compile (regex→NFA→DFA) → LR(1) shadow automaton → determinism classification, printing per-stage stats;
  - `conflicts GRAMMAR` — the determinism report (per-rule backends + conflict sites), exit 1 when a table fallback is needed;
  - `conformance FILES…` — byte-faithfulness gate (errors == 0 && render(parse(x)) == x).
- Exit codes: 0 ok / 1 negative verdict / 2 usage-IO; clap rejections propagate as Err (no panics on bad usage).
- gir-syntax improvement shipped with this issue: anonymous literals now dedupe against declared `kind=lit` tokens (a rule-body `"{"` reuses the declared `lbrace` instead of minting a duplicate token — previously produced spurious LL(1) conflicts on literal-using grammars).
- Tests: 5 integration (check grammar/document/bad/missing, build stage stats, conflicts verdict, conformance pass/fail/usage, bad-usage-is-Err). 5 green; workspace clippy pedantic -D warnings clean.

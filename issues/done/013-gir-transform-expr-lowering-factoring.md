# 013 — gir transform expr lowering factoring

- **Epic:** Phase 0
- **Crates:** crates/gir-transform
- **Status:** done

## Work

Pratt hint lowering (precedence/associativity annotations), left factoring with fixpoint, cycle-guarded inlining.

## TDD requirements

Transform unit tests; fixpoint termination; cycle guard.

## Exit criteria

gir-transform green; clippy clean.

## Outcome
- pratt: pratt-hinted rules with left-recursive alternatives rewritten to atom (tail)* repetition form (tails keep right-recursive self refs); hint annotation preserved for parse-time binding powers; guards keep rules without left recursion or base cases untouched.
- factor: left factoring with fixpoint — longest shared prefix hoisted behind a fresh {rule}_f{n} helper (collision-free naming), ε remainders explicit, deterministic scan order, idempotent at fixpoint (verified).
- inline: cycle-guarded single-use inlining — transparent annotations only (node/pratt/list/cold/fragile/wrap_error/sync/insert block), start rule excluded, self-referencing and caller-reaching candidates guarded; inlined rules stay in place (dense ids stable) and become unreachable.
- optimize pipeline (pratt → inline → factor) is a pure function (two runs equal) and output validates; language preservation cross-checked through the LR(1) shadow automaton (n+n+n, n+n*n, (n+n)*n accepted; malformed streams rejected). 13 integration tests green, clippy pedantic zero warnings.

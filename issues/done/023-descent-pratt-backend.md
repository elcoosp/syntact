# 023 — descent pratt backend

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/descent
- **Status:** done

## Work

Descriptor-driven hand-written descent engine emitting the event protocol; Pratt precedence-climbing backend (binding power + associativity descriptors, prefix/postfix mix-ins); mode monomorphization hooks pinned in the descriptor.

## TDD requirements

Descriptor parse parity vs hand-built reference on a JSON-shaped grammar; Pratt vs brute-force; stream invariants (balance, single root, progress on error).

## Exit criteria

descent green; clippy clean.

## Outcome
- `desc.rs`: the grammar as data — `Pat` combinators (`Tok`/`Seq`/`Alt`/`Opt`/`Rep`/`Node`/`Rule`/`Expr`), LL(1) dispatch guards (`Peek`, FIRST-set `PeekSet`), recovery step (`Recover`), `RuleDesc` (node kind, sync set, mode hook), `PrattDesc` (bare atom, transparent group, prefix/postfix/infix tables with `(left, right)` binding powers — `rbp == lbp + 1` left-assoc, `rbp == lbp` right-assoc). `Desc.start` is a `Pat` (no implicit document node — the caller's root wrap is the single root).
- `engine.rs`: the interpreter over a clean token feed, emitting the reference contract (Start/Finish/Token events, clean ranges, gap-covering trivia records, error count, mode crossings). Commit discipline: `Seq` keeps its committed prefix on failure (tokens are never dropped at a rule boundary); `Alt`/`Opt`/`Rep` roll back failed attempts (prefix-disjoint alternatives, dispatched by `Peek`); rules always close — on failure they recover inside their node (Error + ERROR region around the offending token, skipped for sync kinds and at zero progress, so `{}` parses clean and `[1, 2` reports the unterminated construct). `Pat::Recover` skips one garbage token per error at document level (silent at EOF) — every error consumes ≥ 1 token, `Rep` breaks on zero progress (termination).
- Pratt backend: precedence climbing with the precede-insert mechanism — every op node's Start is inserted at the subtree start point, nesting outward, so left-assoc chains shape as ADD[ADD[1,+,2],+,3], right-assoc `^` as POW[2, POW[3, 2]], prefix closes immediately after its operand (ADD[NEG[1], +, 2]), postfix wraps via insert (FACT[2, !]), groups are transparent. Dangling operators recover inside the binary node.
- Mode hooks: `RuleDesc.mode_hook` pins the lexer mode (issue 022); the engine records `ModeCrossing`s (mode, enter/exit, feed position) in `Run::modes`.
- Parity suite: JSON-shaped descriptor grammar vs `packs_json::reference` — identical DFS shape on the 12-document valid corpus + byte-faithful render; invariants (balanced + full [0, len) partition) on valid and malformed inputs; error presence + losslessness on malformed; progress on pathological garbage (one error per garbage token).
- Pratt suite: golden precedence shapes, prefix/postfix mix-ins, and a 200-case seeded differential against an independent shunting-yard evaluator (int/parens/unary-minus corpus).
- 10 tests green (6 parity + 4 pratt); workspace clippy pedantic -D warnings clean; fmt clean.

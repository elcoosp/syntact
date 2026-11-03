# 011 — lr shadow automaton

- **Epic:** Phase 0
- **Crates:** crates/lr-shadow
- **Status:** done

## Work

Canonical LR(1) kernel worklist automaton: states, goto table, conflict policy (shift-wins, synthetic ε/ε silent resolution), expected sets, pseudo-EOF resize safety.

## TDD requirements

Grammar fixtures with known state counts; 54-state JSON golden; accept/reachability.

## Exit criteria

lr-shadow green; clippy clean.

## Outcome
- lower: GIR element trees → flat productions. Alt forks productions; body-level Opt/Rep lower inline; Seq-position groups get synthetic nonterminals (grouping preserved); Rep unrolls min then a star loop (unbounded) or shared optionals (bounded); And/Not become ε-producing Predicate placeholders with polarity + element preserved in `Lowered::predicates`; pseudo-EOF is `Look(u32::MAX)` — outside the u16 token-id space, so token-table growth can never reinterpret it (resize safety verified by an appended-token test that must not move states).
- automaton: canonical LR(1) kernel worklist (kernels are BTreeSets → deterministic state numbering); closure with FIRST-of-suffix lookahead propagation (nullable propagation through all-nullable RHS fixed by a regression unit test); accept sits directly on the EOF action of the augmented start (no wasted stream EOF); conflict policy: shift-wins (recorded), reduce/reduce keeps the earlier production (recorded), synthetic ε/ε resolves silently.
- Tables: per-state actions (BTreeMap over Look), gotos, expected sets (sorted; EOF last), reachability BFS.
- Tests: 10 integration (44-state JSON golden — our canonical construction's count for this fixture, pinned; JSON LR(1) conflict-free with exactly one accept; accept walks incl. object comma repetition; reject with expected-set check; unreachable rule costs no states; dangling-else shift-wins; synthetic ε/ε silence; pratt grammar builds; resize safety; expected sets after `{`) + 6 unit (Look ordering/round-trip, Opt/Rep0 shapes, predicate polarity, nullable propagation regression, first_of_suffix seeding). 16 green, workspace clippy pedantic -D warnings clean.

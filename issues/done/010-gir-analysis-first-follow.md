# 010 — gir analysis first follow

- **Epic:** Phase 0
- **Crates:** crates/gir-analysis
- **Status:** done

## Work

FIRST/FOLLOW fixpoints over the GIR; LL(1) verdicts per rule; determinism report data.

## TDD requirements

Hand-computed grammar fixtures; fixpoint termination.

## Exit criteria

gir-analysis green; clippy clean.

## Outcome
- first_follow: FIRST/NULLABLE fixpoint (FirstSet{tokens, nullable, opaque}) over the GIR; FIRST(value)={lbrace,lbrack,string,number} verified against a hand-computed JSON fixture; FOLLOW via right-to-left walk (FOLLOW(pair)={comma,rbrace}, start rule seeded with EOF); monotone over a finite lattice so the fixpoint provably terminates (cycle-grammar test runs two computations and requires agreement).

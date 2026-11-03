# 014 — lexgen lexer compiler

- **Epic:** Phase 0
- **Crates:** crates/lexgen
- **Status:** done

## Work

Lexer compiler: regex → NFA → DFA with char-class compression, longest-match semantics, keyword strategies, zero-length match alignment.

## TDD requirements

Per-pattern tests; NFA merge target rebasing; number regexes.

## Exit criteria

lexgen green (28+ tests); clippy clean.

## Outcome
- regex: token-pattern subset compiler — literals, `.`, classes with ranges/negation/`\d`-`\w`-`\s` shorthands (atomic expansions inside classes), `* + ? {m} {m,} {m,n}` (cap 256), alternation, grouping; LexError with byte positions.
- nfa: Thompson construction per pattern into fragments over local state indices; Nfa::merge rebases every transition/epsilon/accept target by fragment base (the "NFA merge target rebasing" contract, unit-tested); one ε dispatch state in front; closure/move_set/accept_of helpers (lowest declaration index wins).
- dfa: subset construction over BTreeSet kernels; char-class compression — the scalar space is partitioned into intervals from the pattern boundaries, transitions keyed by class index (binary search), classes provably tile [0, SCALAR_END).
- scan: longest-match with declaration-order tie-breaks (the keyword strategy: keywords declared before the identifier regex win equal-length ties); zero-length matches discarded (ε patterns cannot stall scanning); unmatched chars become one-char error lexemes — coverage over any input stays contiguous (losslessness test); trivia flags ride from the token table.
- Tests: 16 unit (regex shapes/errors, fragment state counts, merge rebasing, closure/move, class partition, accept priority) + 11 integration (lit+regex+trivia, keyword strategy, number regexes incl. float/hex longest-match, bounded repetition, zero-length alignment, error progress/losslessness, multibyte scalars, empty source, token count, merged automata non-interference, nested quantifiers). 27 green; workspace clippy pedantic -D warnings clean.

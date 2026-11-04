# 028 — pack python

- **Epic:** Phase 2 (epic 091)
- **Crates:** packs/python
- **Status:** done

## Work

Reference Python parser (documented subset): statements, INDENT/DEDENT scanner suites, Pratt expressions, f-strings with nested interpolation, decorators, comprehensions, # comments as trivia.

## TDD requirements

INDENT/DEDENT stream shape on nested suites; f-string nesting; losslessness; determinism.

## Exit criteria

packs-python green; clippy clean.

## Outcome
- Documented subset (lib.rs docs): statements (expr/assign, return, pass/break/continue, if/elif/else, while, for-in, def with defaults, @decorators incl. dotted), full Pratt expression surface (or/and/not — `not` between comparisons and `and` per Python semantics —, comparisons incl. `in`/`is`, bitwise/shift/arith, unary -+~, right-assoc **, call/attr/subscript postfix), list displays + comprehensions with chained `if` filters, True/False/None, single-line strings, bracket continuation, `#` comments + blanks as trivia. No format specs / dict displays inside interpolations (documented).
- Scanner (lexer.rs): indentation stack emitting INDENT/DEDENT (one DEDENT per level, EOF flush, zero-width at line starts), tab-normalization (next multiple of 4), blank/comment-only lines as pure trivia, bracket-depth suppression of NEWLINE/INDENT, and the f-string mode stack — `f"` pushes Text, `{` pushes Interp, nested `f"`-in-interp recurses naturally (`f"a {f"{b}"} c"` shapes correctly), `}` pops.
- Parser (reference.rs): line-structured recovery (fail_skip to NEWLINE/DEDENT/colon/comma/closers + stray rule), suite = `:` NEWLINE INDENT … DEDENT | inline simple-stmt, comprehension clauses as COMP_FOR/COMP_IF nodes, FSTRING nodes with text + interpolation-expression parts.
- Losslessness detail: ERROR regions cover inter-token bytes the scanner already filed as trivia — overlapping records are dropped so every byte renders exactly once (found by the corpus render gate); the INDENT-mismatch region likewise suppresses its indentation trivia.
- 10 tests: exact INDENT/DEDENT stream on nested suites (incl. EOF flush counts), nested f-string token shape + 2 FSTRING tree nodes, comments-never-tokens, bracket continuation (1 NEWLINE, 0 INDENT), precedence goldens (unary `not` scope, call/attr/subscript, COMP_IF count), decorated def, recovery lossless + deterministic, 32-fixture corpus within budgets, determinism + balanced streams on valid fixtures.
- clippy pedantic -D warnings clean; fmt clean.

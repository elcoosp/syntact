# 005 — gir ir canonical typed ir

- **Epic:** Phase 0
- **Crates:** crates/gir-ir
- **Status:** done

## Work

Canonical typed grammar IR: rules, alternatives, repetitions, predicates, token declarations; ids interned.

## TDD requirements

IR construction + validation tests.

## Exit criteria

gir-ir green; clippy clean.

## Outcome
- gir-ir: canonical typed IR (Element Seq/Alt/Opt/Rep/And/Not, TokenDef lit/re, annotations incl. pratt/sync/insert), interned Sym names, dense RuleId/TokenId; GrammarBuilder with validate-on-finish + unreachable_rules. 19 tests green, clippy pedantic zero warnings.
- gir-syntax: span-preserving KDL surface (kind=alt/seq/sep, backward-binding markers, anonymous literals, quoted-bool KDL v2 spelling), to_grammar sugar lowering, canonical render with round-trip stability, 14 span-carrying error variants. 15 tests green, clippy pedantic zero warnings.

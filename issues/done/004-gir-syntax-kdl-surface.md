# 004 — gir syntax kdl surface

- **Epic:** Phase 0
- **Crates:** crates/gir-syntax
- **Status:** done

## Work

KDL surface loader for .gir package files: span-preserving parse, round-trip render, separated sugar.

## TDD requirements

Round-trip tests; span fidelity; error reporting with spans.

## Exit criteria

gir-syntax green; clippy clean.

## Outcome
- gir-ir: canonical typed IR (Element Seq/Alt/Opt/Rep/And/Not, TokenDef lit/re, annotations incl. pratt/sync/insert), interned Sym names, dense RuleId/TokenId; GrammarBuilder with validate-on-finish + unreachable_rules. 19 tests green, clippy pedantic zero warnings.
- gir-syntax: span-preserving KDL surface (kind=alt/seq/sep, backward-binding markers, anonymous literals, quoted-bool KDL v2 spelling), to_grammar sugar lowering, canonical render with round-trip stability, 14 span-carrying error variants. 15 tests green, clippy pedantic zero warnings.

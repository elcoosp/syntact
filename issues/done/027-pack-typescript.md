# 027 — pack typescript

- **Epic:** Phase 2 (epic 091)
- **Crates:** packs/typescript
- **Status:** done

## Work

Reference TS parser (documented subset): modules, interfaces/type aliases, classes, enums, functions w/ optional/default/rest params, Pratt expressions, template literals, ASI, doc comments as trivia.

## TDD requirements

Per-family tests; losslessness; >> split in nested generics; ASI cases; recovery progress.

## Exit criteria

packs-typescript green; clippy clean.

## Outcome
- Documented subset: import (default + named) / export (incl. default), interface (props/methods/`?`), type aliases (unions, function types, tuples, object types), class (readonly/private/public/static fields + methods), enum (initializers), functions with optional/default/rest params, const/let/var with annotations, if/else/while/for..of/return/blocks, full Pratt expressions, template literals with nested `${}` interpolation, arrow functions (params with annotations + return annotations), array/object literals — and the two headline rules: **`>>` splits** (the scanner never merges `>`/`<`; the Pratt climb combines adjacent tokens into one shift node, Start-first so the operators stay inside it) and **ASI** (statement end at `;` / newline / `}`; `return` obeys the restricted production).
- Template mode stack rewritten around per-frame state (each frame tracks its own in-interpolation flag): nested templates return to the OUTER code mode after the inner backtick — a single boolean conflated the frames and broke `f\`a ${\`b ${c}\`} d\`` style sources (found by the nesting test).
- Arrow disambiguation: a bounded read-only lookahead past the matching `)` (skipping a return annotation) decides params+arrow vs grouped expression; the GROUP node stays open across the arrow (lossless nesting). Found en route: the `)` closes the PARAMS in JS — the body lives outside it.
- `/** */` doc comments are their own trivia kind (never tokens); `//` and `/* */` likewise; ERROR regions drop overlapping trivia so renders stay byte-exact.
- 10 tests: 36-fixture corpus lossless within budgets, per-family node kinds (import/export/interface/type/class/enum/function/var), param flavors (optional/default/rest), `>>` split token evidence + combined shift nodes (`>>`/`<<`/`>>>`), ASI (newline termination, restricted return, semicolons), template nesting (2 TEMPLATE_EXPR), doc-comment trivia, determinism + balance, recovery progress.
- clippy pedantic -D warnings clean; fmt clean.

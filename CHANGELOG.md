# Changelog

All notable changes to syntact are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
the [semver policy](docs/semver-policy.md).

## [Unreleased]

### Compiler loop — descent-lower (issue 043)

- **descent-lower**: the grammar → descriptor compiler — the keystone
  that closes the pipeline's compiler loop. The front half always
  *analyzed* grammars and the runtime always *interpreted* descriptors,
  but nothing connected the two (every pack parser was hand-written).
  A canonical IR `Grammar` now lowers deterministically into the
  `descent::Desc` shape: one dense kind space (node rules, node tokens,
  plain tokens, trivia, unmatched sentinel), transparent-rule inlining
  (acyclicity-checked), FIRST/FOLLOW-driven LL(1) dispatch guards, and
  pratt-hinted rules lowered to `PrattDesc` tables whose operator nodes
  ride the operator token's kind. A trailing `rep(recover)` document
  sweep keeps arbitrary input byte-faithful. Every canonical form v0
  cannot express (lookahead predicates, bounded repetitions, transparent
  cycles, LL(1) conflicts, malformed pratt shapes) is a named refusal —
  nothing mis-lowers silently. `Lowered::feed` bridges the lexgen
  scanner to the engine's clean-feed contract.
- **fix(gir-syntax)**: the derived pratt right binding power followed
  the wrong encoding (`lbp - 1` for left-associativity); it now derives
  `rbp = lbp + 1` per the descent engine's documented `InfixDesc`
  contract. Latent until now — no consumer executed hints end-to-end
  before descent-lower.
- **fix(importer)**: `prec.left` hints emitted `rbp = lbp` (the
  right-associative encoding); converted grammars now carry
  `rbp = lbp + 1` for left associativity.
- **bench**: `benches/lowered_json.rs` — the generated pipeline
  head-to-head against the hand-written reference (scan / parse_events /
  green_tree per fixture, plus the one-time compile step), and
  `tests/lowered_budgets.rs` dhat gates (lower ≤ 600 blocks, generated
  parse ≤ 5× reference on the steady fixture — the engine's per-entry
  descriptor clones are the known follow-up).
- **lints**: every `#[allow]`/`#![allow]` moved out of crate sources
  into justified `[workspace.lints.clippy]` overrides — the source tree
  is now allow-free (`grep -rnE '#!?\[allow\(' crates/` is empty).
### Editor tooling — pack-lsp (issue 042)

- **lsp**: the pack language server (roadmap §12.2 p3d "LSP for
  packs") — a std-only LSP 3.17 server over stdio for the ten shipped
  pack languages. Documents resolve their pack by `languageId` then uri
  extension; one pack parse per version builds the green tree plus a
  parent-indexed outline from which every view is prebuilt: error-region
  diagnostics (push + pull), a hierarchical document outline named by
  first-significant tokens, folding ranges, shape-classified semantic
  tokens (fixed four-entry legend, delta-encoded), kind+text document
  highlight, and selection ranges. The stdio transport loop is
  generalized behind a `Handler` trait both servers implement. Served
  by `syntact pack-lsp`.
- **cli**: `pack-lsp` subcommand (`--max-message` bound, default 8 MiB).
- **bench**: `benches/pack_lsp.rs` (open/didChange/query latencies for
  json+toml+python fixtures) and `tests/pack_lsp_budgets.rs` dhat gates
  (open ≤ 450 blocks/KiB incl. the pack parse, zero-alloc no-match
  highlight and prebuilt-view reads).
- **perf**: the outline emits span-boundary events in document order, so
  one monotonic UTF-16 sweep (no sort) positions every view; document
  open and didChange dropped ~24% on the JSON fixture.

### Editor tooling — gir-lsp (issue 041)

- **lsp**: the `.gir` grammar language server — std-only JSON-RPC 2.0
  over stdio with LSP `Content-Length` framing: completion of
  rules/tokens/markers, hover on annotations and declarations, inline
  conflict squiggles (surface errors, LL(1) conflicts downgraded on
  pratt rules, LR(1) shadow conflicts via production origins,
  unreachable rules), definition/references, document symbols, and
  rename with stable-ID preservation (declaration order — and the
  dense `RuleId`/`TokenId` numbering — never moves). Incremental
  `didChange` sync with UTF-16 position mapping (binary-searched line
  index). Served by `syntact lsp`.
- **cli**: `lsp` subcommand (`--max-message` bound, default 8 MiB).
- **bench**: `benches/lsp.rs` (open/reanalysis/query latencies) and
  `tests/lsp_budgets.rs` dhat gates (zero-alloc definition lookup,
  per-item completion, per-decl symbols, per-site rename, open
  ≤ 600 KiB/KiB incl. the external loader bound).

### Phase 5 — ecosystem 1.0 (issues 037–040)

- **importer**: tree-sitter `grammar.js` → gir KDL converter with a
  deterministic mapped/dropped/why report, regex-decided literal
  merging, separated-list sugar detection, externals as flagged
  placeholder tokens, and the kind-multiset conformance wedge over five
  fixture grammars (json, toml, ini, csv, markdown).
- **playground-server**: std-only HTTP parse service (`POST /parse`,
  `GET /packs`) with a pure router, bounded request sizes (413),
  graceful shutdown, and deterministic JSON serialization over all
  seven packs.
- **docs**: the syntact book (six chapters) plus a cookbook of five
  recipes that are compiled and executed by the test suite; chapter
  code blocks are pinned to the recipes byte-for-byte and SUMMARY and
  chapter links are checked.
- **api freeze**: semver policy, the `docs/api-snapshot.md` public-item
  freeze enforced by `api_snapshot`, changelog; three community packs —
  **INI**, **CSV**, **Markdown** — shipped at the flagship bar
  (per-family shapes, losslessness, determinism, recovery budgets).
- **fix(gir-syntax)**: seq markers now wrap their own item when
  lowering to the IR (a marked item used to be wrapped onto the wrong
  element, and a one-item sequence with a marker failed to lower).

### Phase 4 — performance maxima (issues 033–036)

- **packs-rust / packs-c / packs-sql**: byte-based reference parsers
  with quote disambiguation, declarator grammar, and case-insensitive
  keywords.
- **runtime-core**: persistent-red mode + parallel queries.

### Phase 3 — incremental & queries (issues 030–032)

- **incremental**: the reparse engine (margin-window fast path,
  context hashes, fallback discipline) with incremental-equivalence
  fuzzing.
- **query**: the query compiler + streaming matcher (kind bitsets,
  quantifiers, predicates, captures).
- **structural-index**: block scanner + SoA region index.

### Phase 2 — recovery & flagship packs (issues 027–029)

- **packs-python**: fused scanner+parser with INDENT/DEDENT and
  f-strings.
- **packs-typescript**: byte-based TS-subset parser with ASI.
- **cli**: `syntact trace` — per-region recovery events with JSON and
  text output and rescan parity against the full scan.

### Phase 1 — analysis & codegen (issues 010–026)

- **gir-analysis**: FIRST/FOLLOW, LR(1) shadow automaton, the
  determinism report, expression lowering and factoring.
- **lexgen**: the lexer compiler (mode stacks, expected-mask PHF,
  single-token relex).
- **codegen-rust**: the compact-table backend (row displacement, diff-
  stable emission).
- **recovery**: the uniform-cost repair planner with sync hints and
  cost models.
- **harness**: the conformance runner, differential driver, recovery
  calibration, and criterion budgets.
- **packs**: JSON and TOML reference packs at the flagship TDD bar;
  `syntact check/build/conflicts/conformance/trace` CLI.

### Phase 0 — foundations (issues 001–009)

- Workspace scaffold, toolchain, ADR discipline, and the runtime-core
  event stream, green/red trees, token sets, and diagnostics.

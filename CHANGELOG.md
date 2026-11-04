# Changelog

All notable changes to syntact are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
the [semver policy](docs/semver-policy.md).

## [Unreleased]

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

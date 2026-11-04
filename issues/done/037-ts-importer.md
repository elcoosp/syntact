# 037 — ts importer

- **Epic:** Phase 5 (epic 094)
- **Crates:** crates/importer
- **Status:** done

## Work

tree-sitter grammar.js subset → gir-syntax KDL converter: kinds, fields, precedence/associativity, externals as placeholders, regex-decided literals; conversion report (mapped/dropped/why); conformance wedge with kind-multiset diff.

## TDD requirements

5 fixture grammars (json, toml, ini, csv, markdown subset) with expected-report snapshots; report determinism.

## Exit criteria

importer green; clippy clean.

# 031 — query compiler

- **Epic:** Phase 3 (epic 092)
- **Crates:** crates/query
- **Status:** open

## Work

Query language (tree-sitter .scm shape): patterns, wildcards, quantifiers ? * +, captures @name, predicates (#eq?); compile to QueryStep flat arrays with kind-prefilter bitsets; streaming QueryCursor over the packed Output stream; predicate closures; capture spans.

## TDD requirements

Query parse/validate with spans; matching on hand-built streams; quantifier semantics; predicate invocation; capture dedup; highlight + symbol fixtures.

## Exit criteria

query green; clippy clean; O(1) alloc per candidate step documented + tested.

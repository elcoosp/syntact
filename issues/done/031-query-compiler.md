# 031 — query compiler

- **Epic:** Phase 3 (epic 092)
- **Crates:** crates/query
- **Status:** done

## Work

Query language (tree-sitter .scm shape): patterns, wildcards, quantifiers ? * +, captures @name, predicates (#eq?); compile to QueryStep flat arrays with kind-prefilter bitsets; streaming QueryCursor over the packed Output stream; predicate closures; capture spans.

## TDD requirements

Query parse/validate with spans; matching on hand-built streams; quantifier semantics; predicate invocation; capture dedup; highlight + symbol fixtures.

## Exit criteria

query green; clippy clean; O(1) alloc per candidate step documented + tested.

## Outcome
- Query language (documented subset): `(kind child… @cap quant)` patterns with arbitrary nesting, `_` wildcards, quantifiers `? * +` (min/max bounds in the step), `@name` captures (root captures after the closing paren; grouped `((a) @x (b) @y)` holds several root-level patterns), and `#eq? @cap "value"` predicates. Every parse error carries the offending byte span.
- Compilation: patterns flatten to pre-order `QueryStep` arrays — `{kind, min, max, capture, prefilter}` — where the **prefilter bitset** ([u64; 3], 192 kinds) carries the step's own kind bit plus all descendant bits (wildcards set all bits): a candidate subtree whose kind bit is absent rejects in one AND + test (`bitset_allows`); kind names resolve through an injected `KindResolver` (unknown kinds are compile errors with spans).
- Streaming `QueryCursor`: ONE pass over the packed Output steps builds a compact node table (kind + span from the range table + children — no tree allocation beyond the table); matching runs quantifier-backtracking against that table with capture spans (first token start → last token end); `#eq?` predicates filter on the captured text; per-match capture dedup.
- **O(1) alloc per candidate step**: the cursor's scratch is allocated once and reused across candidates — `scratch_capacity()` never grows across 1000 candidates x 11 re-runs (pinned by test).
- 13 tests: parse shapes + spans, 4 parse-error spans, compile kind validation, hand-built stream matching, wildcard, quantifier semantics (? / * / + on empty and present children) + step bounds, predicate pass/filter, capture dedup, highlight fixture (3 families), symbol fixture (3 pairs), scratch-capacity evidence, prefilter bitset unit.
- clippy pedantic -D warnings clean; fmt clean.

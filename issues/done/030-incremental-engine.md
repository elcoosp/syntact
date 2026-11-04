# 030 — incremental engine

- **Epic:** Phase 3 (epic 092)
- **Crates:** crates/incremental
- **Status:** done

## Work

Edit normalization; single-token relex fast path; balanced-block reparse; margin-based fragment reuse (ReuseCursor + safety predicates, context hashes); ReusedSubtree splicing; changed-ranges diff; incremental-equivalence fuzz reparse(edit(x)) == fresh_parse(x).

## TDD requirements

Fast-path trigger stats; reuse predicate rejections; changed ranges vs brute force; seeded fuzz equivalence on json+toml packs.

## Exit criteria

incremental green incl. fuzz suite; clippy clean.

# 039 — docs book cookbook

- **Epic:** Phase 5 (epic 094)
- **Crates:** docs/
- **Status:** done

## Work

docs/book (SUMMARY.md + chapters: getting started, event protocol, packs walkthrough, incremental, queries, recovery) + docs/cookbook (recipe-per-file runnable snippets).

## TDD requirements

Every snippet exercised by a doc-test or integration test; SUMMARY link check.

## Exit criteria

Book + cookbook complete; cargo test --doc green; link check clean.

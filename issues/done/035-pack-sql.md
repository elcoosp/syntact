# 035 — pack sql

- **Epic:** Phase 4 (epic 093)
- **Crates:** packs/sql
- **Status:** done

## Work

Reference SQL (SQLite dialect subset): SELECT w/ joins/subqueries/CTEs, INSERT/UPDATE/DELETE, CREATE TABLE w/ constraints, expressions, CASE, literals, -- and /* */ trivia, case-insensitive keywords.

## TDD requirements

Per-statement-family tests; case-insensitivity; nested subquery depth; losslessness.

## Exit criteria

packs-sql green; clippy clean.

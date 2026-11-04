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

## Outcome
- `Engine<A: Adapter>` over a token-level pack hook (scan + optional relex_one — lexgen's `Scanner::relex_one` is the JSON fast path; TOML has none, documented). Per edit, the cheapest sufficient strategy:
  1. **Fast path**: the edit fully absorbed inside ONE token — same kind, same start, end shifted by exactly the length delta — plus a one-token MARGIN check (the next token must relex identically, or there must be nothing after it; an edit that changes the tail's lexing falls through). Found by the fuzz gate: without the margin, `""` + boundary edits slipped through with stale suffix tokens.
  2. **Balanced-block fragment reuse**: the enclosing bracket-balanced block is the reuse boundary; tokens outside carry over (range-shifted) after the safety predicates — bytes outside the block byte-identical old<->new and boundary tokens' context hashes (kind + text + neighbor kinds) match. Two bugs found by the fuzz: the whole-doc block's byte span must extend to the DOCUMENT end (not the last token's end), and an empty prefix must give the rescan the document head.
  3. **Rejected reuse**: full rescan, counted.
- `ReusedSubtree` splicing: carried fragments reported as old/new byte-range pairs; the token list itself is the splice. `changed_ranges` diffs old vs new token lists, with `changed_ranges_brute_force` as the differential reference (asserted equal on a corpus of edits).
- **Fuzz equivalence gate** (60 seeded rounds x 4 JSON docs + 3 TOML docs): `incremental_tokens(edit(x)) == fresh_scan(x)` every round, plus fresh-tree byte-faithfulness on TOML. Found en route: a TOML parser spin on trailing junk after a valid key-value (`b = "x",` — nothing consumed the comma) — fixed with a trailing-junk error region AND a global document-loop progress guard (an iteration that consumed no bytes force-feeds one byte into an error region).
- 8 tests: fast-path trigger + kind-change rejection, reuse rejection, changed ranges vs brute force, context-hash discrimination, TOML reuse + losslessness, both fuzz gates.
- clippy pedantic -D warnings clean; fmt clean.

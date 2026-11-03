# 022 — lexgen mode stack

- **Epic:** Phase 1 (epic 090)
- **Crates:** crates/lexgen
- **Status:** done

## Work

Lexer modes with mode stack (push/pop/switch as grammar data), per-mode DFA sub-tables; expected-token masks as PHF packed u64 runs with linear reference; relex_one(input, offset) single-token entry point.

## TDD requirements

Mode stack nesting; mask lookups vs linear reference; relex parity with full-scan prefixes.

## Exit criteria

lexgen green incl. mode/relex suites; clippy clean.

## Outcome
- `ModeLexer` (`modes.rs`): per-mode DFA sub-tables compiled from data-driven `ModeSpec`s; each `ModeToken` carries a `ModeAction` (`Stay`/`Push`/`Pop`/`Switch`) applied after the token matches. Stack discipline: base mode is sticky (`Pop` at depth 1 is a no-op), `Switch` replaces the top, `Push` nests (the nested mode can re-enter itself). DFA accepts carry the token's LOCAL index; a `kinds` table maps local → global kind, so per-mode token tables are proper subsets of the grammar table. Unmatched chars stay one-char error lexemes (progress guarantee holds per mode).
- `TokenSet` (`token_set.rs`): packed 192-bit bitmask over token kinds (3×u64), `Copy`-sized, out-of-capacity inserts are no-ops.
- `ExpectedMasks` (`masks.rs`): `(state → expected TokenSet)` table via a deterministic displacement PHF — splitmix64 hashing, buckets placed largest-first, smallest displacement search, seed/table growth on failure, and exact-key verification on every lookup (no false positives). `lookup_linear` is the linear-scan reference; the differential suite holds `lookup == lookup_linear` for all 500 keys and absent keys, plus duplicate folding (last wins) and adversarial odd-multiple key skew.
- `Scanner::relex_one(source, offset)` (`scan.rs`): single-token incremental entry point — longest match from `offset` (zero-length accepts excluded), one-char error lexeme on no-match, `None` out of bounds. Parity suite: relex at every token-start offset equals the full-scan lexeme; immune to trailing edits (different tail ⇒ identical head token); multibyte error lexemes cover whole chars.
- 49 tests green (23 lib + 11 masks + 7 modes + 4 relex + 4 lexer) + doctests; `cargo clippy --workspace --all-targets -D warnings` clean; `cargo fmt --check` clean.

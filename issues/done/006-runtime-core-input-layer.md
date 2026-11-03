# 006 — runtime core input layer

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** done

## Work

Input layer: Source (unchecked UTF-8 carrier), line index, byte-based line/col mapping, u32 length guard, simdutf8 fast path hooks.

## TDD requirements

Line mapping, CRLF, multibyte columns, length limit tests.

## Exit criteria

runtime-core input green; clippy clean.

## Outcome
- input.rs: Source unchecked-UTF-8 carrier, line index (byte-based line/col mapping), u32 length guard. 6 unit tests green; clippy pedantic clean (workspace gate).

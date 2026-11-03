# 007 — runtime core event stream

- **Epic:** Phase 0
- **Crates:** crates/runtime-core
- **Status:** open

## Work

8-byte event protocol: Start{kind, forward_parent}, Token{kind, n_raw}, Error{diag}, Finish; packed Output + Step unpacking; process() validation (balance, forward-parent nesting, diag bounds).

## TDD requirements

Pack/unpack round-trips; malformed streams rejected; fuzz property tests.

## Exit criteria

events green; clippy clean.

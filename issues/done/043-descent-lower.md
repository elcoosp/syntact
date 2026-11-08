# 043 — descent-lower: the grammar → descriptor compiler

- **Epic:** compiler loop (post-1.0)
- **Crates:** descent-lower (new), gir-syntax, importer, bench, gir-syntax tests
- **Status:** done

## Work

The pipeline's compiler loop was open: the front half (`gir-syntax` →
`gir-ir` → `lexgen` / `lr-shadow` / `gir-analysis`) analyzed grammars,
the runtime (`descent`) interpreted descriptors, and nothing connected
the two — every pack parser was hand-written, `descent::Desc` had no
producer, and `desc.rs`'s "the interpreter and the generated parser must
agree by construction" was an aspiration, not a mechanism.

`descent-lower` is the keystone: a deterministic lowering from the
canonical typed IR into the descriptor shape the engine (and the codegen
table backend) consumes.

- **Kind space** — one dense `u16` map: `node="true"` rules in
  declaration order, then `node="true"` tokens, plain tokens, trivia;
  the unmatched-character sentinel (`bad_kind`) lands last. Pratt
  operator nodes reuse the operator token's kind (tree-sitter's
  anonymous-node convention).
- **Transparent rules** — `node="false"` rules inline at their call
  sites; the transparent subgraph must be acyclic
  (`LowerError::TransparentCycle` names the entry rule).
- **LL(1) dispatch** — `Alt` branches dispatch through
  `Peek`/`PeekSet` FIRST guards; overlaps are `AltConflict`, nullable
  branches must be last. Rule-headed optional/repetition bodies get
  FIRST guards so valid input never speculatively enters a
  self-recovering rule (the `{}` case).
- **Sync sets** — declared `sync` tokens, else the rule's FOLLOW set.
- **Pratt rules** — a hinted rule lowers to one `PrattDesc` table:
  leading-self branches are infix entries, bare-operator tails postfix,
  `op ~ self` atom branches prefix unaries, `open ~ self ~ close` the
  transparent group; the rule's node wraps the climb; unaries bind
  tighter than every infix level.
- **Document sweep** — the start pattern is followed by
  `rep(recover)`: every token the grammar cannot place still lands in
  an ERROR node, which is what makes the generated parser
  byte-faithful on arbitrary input.
- **Feed bridge** — `Lowered::feed` remaps the lexgen scanner's
  token ids into the generated kind space, sends unmatched characters
  to `bad_kind`, and drops trivia (ADR-008 gap re-derivation).

Every canonical form v0 cannot express is a named, tested refusal:
lookahead predicates (`and`/`not`), bounded repetitions above one,
transparent pratt rules, node-rule pratt branches, malformed pratt
tails/atoms, LL(1)-conflicting alternatives, transparent cycles.

## Bugs found and fixed on the way

- **gir-syntax**: the derived pratt right binding power for left
  associativity was `lbp - 1`, contradicting the descent engine's
  documented `InfixDesc` encoding (`rbp = lbp + 1` for left-assoc).
  Latent since issue 013/023 — no consumer executed hints end-to-end
  until now. Fixed to `lbp.saturating_add(1)`; explicit `rbp=` on the
  surface still overrides; the pinned surface test updated.
- **importer**: `prec.left` emitted `rbp = lbp` (the right-assoc
  encoding); now emits the engine encoding. Conformance fixture updated.
## TDD evidence

Failing tests first (`unresolved import descent_lower::lower`), then the
implementation:

- `crates/descent-lower/tests/json_parity.rs` — the reference pack's
  language on the KDL surface: dense kind-numbering pin; tree-shape +
  token-range parity against `packs_json` on a 13-entry valid corpus
  (name-resolved, structural); byte-faithfulness (`render(parse(x)) ==
  x`) on the same corpus; malformed corpus stays lossless, balanced,
  and error-bearing; garbage consumes exactly one error per feed token.
- `crates/descent-lower/tests/pratt_lower.rs` — golden climb shapes:
  left-associative chains nest left, single-precedence chains group
  strictly left-to-right, parenthesized groups stay transparent (bare
  paren tokens only), prefix unaries wrap their operand (never the
  infix tail), and the table lands in the descriptor with the engine's
  binding-power encoding (`rbp = lbp + 1`).
- `crates/descent-lower/tests/errors.rs` — every refusal variant with
  its message, plus `rep1` lowering semantics (`body ~ rep(body)`).
- `crates/bench/tests/lowered_budgets.rs` — dhat gates: `lower`
  ≤ 600 blocks (grammar-proportional, not document-proportional);
  generated parse ≤ 5× the reference on the ~32 KiB steady fixture
  (measured 4.5× — the engine's per-entry `RuleDesc` body/sync clones
  are the known follow-up).
- `crates/bench/benches/lowered_json.rs` — criterion head-to-head
  (scan / parse_events / green_tree per fixture vs the reference, plus
  the one-time compile step at ~0.4 ms).
- API snapshot regenerated (additive: the `descent-lower` section).
- Workspace gates green: `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (pedantic via workspace lints) with **zero** `#[allow]` attributes in
  `crates/` — the 21 pre-existing inline allows moved into justified
  `[workspace.lints.clippy]` overrides; `cargo test --workspace` 102
  suites green.

## Exit criteria

A `.gir` grammar compiles to a working, lossless, byte-faithful parser
through library calls alone (load → lower → scan → engine → tree),
parity-tested against the hand-written oracle; the compiler loop is
closed at the data-model level, ready for `codegen-rust` emission
(the "compiling crate" step) as the follow-up.

# Generating parser crates

`syntact build <grammar.gir> -o <dir>` closes the compiler loop the
roadmap promises (§0, §6.1 pass 12, §12.3 P0 "compiling crate" row): a
`.gir` grammar goes in, a standalone, compiling, safe-Rust parser crate
comes out. The generated parser is not a table dump — it is recursive
descent + Pratt code that reads like the hand-written reference, because
generation chases the ceiling, not the floor (the OXC lesson, §2).

## What the backend emits

One file per concern, matching the packs-json API shape (issue 019):

- `Cargo.toml` — `forbid(unsafe_code)`, `missing_docs`, `clippy::pedantic`
  clean; depends on `runtime-core` (events/trees) and `lexgen` (scanner).
  The emitted crate reuses the substrates — it never forks a second lexer
  engine or a second tree builder.
- `src/lib.rs` — `parse` / `parse_events` / `green_tree` (the packs-json
  shape), the root-kind constant, the diagnostics side table.
- `src/kinds.rs` — one `pub const` per generated kind (declaration order).
- `src/scanner.rs` — the `lexgen` DFA serialized as Rust literals,
  rebuilt once via `OnceLock`; `scan` reuses `lexgen::Scanner::scan`
  exactly. The `Lowered::feed` kind-remap/trivia bridge is reproduced in
  emitted code.
- `src/parser.rs` — one `fn rule_<id>` per node rule, `Pat → Rust`
  control flow mirroring `descent::Engine::pat` line for line:
  `Tok`→`bump`, `Seq`→straight-line, `Alt`→FIRST-guarded dispatch,
  `Opt`/`Rep`→loops with the forward-progress guarantee, `Node`→
  start/finish events, `Rule`→call, `Expr`→generated Pratt climb,
  `Peek`/`PeekSet`→lookahead guards. Sync-set recovery glue and ERROR
  nodes are byte-identical in behavior to `descent::engine`.

## Determinism (ADR-013)

Emission is byte-identical under regeneration: no timestamps, fixed
wrapping, sorted maps, `#[rustfmt::skip]` on dense arrays. Two runs over
the same `Lowered` produce the same bytes — the determinism gate asserts
it (emit twice, byte-equal).

## Parity

The emitted parser reproduces `descent::engine` output byte-for-byte on
every input — same `Run` (events, token ranges, trivia, error count). The
three-way parity gate (packs-json reference vs descent engine vs
generated) runs on the valid corpus (13 entries) and the malformed
corpus: tree shape, token ranges, byte-faithfulness, and lossless
ERROR-bearing recovery all match.

## Performance

The generated parser eliminates the engine's per-entry `RuleDesc`
body/sync clones (one `fn` per node rule, no interpreter dispatch), so it
is faster than the engine it replaces while staying within a constant
factor of the hand-written reference. On the ~32 KiB steady JSON
fixture (criterion, release):

| path | time | allocations |
| --- | --- | --- |
| generated `parse_events` | 1.00 ms | 91 blocks |
| engine `parse_events` (post-M2) | 1.05 ms | 54 blocks |
| generated `parse` (tree) | 2.03 ms | 14 665 blocks |
| reference `parse` (tree) | 1.16 ms | 14 625 blocks |

The generated parser's `parse_events` is at parity with the engine's
speed (M2 de-cloned the engine's `RuleDesc` borrows, so the interpreter
dispatch is no longer the bottleneck) and allocates ~1.003× the
reference's blocks (≤2× gate met). The engine's 4.5× reference factor
(issue 043) is closed by both the generated path and the M2 de-clone.

## Running it

```sh
# Emit the crate from a grammar:
syntact build json.gir -o ./out/json

# Or use the workspace member that regenerates on every build:
cargo run -p bench --example generated_json
```

The `generated-json` workspace crate is the emitted output, made a
workspace member so benches and the dhat budget gate can measure the
generated parser head-to-head against the engine and the reference. Its
`build.rs` runs the codegen-rust descent backend; every `cargo build`
regenerates the source.

## Cookbook recipe

The generated parser, driven the way a real consumer would (the recipe is
compiled and executed by `crates/cli/tests/docs_sync.rs`):

```rust
// Recipe: parse JSON with the generated parser crate (issue 045).
//
// The codegen-rust descent backend emits a standalone, compiling Rust
// parser crate from a `.gir` grammar. This recipe drives the emitted
// crate (the `generated-json` workspace member, regenerated on every
// build) the way a real consumer would: parse a document, count the
// errors recovery absorbed, and verify byte-faithfulness.
//
// The emitted parser reproduces `descent::engine` output byte-for-byte
// (the three-way parity gate asserts it) while reusing `runtime-core`
// (events/trees) and `lexgen` (scanner) — no second lexer engine, no
// forked runtime. `syntact build json.gir -o <dir>` emits this same
// source for any grammar that lowers cleanly.

use runtime_core::input::Source;

#[test]
fn generated_parser_is_byte_faithful() {
    let source = r#"{"name": "syntact", "version": 1, "ok": true}"#;
    let (events, tokens, errors) = generated_json::parse_events(source);
    assert_eq!(errors, 0, "valid doc has no errors");
    assert!(!events.is_empty(), "events emitted");
    assert!(!tokens.is_empty(), "tokens emitted");

    let tree = generated_json::parse(source).expect("tree integration never fails structurally");
    let input = Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source, "render(parse(x)) == x");
}

#[test]
fn generated_parser_recovery_is_lossless() {
    // A stray `}` mid-array: recovery absorbs it and keeps the tree
    // byte-faithful (every unplaceable token lands in an ERROR node).
    let source = "{ \"a\": [1, 2}, \"b\": 0 }";
    let (_, _, errors) = generated_json::parse_events(source);
    assert!(errors > 0, "recovery reported the error");
    let tree = generated_json::parse(source).expect("recovery keeps the tree structurally valid");
    let input = Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source, "recovery is lossless");
}
```

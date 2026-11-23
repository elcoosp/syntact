# 045 — codegen-rust descent emission: `.gir` → compiling Rust parser crate

- **Epic:** compiler loop (post-1.0)
- **Crates:** codegen-rust (descent backend), cli, generated-json (new),
  bench, lexgen, docs/book, docs/cookbook
- **Status:** done

## The gap this closed

The product promise (roadmap §0, §12.3 P0 "compiling crate" row, §6.1
pass 12: ".gir in → a compiling Rust parser crate out") was unbuilt.
`codegen-rust` emitted only static tables; `syntact build` printed stats;
every pack was hand-written; the CLI's `codegen-rust` dependency was
dead; `descent_lower::Lowered` had no production consumer (only tests
and benches). The last milestone before this one spent 13/25 commits on
lint-allow removal, badges, and snapshot regen — completion theater, not
progress on the plan's core.

This issue ships the keystone: a descent backend in `codegen-rust` that
takes a `Lowered` program + a compiled `lexgen::Scanner` and emits a
**standalone, compiling, safe-Rust parser crate** reproducing
`descent::engine` output byte-for-byte, plus the CLI wiring, the perf
gate, the example, and the docs.

## What it does

- **`codegen-rust::descent::emit_crate`** — emits `Cargo.toml` +
  `src/{lib,kinds,scanner,parser}.rs`. One `fn pat_<n>` per pattern node
  (interned, no duplicates) + `fn rule_<id>` per node rule + `fn
  pratt_<id>` per Pratt table. `Pat → Rust` control flow mirrors
  `descent::Engine::pat` line for line: `Tok`→`bump`, `Seq`→straight-line,
  `Alt`→FIRST-guarded dispatch, `Opt`/`Rep`→loops with the forward-progress
  guarantee, `Node`→start/finish events, `Rule`→call, `Expr`→generated
  Pratt climb, `Peek`/`PeekSet`→lookahead guards. Sync-set recovery glue
  + ERROR nodes byte-identical in behavior to the engine. The
  `Lowered::feed` kind-remap/trivia bridge is reproduced in emitted code.
  The DFA is serialized as Rust literals and rebuilt once via `OnceLock`
  — `scan` reuses `lexgen::Scanner::scan` exactly (no second lexer engine).
- **CLI wiring** — `syntact build <g.gir> -o <DIR>` emits the crate;
  lowering refusals surface as named FAIL diagnostics (never silently
  skipped). No `-o` keeps today's stats output.
- **`generated-json` workspace crate** — the emitted output, made a
  workspace member so benches and the dhat budget gate can measure it
  head-to-head against the engine and the reference. `build.rs` runs the
  emitter on every `cargo build`.

## Exit criteria (real, measured)

- **Compile gate**: the integration test
  `crates/codegen-rust/tests/emitted_crate_compiles.rs` emits the crate
  into `target/codegen/json/` and runs `cargo test` on it — the emitted
  crate compiles (`forbid(unsafe_code)`, `missing_docs`,
  `clippy::pedantic`-clean posture) and its tests pass.
- **Three-way parity**: the emitted crate's tests assert `parse_events`
  matches `descent::engine` on the 13-entry valid corpus + 7-entry
  malformed corpus — events, token ranges, error count all equal;
  byte-faithfulness (`render(parse(x)) == x`) on the valid corpus.
- **Determinism**: emit twice, byte-equal (asserted in the integration
  test). ADR-013: no timestamps, fixed wrapping, sorted maps.
- **Perf gate** (criterion, release, ~59 KiB steady fixture — the
  `bench::steady_state_document()` ≥ 32 KiB):
  - **M1 (pre-M2)**: generated `parse_events` 995 µs vs engine 2.44 ms →
    **2.45× faster** (≥2× gate met). The engine's per-entry `RuleDesc`
    body/sync clones were the bottleneck.
  - **M2 (post-de-clone)**: the engine's `rule` fn now borrows `body`/
    `sync` from the `&'d Desc` instead of cloning; engine `parse_events`
    dropped to 1.05 ms (allocations 50 553 → 54 blocks). Generated
    `parse_events` 1.00 ms vs engine 1.05 ms → parity (the interpreter
    dispatch is no longer the bottleneck).
  - generated `parse` (tree) 14 665 blocks vs reference 14 625 blocks →
    **1.003× reference allocations** (≤2× gate met, both pre- and
    post-M2).
- **Zero `#[allow]`** in the emitted code (the unit test
  `emitted_parser_has_no_allow_attributes` asserts it).
- **Consumer example** (`crates/bench/examples/generated_json.rs`) +
  **book chapter** (`docs/book/codegen.md`) + **cookbook recipe**
  (`docs/cookbook/recipes/generated_parser.rs`, compiled in
  `crates/cli/tests/docs_sync.rs`).

## What it refuses (deterministic, named)

`EmitError::UnsupportedPattern` — every pattern shape v0 cannot lower to
Rust is a named refusal (the backend never silently skips). The JSON
fixture lowers cleanly, so no refusal fires on the gate corpus.

## Remaining gaps (named, not done)

- **Tree-sitter oracle** — the parity gate compares generated vs engine
  vs the hand-written reference; a tree-sitter cross-check is a future
  hardening (the reference is itself the differential oracle today).
- **`parse_state` reuse** — the emitted parser does not yet stamp
  `parse_state` for incremental reuse (roadmap §7.7); the generated code
  is full-parse only.
- **PGO** — the emitted crate is not PGO-instrumented (roadmap §6.1 pass
  13); the perf numbers above are unprofiled release builds.

## M2 — engine de-clone (done)

M1 was green end-to-end (parity + compile + determinism + perf + example
+ docs), so M2 proceeded: killed the engine's per-entry `RuleDesc`
body/sync clones (`descent::Engine::rule` now borrows `body`/`sync` from
the `&'d Desc` instead of cloning; `Pat::Expr` passes the `&PrattDesc`
directly instead of cloning the table). Re-measured: engine allocations
dropped from 50 553 → 54 blocks on `parse_events` (and 65 164 → 14 665 on
the tree — parity with the reference). `GENERATED_VS_REFERENCE_FACTOR`
tightened 5 → 2 (the clone-tax headroom is gone; the gate now reflects
the realistic allocation shape). The generated parser stays at parity
with the engine on speed post-M2 (the interpreter dispatch is no longer
the bottleneck).

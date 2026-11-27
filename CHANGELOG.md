# Changelog

All notable changes to syntact are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
the [semver policy](docs/semver-policy.md).

## [Unreleased]

### Token-class inference — §6.1 pass 7

- **lr-shadow**: [`token_class`] — the pipeline's pass 7 ("group tokens
  never distinguished in any parser state", the tree-sitter
  `ts_symbol_map` idea, extended). [`TokenClasses::compute`] reads each
  token's action column over the resolved LR(1) table (states in
  discovery order, `Shift`/`Reduce`/`Accept` keyed for equality) and
  groups identical columns: declared-but-unreferenced tokens (reserved
  keywords, lexer-only spellings) collapse into one
  never-distinguished class, used tokens keep their column, and the
  pseudo-EOF sentinel never enters a class. The extension beyond the
  upstream map: the grouping is *checked* — same-class tokens are
  machine-indistinguishable, so mapping a feed to class representatives
  must preserve the table-driven driver's verdict, pinned end-to-end
  against [`driver::parse`] (`tests/token_class.rs`: dead-token
  grouping, distinguished singletons, representative/`class_of`
  resolution, class ordering, EOF exclusion, driver substitution).
  Classes order by representative, members keep declaration order, the
  computation is deterministic for a given `(automaton, grammar)` pair.
- **cli**: [`syntact build`]'s front-half summary prints the pass-7
  line — `token classes: N tokens -> M classes (K never
  distinguished)` — so dead token-table weight is visible at build
  time. Book chapter `docs/book/token-classes.md` with the
  `token_class` recipe compiled by the docs gate; criterion suite
  `bench/benches/token_class.rs`; consumer example
  `crates/lr-shadow/examples/token_class.rs`
  (`cargo run -p lr-shadow --example token_class`); API snapshot
  regenerated.

### Rope input — §7.1's editor integration seam

- **incremental**: [`rope`] — §7.1's "ropey::Rope accepted as an
  alternative input for editor integrations — the incremental engine
  consumes rope deltas as edit descriptors (§7.7)". [`edit_between`]
  derives the engine's [`Edit`] descriptor from a rope state pair (an
  editor's transition): common character prefix and suffix, the middle
  span of the new rope as the replacement text — char-aligned by
  construction, so the text is always a valid `&str`; equal ropes
  decline with `None` (nothing to classify). [`apply_edit`] closes the
  §8 "ropey-compatible" loop the other way, applying a descriptor back
  onto a rope (byte offsets resolved to ropey's character indices once
  per boundary). The cover is conservative, never wrong: a multi-hunk
  rope change yields one wide replacement span whose splice still
  reconstructs the new text exactly, so the engine's fast-path
  predicates and fallback stay correct — the engine-on-descriptor
  stream is equivalence-gated against a fresh full lex and against the
  hand-built string edit (`tests/rope.rs`: insertion, deletion,
  replacement, multibyte, empty→document and back, round trip, engine
  equality, the multi-hunk cover). [`Rope`] is re-exported so consumers
  pin one ropey version through this crate (ADR-012's thin-wrapper
  posture). The incremental book chapter gains the "Rope input" section
  with the `rope_edit` recipe compiled by the docs gate; criterion
  suite `bench/benches/rope_edit.rs` (descriptor derivation + engine
  run vs the string edit); consumer example
  `crates/incremental/examples/rope_edit.rs`
  (`cargo run -p incremental --example rope_edit`); API snapshot
  regenerated.

### The balanced-block fast path — §7.7's second reparse accelerator

- **incremental**: [`Engine::incremental_edit_with_brackets`] — the
  §7.7 fast path 2 ("small balanced-brace block → re-parse block … fall
  through to the general engine on mismatch") at the token-stream
  engine's level: multi-token damage contained in a bracket-balanced
  block relexes ONLY the smallest enclosing block, with the margin
  window's predicate discipline carried over (exact tiling, error-free
  fragment, boundary kind AND text agreement, pair-table balance with
  interleaved pairs rejected). Every rejection falls through to the
  full relex — the method is a pure accelerator and [`Engine::
  incremental_edit`] is now its `&[]` form, so the public surface only
  grows. Resolves the issue 030 drift honestly: its Work section
  claimed the balanced-block reparse, but `incremental/src` only ever
  shipped the margin-window path (grep-evidenced); the plan line now
  has code, tests, and a bench. Measured (`bench/benches/
  incremental.rs`, run-fast): the block splice saves ~80× over the
  fallback on a 256-row document (33 µs vs 2.7 ms per edit).
  Seeded block-edit fuzz (200 deterministic multi-token edits) keeps
  the issue 030 equivalence gate: `reparse(edit(x)) == fresh_parse(x)`
  on every path.

### The cross-backend oracle — `syntact test-all`

- **lr-shadow**: [`driver`] — the table-driven LR(1) runtime (roadmap
  §7.4's "table-driven LR (O1 tier)" and §7.10's O1 row). Until this
  seam the shadow automaton was analysis-only: `codegen-rust`'s table
  emission (issue 015) had no runtime consumer, and §7.4's "cross-backend
  oracle" had no table backend to compare against. [`parse`] is a
  deterministic, total stack machine over the automaton's
  `action`/`goto` tables — shift/reduce/accept with the §7.4
  expected-set extraction on every rejection (state, offending feed
  index, sorted expected terminals), and a deterministic step budget
  (the §7.5 work-budget posture) that turns the ε-reduce/goto cycles a
  hand-built ε-cyclic table can contain into a rejection instead of a
  hang. Unit-tested over the kernel-worklist automaton: accept/reject,
  nullable-start ε derivations, reduce/goto loops, expected-set
  contents, determinism, the budget.
- **cli**: [`syntact test-all <GRAMMAR.gir> <FILES…>`]
  (crates/cli/src/testall.rs) — §7.4's differential oracle and §11.1
  #4's self-differential as a registry-free library seam plus command:
  both backends consume the SAME scanner feed (`Lowered::feed`), the
  descent verdict (a clean derivation, with the nullable-start clause
  the engine's silent zero-progress close requires) must equal the
  table verdict (the accept action) on every corpus document. A
  rejected run reports the table step error by token name; the whole
  evaluation runs twice (fresh loader → automaton → feeds) and the
  report refuses to claim determinism unless the second pass reproduces
  the first (ADR-013 byte-stable JSON via `--json`). Exit codes follow
  the command convention: 0 agreement, 1 disagreement (one backend
  mis-classifies the language), 2 usage/IO. Book chapter
  `docs/book/test-all.md` with the `test_all` recipe compiled by the
  docs gate; criterion suite `bench/benches/testall.rs` (descent vs
  table per fixture document); consumer example
  `crates/cli/examples/testall.rs`
  (`cargo run -p cli --example testall`); API snapshot regenerated.

### PGO superinstruction mining — the interpreter's fused token shifts

- **descent**: [`superinstr`] + [`parse_events_with`] — the O3 tier's
  "superinstruction fusion" for the descriptor interpreter (roadmap
  §7.10, §10.2, §12.2 "superinstruction mining", the data-driven
  synthesis of §1.3 #16). [`mine`] profiles the §5.2 `profile/` corpus
  idea into a [`SuperSet`] of hot adjacent token-shift pairs — only
  pairs observed ≥ `min_count` times enter the table ("only from PGO
  hot paths"), ordered count-descending with kind-ordered tie-breaks
  and capped (the mined table is a diff-stable, reviewable artifact);
  [`parse_events_with`] runs the engine with the table so every maximal
  `Pat::Tok` run whose adjacent pairs are hot executes as ONE fused
  dispatch, checks-and-consumes in a single feed lookup per token, and
  keeps the `Seq` committed-prefix discipline on mid-run mismatch. The
  fused output is conformance-gated to equal [`parse_events`]'s run
  byte for byte on valid AND malformed input (`crates/descent/tests/
  superinstr.rs`, the §10.2 "conformance-gated" criterion; the lowered
  JSON pipeline is in the gate). Measured honestly
  (`bench/benches/superinstr.rs`): on the JSON fixture the fused path
  lands ~5–7% behind the generic interpreter — below the §10.2 ≥10%
  ship gate, so the table stays an opt-in per-grammar,
  decision-by-measurement artifact (the ADR-010 pattern) and the
  default [`parse_events`] path is untouched; deeper fusion sites (the
  `Alt` dispatch, the Pratt atom) are the follow-up. Mining cost is
  bench-per-corpus (`superinstr/mine/*`). Consumer example
  `crates/descent/examples/superinstr.rs`
  (`cargo run -p descent --example superinstr`).

### The parallel query drive — issue 036's execution half

- **runtime-core**: [`parallel_query`] now scans its chunks on scoped
  worker threads instead of only planning for them, and
  [`parallel_query_with`] is the general form taking any scan predicate
  (the kind-set query shape) — issue 036's "parallel query mode over
  the SoA chunk plan with deterministic ordered merge", the same
  `std::thread::scope` drive the structural index's region-parallel
  validation runs on: workers own contiguous ascending chunk ranges,
  per-worker hit lists join in worker order, and the final
  ascending-start sort keeps the merge contract explicit — the merged
  output IS the sequential scan byte-for-byte at every fan-out
  (`0` = `available_parallelism`, any count capped at the chunk
  count), pinned by parity/determinism/exactly-once-scan tests over a
  50k-token seeded corpus (`crates/runtime-core/tests/
  parallel_query.rs`). Measured (`bench/benches/persistent_query.rs`):
  on the 50k-token corpus the scoped drive's spawn overhead dominates
  (sequential ~79µs vs w2 ~143µs in the bench build) — the contract
  delivered here is the parallel mode + deterministic merge; a
  persistent worker pool is the follow-up. Consumer example updated
  (`crates/runtime-core/examples/persistent_query.rs`).

### The PGO pipeline emission — the compiler pipeline's 13th pass

- **cli**: [`syntact pgo`](crates/cli/src/pgo.rs) — the pass-13
  obligation ("PGO wiring: instrument profile corpus, emit cargo pgo
  scripts, merge profiles", roadmap §6.1, ADR-014) as a
  registry-routed command: name a registered pack and its profile
  corpus (the §5.2 `profile/` idea) and get the complete
  instrument → train → merge → use → bench pipeline as two
  deterministic artifacts (`manifest.kdl`, `pgo.sh`). Training reuses
  the engine end-to-end (the instrumented `syntact` binary drives its
  `conformance` command over the corpus through the named pack), the
  bench phase exists only where `crates/bench` registers a suite for
  the pack, emission is byte-stable for identical inputs (ADR-013),
  and every invalid input is a named refusal — unknown packs (with the
  registered list), empty corpora, paths the emitted script could not
  carry. Library seam `cli::pgo` (`plan` pure, `emit` filesystem,
  `render_json` for `--json`); consumer example
  `crates/cli/examples/pgo_pipeline.rs`; book chapter
  `docs/book/pgo.md` with the `pgo_pipeline` recipe compiled by the
  docs gate; API snapshot regenerated.

### `syntact import` — the tree-sitter adoption wedge on the CLI

- **cli**: `syntact import <GRAMMAR.js> [-o <FILE>]` (roadmap §9.1
  command list, ADR-015) wires the importer library (issue 037) into
  the command line: the converted gir surface goes to stdout or disk,
  gated to load back through `gir-syntax` before anything is written
  (the wedge gate: an import nobody can load is worthless), with the
  mapped/dropped/why provenance log in text or `--json` form. Library
  seam `cli::import` (`import_document` with the reload gate,
  `render_json`); `importer` added to the workspace dependency table
  and the cli manifest; consumer example
  `crates/cli/examples/import_grammar.rs`; book chapter
  `docs/book/import.md` with the `import_grammar` recipe compiled by
  the docs gate; API snapshot regenerated.

### Persistent-red is loom-clean — the model-checked concurrency contract

- **runtime-core**: the persistent-red contract
  ([`RedHandle`], [`Version`], [`count_shared`]) is now model-checked,
  not prose-asserted (roadmap §11.2 "loom for the persistent-red
  mode", §12.3 P4 gate "persistent-red mode loom-clean"). A test-only
  `loom` cargo feature swaps the persistent module's version-cell
  `Arc` for loom's model type — the REAL handle participates in the
  interleaving search (tree internals stay on the kernel's `std` Arcs,
  which carry no concurrency of their own); `crates/runtime-core/
  tests/loom_red.rs` explores every schedule of a single writer
  publishing to two readers and of a reader retaining a version across
  three publishes, pinning: whole-version publication (readers never
  observe a half-published tree — a byte-sum walk proves every
  snapshot complete), cross-version structural sharing (the shared
  leaf `Arc` is pointer-identical, witnessed by `count_shared` on the
  retained history), version retention (strictly increasing numbers,
  the reader's snapshot alive after the writer exits — strong-count
  proven), and the single-writer/multi-reader shape. Run:
  `cargo test -p runtime-core --features loom`. The crate's own
  persistent unit tests run through a `run_model` shim (plain
  execution normally, loom's scheduler under the feature).

### Region-parallel chunk drive — the parallel half of the chunk seam

- **structural-index**: [`validate_chunks_parallel`] +
  [`validate_chunks_parallel_with`] + [`validate_plan_parallel`] — the
  chunk plan now executes on scoped worker threads instead of only
  planning for them (roadmap §7.9 "parallel chunk parsing", §12.2
  "region-parallel scanning", the execution half issue 032 left as the
  planning API). `std::thread::scope` fan-out, no `unsafe`, one fresh
  [`ChunkValidator`] per chunk via the factory seam (stateful checkers
  never see another chunk's bytes), workers capped at the chunk count
  with `0` resolving to `available_parallelism`, and the deterministic
  ordered merge: chunks are planned in scan order, workers own
  contiguous ascending ranges, and per-worker fault lists join in
  worker order — the merged [`ChunkReport`] IS the sequential
  [`validate_chunks`] verdict byte-for-byte, pinned by parity tests at
  every fan-out over clean + faulted + 64-seed corpus sources (with an
  engagement gate keeping the plans multi-chunk). Clipping semantics
  are shared with the sequential drive through one `chunk_text`
  helper, so the two drives cannot drift. Benchmarked
  sequential vs w1/w2/w4/w8/auto (`bench/benches/region_parallel.rs` —
  ~1.9× on the 8-chunk fixture, auto workers, debug build); consumer
  example `crates/structural-index/examples/region_parallel.rs`
  (`cargo run -p structural-index --example region_parallel`).

### Gated drives — the step-gated early-exit seam

- **runtime-core**: [`DriveGate`] + [`drive_sink_until`] — the strict
  driver contract `drive_sink`'s docs reserve their failure channel
  for: every step is shown to the gate before it reaches the sink, a
  `ControlFlow::Break` halts the drive with the rejected step
  undelivered, and the run returns a copyable [`DriveStats`]
  (`forwarded`, `stopped_early`) instead of nothing. The trait
  completes the stream-side gate family — [`FilterSink`] shapes what a
  sink receives, a gate decides whether the drive continues at all —
  mirroring the `ControlFlow` convention already on
  `structural_index::RegionSink` and `view::visit::Visitor`. Blanket
  impl for `FnMut(&FilterStep) -> ControlFlow<()>` closures;
  [`MaxErrors`] is the built-in error-budget gate (let the first `max`
  diagnostics through, halt on the one that exceeds). Pinned by
  parity/early-exit/budget/truncation-differential tests; benchmarked
  baseline vs pass-through vs early-stop
  (`bench/benches/drive_gate.rs`); consumer example
  `crates/runtime-core/examples/fail_fast.rs`
  (`cargo run -p runtime-core --example fail_fast`).

### Tree-level changed ranges (§7.7 downstream invalidation)

- **incremental**: [`tree_diff`] — the green-tree counterpart of the
  token-level `changed_ranges`: a parallel old/new walk classifies every
  compared subtree pair as `Matches`/`MayDiffer`/`Differs` (the
  tree-sitter `get_changed_ranges.c` shape) and reports the coalesced
  new-source byte ranges where the trees disagree. Identical prefix/
  suffix absorption keeps insertions and token edits precise; a refused
  middle keeps multi-edit reports conservative-sound. [`DiffVisitor`] is
  the observability seam (one event per comparison, `ControlFlow` early
  exit — the `RegionSink` convention); [`RangeCollector`] is the
  built-in consumer and `changed_tree_ranges` the convenience entry
  point. Pinned by hand-built unit suites, real JSON pack integration
  tests (token-precision, insertion, value reshape), structural
  invariants, and a 150-round seeded fuzz with a determinism re-run and
  a shape-isomorphic single-token precision gate. Benchmarked
  identical/edit/brute-force (`bench/benches/tree_diff.rs`); consumer
  example `crates/incremental/examples/tree_diff.rs`
  (`cargo run -p incremental --example tree_diff`).

### Sink statistics + predicate-gated filtering

- **runtime-core**: [`StatsSink`] — the per-kind statistics consumer:
  node/token histograms, max enter/exit depth, token byte coverage and
  the totals in one [`StreamStats`] snapshot (the O(kinds) composition
  counterpart of `ValidationSink`'s O(1) totals); grammar profiling and
  kind-coverage reports read off the snapshot instead of re-walking the
  tree. Pinned by fixed-stream, underflow, tee-parity and randomized
  oracle tests; benchmarked solo and zipped with `ValidationSink`
  (`bench/benches/stats_sink.rs`).
- **runtime-core**: [`FilterSink`] + [`FilterStep`] — the predicate-gated
  forwarding decorator: every step is shown to a
  `FnMut(&FilterStep) -> bool` gate and only accepted steps reach the
  inner sink — drop diagnostics by index or restrict a token pass to
  the kinds a tool cares about, without a second drive. Benchmarked
  pass-through vs token-dropping (`bench/benches/filter_sink.rs`).
- **runtime-core**: consumer example `examples/stats_sink.rs` — one
  drive over a hand-built stream: validation verdict plus per-kind
  statistics (run `cargo run -p runtime-core --example stats_sink`).
- **docs**: the §7.2 consumer table (module docs and roadmap) now lists
  the kind-statistics and step-gated rows alongside the shipped
  diagnostic-resolution row.

### Shared pack registry (issue 046)

- **pack-registry**: one canonical registration of the ten packs with
  three lookup axes (name, language id, extension/URI) and the uniform
  fn-pointer entry points; the CLI, both language servers, and the
  playground server consume the seam (the playground previously
  reached only seven packs).

### Streaming sinks — query matcher + region validator

- **runtime-core**: [`TeeSink`] — the two-way sink combinator that
  runs two consumers (tree builder + query matcher) over one
  `drive_sink` pass; consumer-table row for the streaming query
  matcher now points at a real implementation.
- **query**: [`QuerySink`] — the push-based query matcher implementing
  `runtime_core::sink::Sink`; matching, capture binding, dedup, and
  predicate dispatch share the pull cursor's evaluation path, pinned
  by a differential suite (valid, malformed, hand-packed, and 300
  seeded randomized streams) and a criterion head-to-head.
- **structural-index**: the region sink seam — [`RegionSink`] +
  `scan_regions` (ControlFlow early exit) with the index collector as
  a consumer, and [`FaultScanner`], the streaming fail-fast validator
  now behind `validate_fast`: the "without building the index"
  contract is real (dhat-gated at zero allocations) instead of
  documented-only.

### Chunked validation — the balanced-plan seam

- **structural-index**: `plan_chunks` now cuts chunks at TRUE depth-0
  boundaries. The region index records the depth at each region's end
  (braces do not split Code regions), so a region like ` } x` recorded
  depth 0 while the position it starts at actually sits at depth 1 —
  the old candidate filter picked such starts and produced unbalanced
  chunks (e.g. `{ "s" } x` split into `{ "s" ` + ` } x`), breaking the
  "each chunk is independently parseable" contract its own docs claim.
  Boundaries are now chosen by the depth the boundary sits at (the
  previous region's end depth), pinned by balance-property tests.
- **structural-index**: the chunk-validation seam consuming those
  plans — [`ChunkValidator`] (validate one chunk in isolation), the
  built-in [`ChunkFaultScanner`] (a fresh fail-fast FaultScanner per
  chunk), `validate_chunks` / `validate_chunks_with` /
  `validate_plan`, and the merged [`ChunkReport`] (faults attributed
  to their chunk, offsets rebased to source coordinates). Verdict
  parity with the whole-source `validate_fast` (clean ⇔ clean; first
  fault = whole-source fault) is pinned by differential + seeded
  property tests, a criterion head-to-head, and the
  `validate_chunks` consumer example. This is the shape the
  region-parallel roadmap (P4) runs on.

### Diagnostic resolution — the consumer-side error seam

- **runtime-core**: [`DiagCollector`] — the sink that resolves the
  event stream's deferred diag indexes through the parse's side table
  into owned, stream-ordered `Diagnostic`s (message + byte range),
  with an `unresolved` counter for out-of-table indexes. Every other
  shipped sink treats the error channel as a count at most; tree-and-
  diagnostics consumers now zip the collector into a `TeeSink` half
  and get both from one `drive_sink` pass. Pinned by unit tests, a
  property-oracle extension, a criterion bench group, and the
  `diag_collector` consumer example (runtime-core's first example).

### Documentation and debt

- Drift fixes: `drive_sink`'s error/tag docs, the persistent module's
  parallelism claims (the chunked merge is single-threaded today — the
  parallelism is caller-side), `TreeBuilder::finish`'s open-stack
  behavior, the incremental engine's `ContextHash` mention, the
  cookbook recipe list, the CLI usage string (missing `packs`), and
  the stale issue-044 pointer in the workspace lint rationale.
- Debt: the duplicated `#[cfg(test)]` banner in `lr-shadow`'s lower
  module removed; the vacuous u32-guard test in `runtime-core`'s input
  layer rewritten to assert what it actually verifies; gir-syntax's
  unused `thiserror` dependency dropped; the bare `unreachable!()` in
  codegen-rust's pattern emitter now states its narrowing invariant.
- Drift: the roadmap's "persistent red mode is a cargo feature of
  runtime-core" claim corrected (runtime-core defines no cargo
  features; the merge is single-threaded caller-side parallelism
  today) and §12.2's gantt annotated with the compressed actual
  delivery (issues 001–094 shipped 2026-11-02 → 2026-11-23);
  packs-json's `parse` doc no longer claims it returns the diagnostic
  side table (it returns the tree only).

### Compiler loop — descent-lower (issue 043)

- **descent-lower**: the grammar → descriptor compiler — the keystone
  that closes the pipeline's compiler loop. The front half always
  *analyzed* grammars and the runtime always *interpreted* descriptors,
  but nothing connected the two (every pack parser was hand-written).
  A canonical IR `Grammar` now lowers deterministically into the
  `descent::Desc` shape: one dense kind space (node rules, node tokens,
  plain tokens, trivia, unmatched sentinel), transparent-rule inlining
  (acyclicity-checked), FIRST/FOLLOW-driven LL(1) dispatch guards, and
  pratt-hinted rules lowered to `PrattDesc` tables whose operator nodes
  ride the operator token's kind. A trailing `rep(recover)` document
  sweep keeps arbitrary input byte-faithful. Every canonical form v0
  cannot express (lookahead predicates, bounded repetitions, transparent
  cycles, LL(1) conflicts, malformed pratt shapes) is a named refusal —
  nothing mis-lowers silently. `Lowered::feed` bridges the lexgen
  scanner to the engine's clean-feed contract.
- **fix(gir-syntax)**: the derived pratt right binding power followed
  the wrong encoding (`lbp - 1` for left-associativity); it now derives
  `rbp = lbp + 1` per the descent engine's documented `InfixDesc`
  contract. Latent until now — no consumer executed hints end-to-end
  before descent-lower.
- **fix(importer)**: `prec.left` hints emitted `rbp = lbp` (the
  right-associative encoding); converted grammars now carry
  `rbp = lbp + 1` for left associativity.
- **bench**: `benches/lowered_json.rs` — the generated pipeline
  head-to-head against the hand-written reference (scan / parse_events /
  green_tree per fixture, plus the one-time compile step), and
  `tests/lowered_budgets.rs` dhat gates (lower ≤ 600 blocks, generated
  parse ≤ 5× reference on the steady fixture — the engine's per-entry
  descriptor clones are the known follow-up).
- **lints**: every `#[allow]`/`#![allow]` moved out of crate sources
  into justified `[workspace.lints.clippy]` overrides — the source tree
  is now allow-free (`grep -rnE '#!?\[allow\(' crates/` is empty).
### Editor tooling — pack-lsp (issue 042)

- **lsp**: the pack language server (roadmap §12.2 p3d "LSP for
  packs") — a std-only LSP 3.17 server over stdio for the ten shipped
  pack languages. Documents resolve their pack by `languageId` then uri
  extension; one pack parse per version builds the green tree plus a
  parent-indexed outline from which every view is prebuilt: error-region
  diagnostics (push + pull), a hierarchical document outline named by
  first-significant tokens, folding ranges, shape-classified semantic
  tokens (fixed four-entry legend, delta-encoded), kind+text document
  highlight, and selection ranges. The stdio transport loop is
  generalized behind a `Handler` trait both servers implement. Served
  by `syntact pack-lsp`.
- **cli**: `pack-lsp` subcommand (`--max-message` bound, default 8 MiB).
- **bench**: `benches/pack_lsp.rs` (open/didChange/query latencies for
  json+toml+python fixtures) and `tests/pack_lsp_budgets.rs` dhat gates
  (open ≤ 450 blocks/KiB incl. the pack parse, zero-alloc no-match
  highlight and prebuilt-view reads).
- **perf**: the outline emits span-boundary events in document order, so
  one monotonic UTF-16 sweep (no sort) positions every view; document
  open and didChange dropped ~24% on the JSON fixture.

### Editor tooling — gir-lsp (issue 041)

- **lsp**: the `.gir` grammar language server — std-only JSON-RPC 2.0
  over stdio with LSP `Content-Length` framing: completion of
  rules/tokens/markers, hover on annotations and declarations, inline
  conflict squiggles (surface errors, LL(1) conflicts downgraded on
  pratt rules, LR(1) shadow conflicts via production origins,
  unreachable rules), definition/references, document symbols, and
  rename with stable-ID preservation (declaration order — and the
  dense `RuleId`/`TokenId` numbering — never moves). Incremental
  `didChange` sync with UTF-16 position mapping (binary-searched line
  index). Served by `syntact lsp`.
- **cli**: `lsp` subcommand (`--max-message` bound, default 8 MiB).
- **bench**: `benches/lsp.rs` (open/reanalysis/query latencies) and
  `tests/lsp_budgets.rs` dhat gates (zero-alloc definition lookup,
  per-item completion, per-decl symbols, per-site rename, open
  ≤ 600 KiB/KiB incl. the external loader bound).

### Phase 5 — ecosystem 1.0 (issues 037–040)

- **importer**: tree-sitter `grammar.js` → gir KDL converter with a
  deterministic mapped/dropped/why report, regex-decided literal
  merging, separated-list sugar detection, externals as flagged
  placeholder tokens, and the kind-multiset conformance wedge over five
  fixture grammars (json, toml, ini, csv, markdown).
- **playground-server**: std-only HTTP parse service (`POST /parse`,
  `GET /packs`) with a pure router, bounded request sizes (413),
  graceful shutdown, and deterministic JSON serialization over all
  seven packs.
- **docs**: the syntact book (six chapters) plus a cookbook of five
  recipes that are compiled and executed by the test suite; chapter
  code blocks are pinned to the recipes byte-for-byte and SUMMARY and
  chapter links are checked.
- **api freeze**: semver policy, the `docs/api-snapshot.md` public-item
  freeze enforced by `api_snapshot`, changelog; three community packs —
  **INI**, **CSV**, **Markdown** — shipped at the flagship bar
  (per-family shapes, losslessness, determinism, recovery budgets).
- **fix(gir-syntax)**: seq markers now wrap their own item when
  lowering to the IR (a marked item used to be wrapped onto the wrong
  element, and a one-item sequence with a marker failed to lower).

### Phase 4 — performance maxima (issues 033–036)

- **packs-rust / packs-c / packs-sql**: byte-based reference parsers
  with quote disambiguation, declarator grammar, and case-insensitive
  keywords.
- **runtime-core**: persistent-red mode + parallel queries.

### Phase 3 — incremental & queries (issues 030–032)

- **incremental**: the reparse engine (margin-window fast path,
  context hashes, fallback discipline) with incremental-equivalence
  fuzzing.
- **query**: the query compiler + streaming matcher (kind bitsets,
  quantifiers, predicates, captures).
- **structural-index**: block scanner + SoA region index.

### Phase 2 — recovery & flagship packs (issues 027–029)

- **packs-python**: fused scanner+parser with INDENT/DEDENT and
  f-strings.
- **packs-typescript**: byte-based TS-subset parser with ASI.
- **cli**: `syntact trace` — per-region recovery events with JSON and
  text output and rescan parity against the full scan.

### Phase 1 — analysis & codegen (issues 010–026)

- **gir-analysis**: FIRST/FOLLOW, LR(1) shadow automaton, the
  determinism report, expression lowering and factoring.
- **lexgen**: the lexer compiler (mode stacks, expected-mask PHF,
  single-token relex).
- **codegen-rust**: the compact-table backend (row displacement, diff-
  stable emission).
- **recovery**: the uniform-cost repair planner with sync hints and
  cost models.
- **harness**: the conformance runner, differential driver, recovery
  calibration, and criterion budgets.
- **packs**: JSON and TOML reference packs at the flagship TDD bar;
  `syntact check/build/conflicts/conformance/trace` CLI.

### Phase 0 — foundations (issues 001–009)

- Workspace scaffold, toolchain, ADR discipline, and the runtime-core
  event stream, green/red trees, token sets, and diagnostics.

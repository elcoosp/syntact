# Changelog

All notable changes to syntact are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
the [semver policy](docs/semver-policy.md).

## [Unreleased]

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

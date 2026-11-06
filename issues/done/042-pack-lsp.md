# 042 — pack-lsp: the pack language server

- **Epic:** post-1.0 editor tooling (roadmap §12.2 p3d "LSP for packs")
- **Crates:** crates/lsp, crates/cli, crates/bench
- **Status:** done

## Work

Issue 041 shipped the language server for grammar *authoring* (`.gir`
files). The remaining editor-facing piece of the roadmap was the server
for the languages the packs *parse*: "LSP for packs" (p3d). This
milestone implements it inside the existing `lsp` crate as a `pack`
module family over the same spine (`rpc` framing, `index` positions,
`transport` loop), so one crate owns every LSP concern:

- **Pack registry** (`lsp::pack::registry`): a `PackEntry` is a name, a
  language id, file extensions, and the pack's `parse` entry point;
  `PackRegistry::reference()` registers all ten shipped packs (the seven
  flagship + three community), mirroring the playground registry's
  deterministic registration order. Documents resolve their pack by
  `languageId` first, then by uri extension; unknown languages are
  ignored cleanly.
- **Document model** (`lsp::pack::document`): one pack `parse` per
  version produces the green tree; a single DFS materializes a
  parent-indexed outline (`lsp::pack::outline`) — byte spans, subtree
  bounds, first-significant-token spans (inherited down the subtree so
  tables/sections/pairs name themselves), the flat token list, and the
  error regions — computed once per version (edit-time compute,
  query-time reads — the rust-analyzer posture):
  - **diagnostics** — every `Event::Error` materializes an ERROR node;
    node spans (zero-width included) become severity-1 diagnostics,
    pushed after every change and served on pull;
  - **document symbols** — a hierarchical outline; names are the first
    significant token of each subtree (capped, single-line); the root is
    the document itself and unlisted; unnamable nodes are spliced;
  - **folding ranges** — every node spanning more than one line, to the
    line of its last byte (char-boundary floored);
  - **semantic tokens** — shape-based classification into a fixed
    four-entry legend (`string`, `number`, `operator`, `identifier`)
    with LSP delta encoding, precomputed as flat `u32` quintuples;
  - **document highlight** — every token matching the token under the
    cursor by kind and text; whitespace positions match nothing;
  - **selection ranges** — the ancestor chain of node spans at a
    position; a document-end position clamps to the root chain.
- **Server** (`lsp::pack::server`): the same lifecycle discipline as the
  grammar server (`initialize` → ready → `shutdown` → `exit`, exit code
  0 only after a clean shutdown), incremental `didChange` sync with
  UTF-16 mapping, push + pull diagnostics, and the six query methods.
  Query paths serve the prebuilt views by reference — no clones.
- **Transport**: the stdio loop is generalized behind a `Handler` trait;
  both servers implement it, the loop is written once (`serve` for the
  grammar server, `serve_with` for any handler).
- **CLI**: `syntact pack-lsp` (`--max-message` bound, default 8 MiB).
- **Why the reuse engine is not on the didChange hot path** (recorded
  honestly): a pack `parse` produces tree *and* token stream in one pass;
  token-level reuse (`incremental::Engine`) can skip a re-lex but cannot
  splice green trees, so it cannot skip the reparse that didChange needs
  for every tree view. The engine stays the standalone incremental layer
  for pure-lexer consumers; the didChange cost is one pack parse plus
  one outline DFS, both benchmarked below.

## Performance engineering

The first working version spent 900 µs of a 1.36 ms document open inside
the view builders — `LineIndex::position`'s per-node char walks. Three
measured changes:

1. **Event-stream outline** — the DFS emits each node's span boundaries
   (entry, name token, exit) as `(byte, slot, node)` events in document
   order, so one monotonic byte→UTF-16 sweep (no sort) positions every
   node; an early sort-based variant measured 595 µs and was replaced.
2. **Two sequential sweeps beat one interleaved sweep** (measured: the
   fused variant scattered writes across two arrays and regressed 297 →
   497 µs — reverted), and first-significant positions resolve once per
   distinct token span via a reverse-preorder one-entry memo (ancestors
   inherit, so every inheritance is a cache hit).
3. **Zero-clone queries** — the server's query paths serve the prebuilt
   views by reference; the name builder takes a short-circuit copy path.

Result (`cargo bench -p bench --bench pack_lsp`, 400-pair JSON fixture,
~25 KiB): open 1.48 → **1.12 ms** (−24%), didChange 1.43 → **1.11 ms**
(−23%). Phase split at the final state: pack parse 384 µs (the packs
carry their own budgets), outline 172 µs, line index 15 µs, views
~610 µs. Allocation gates pin the LSP side at ≤ 450 blocks/KiB for open
and didChange (the bulk is the pack parse), zero allocations for
no-match highlight and prebuilt-view reads.

## TDD evidence

Unit tests per module (registry lookups + extension/language mapping;
document views over json/toml fixtures incl. error documents, UTF-16
positions, empty/whitespace docs, char-boundary edits, and the span
table pinned against `LineIndex::position`; server lifecycle + dispatch
+ params rejection); a scripted end-to-end session test
(`tests/pack_session.rs`) driving the pack server over in-memory buffers
through the full lifecycle (initialize → didOpen → diagnostics push →
six queries → didChange break/repair → second document via extension
fallback → didClose → shutdown → exit) with multi-document, UTF-16, and
unknown-language sessions; CLI subcommand tests; dhat budget gates
(`tests/pack_lsp_budgets.rs`); criterion benches (`benches/pack_lsp.rs`)
plus the `pack_lsp_phases` attribution example. 41 new tests in the
workspace.

## Exit criteria

`cargo test --workspace` green (571 tests); clippy pedantic zero
warnings; benches recorded with the edit/parse/views split; api snapshot
regenerated; book chapter `docs/book/pack-lsp.md` + SUMMARY link with
docs-sync green; README workspace tour and CHANGELOG updated.

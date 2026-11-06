# 041 — gir-lsp: the grammar language server

- **Epic:** post-1.0 editor tooling (roadmap §9.4 "Grammar authoring tooling")
- **Crates:** crates/lsp, crates/cli, crates/bench
- **Status:** done

## Work

The `lsp` crate was the last scaffolding-only member of the workspace
(`//! lsp:LSP server scaffolding`). This milestone implements the
`.gir` grammar language server specified by roadmap §9.4:

- **Transport**: JSON-RPC 2.0 over stdio with LSP `Content-Length`
  framing (std-only, matching the playground-server posture; no async
  runtime, no external LSP SDK — ADR-012 thin-wrapper discipline).
- **Document model**: incremental `didChange` sync (range edits) over a
  workspace of `.gir` documents; byte-offset ↔ LSP UTF-16 line/character
  positions via a binary-searched line index.
- **Features** (the §9.4 list):
  - completion of rules/tokens (plus `(seq)` markers) at reference sites;
  - hover on annotations (`kind=`, `node=`, `reuse=`, `wrap-error=`,
    `lbp=`/`rbp=`/`assoc=`, `trivia=`, `index-bound=`, `allow-trailing=`,
    `unicode=`) and on the backward-binding markers, plus signature hover
    on rule/token declarations and references;
  - inline conflict squiggles: surface errors at their spans, LL(1)
    conflict sites from the determinism report (downgraded on pratt
    rules), LR(1) shadow conflicts mapped through `lr-shadow`
    production origins, left recursion without a pratt hint,
    unreachable rules;
  - go-to-definition / find-references (workspace indexed by uri;
    per-file symbol identity — the surface is a closed package by
    construction);
  - rename with stable-ID preservation: one `TextDocumentEdit` per
    touched file, every reference site updated, declaration order (and
    therefore dense `RuleId`/`TokenId` numbering) untouched;
  - document symbols (rules + tokens with selection ranges).
- **CLI**: `syntact lsp` serves stdio (`--max-message` bound).
- **Book**: `docs/book/grammar-lsp.md` chapter + SUMMARY link.

## TDD evidence

Unit tests per module (framing + JSON-RPC in `rpc`, position mapping +
word scanning in `index`, symbol table/features in `analysis`, the
lifecycle/dispatch state machine in `server`); a scripted end-to-end
integration test over in-memory buffers driving `serve()` through the
full lifecycle (initialize → didOpen → queries → didChange → rename →
shutdown → exit), multi-file and UTF-16 position sessions; criterion
bench (`benches/lsp.rs`); dhat allocation gates
(`tests/lsp_budgets.rs`). 68 tests in the crate.

## Performance record

300-rule fixture (~20 KiB), `cargo bench -p bench --bench lsp`:

| Path | Time |
|---|---|
| line index build | 9.1 µs |
| document open (parse → symbols → report → shadow) | 10.1 ms |
| didChange reanalysis | 8.7 ms |
| hover / hover annotation | 0.42 / 0.73 µs |
| completion | 0.21 µs |
| definition | 0.12 µs (zero allocations) |
| rename plan | 0.39 µs |
| document symbols | 46 µs |
| workspace open ×8 | 78 ms |

Open/reanalysis are dominated by the canonical KDL loader shared with
`syntact check`/`build` (~2 MB/s; measured split: kdl parse 9.5 ms of
10.5 ms, determinism report 0.5 ms, shadow automaton 0.35 ms). The
LSP-owned paths are all nanoseconds, and the allocation gates pin them
(zero-alloc definition lookup; completion one block per item after the
`detail: &'static str` + pre-sized-Vec changes).

**Follow-up (loader bound):** `just-kdl` 0.3.0 would replace the
`kdl` dependency behind the existing thin wrapper (its Reader event
stream carries byte spans, 10–15× faster per its benchmarks), but a
probe found data-corruption bugs — `"a\nb"` unescapes to `a\0b` and
`lbp=1` decodes to `Number("d\0")`. Blocked until upstream fixes
escape/number decoding; the wrapper keeps the swap contained when it
lands.

## Exit criteria

`cargo test --workspace` green (68 new tests); clippy pedantic zero
warnings; benches recorded; api snapshot regenerated; book chapter
added with docs-sync green.

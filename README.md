<div align="center">

<img src="docs/logo.png" alt="syntact logo" width="300"/>

# syntact

**A staged parser compiler: grammar packages in, production-grade Rust parser crates out.**

[![CI](https://img.shields.io/github/actions/workflow/status/elcoosp/syntact/ci.yml?style=flat-square&label=CI)](https://github.com/elcoosp/syntact/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/actions/workflow/status/elcoosp/syntact/release.yml?style=flat-square&label=release)](https://github.com/elcoosp/syntact/actions/workflows/release.yml)
[![crates.io](https://img.shields.io/crates/v/syntact?style=flat-square)](https://crates.io/crates/syntact)
[![Rust](https://img.shields.io/badge/rust-stable%20%E2%80%A2%20edition%202024-DEA584?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-663399?style=flat-square)](https://github.com/rust-lang/rust)
[![Clippy](https://img.shields.io/badge/clippy-pedantic%20zero--warnings-8A2BE2?style=flat-square&logo=rust)](https://doc.rust-lang.org/clippy/)
[![unsafe](https://img.shields.io/badge/unsafe-forbidden-CB3837?style=flat-square&logo=rust)](https://doc.rust-lang.org/book/ch19-01-unsafe-rust.html)
[![License](https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-3D6CDB?style=flat-square)](#license)

</div>

---

`syntact` compiles a grammar package (`.gir`) into a generated Rust parser crate that reads like the
best hand-written parsers in the ecosystem — OXC, ruff, rust-analyzer — because those parsers are the
performance ceiling and we generate code in their image. The user never chooses a parsing algorithm:
the compiler classifies determinism at build time, selects backends per grammar region, and compiles
in error recovery, incremental reuse machinery, queries, and multiple consumption modes.

> The architecture is fully specified in [`docs/architecture-and-roadmap.md`](docs/architecture-and-roadmap.md)
> — every load-bearing design claim carries a primary-source evidence pointer (oxc, tree-sitter,
> rust-analyzer, rowan, lalrpop, grmtools, biome, ruff, lezer).

## Why another parser toolchain?

| | syntact |
|---|---|
| **Backend** | Generated recursive descent + Pratt as primary; a canonical **LR(1) shadow automaton** is compiled alongside for parse states, expected sets and repair planning — even when the hot path never dispatches a table |
| **Tree model** | **Green/red trees with structural sharing** (rowan/cstree lineage); the parser emits a flat **8-byte event stream** (rust-analyzer lineage) and never builds trees directly |
| **Incrementality** | A **codegen contract, not a runtime afterthought**: reuse fields, context hashes and lookahead margins are first-class IR outputs (tree-sitter + lezer mechanisms, unified) |
| **Error recovery** | **Deterministic** CPCT\*-shaped repair planning with a *work* budget (not wall-clock), canonical repair ordering, grammar-derived sync sets, bogus-node/missing-marker conventions |
| **Performance** | An operating model: allocation-count snapshots gate CI, criterion budgets per commit, PGO wired from the first generated parser — every optimization lands with a measured win and a conformance gate |
| **Losslessness** | Byte-faithful trees: whitespace and comments ride trivia records (ADR-008), every byte of the source survives a parse round-trip |

## Workspace tour

```
crates/
├── runtime-core      event protocol, input layer, sinks, green trees, token sets, diagnostics
├── gir-syntax        KDL surface loader for .gir grammar packages
├── gir-ir            canonical typed grammar IR (rules, alternatives, predicates; interned ids)
├── gir-analysis      FIRST/FOLLOW fixpoints, LL(1)/LR verdicts, determinism reports
├── gir-transform     Pratt hint lowering, left factoring, cycle-guarded inlining
├── lr-shadow         canonical LR(1) kernel automaton (the shadow)
├── lexgen            lexer compiler: regex → NFA → DFA, char classes, modes, expected masks
├── descent           descriptor-driven descent + Pratt backend emitting the event protocol
├── codegen-rust      O(1) table emission backend
├── recovery          deterministic repair planner (CPCT+-shaped, work-budgeted)
├── harness           conformance runner: corpora, tolerated-error budgets, differential testing
├── incremental       reparse engine: relex fast path, fragment reuse, changed-range splicing
├── query             tree-sitter-shaped query compiler + streaming matcher (.scm-q)
├── structural-index  64-byte block scanner + flat SoA region index (~16 B/region)
├── cli               syntact check / build / conflicts / conformance / trace
├── bench             criterion suites + allocation-budget gates
└── packs/
    ├── json          reference JSON pack (the differential oracle)
    ├── toml  typescript  python  rust  c  sql      flagship packs
    └── ini  csv  markdown                            community packs
```

## Quick start

```bash
git clone https://github.com/elcoosp/syntact && cd syntact
cargo test --workspace        # full test pyramid: unit + property + conformance + fuzz-shaped
cargo clippy --workspace --all-targets   # pedantic, zero warnings
cargo run -p cli -- check packs/json
```

Parse a document and consume it in three modes from one event stream:

```rust
use packs_json::parse;
use runtime_core::sink::{TokenSink, ValidationSink, drive_sink};

let tree = parse(r#"{"alpha": 1, "beta": [true, null]}"#).expect("parses");

// validate mode: O(1)-memory counts + balance verdict
// scan mode:    flat (kind, range) token stream for highlighting-class tools
// full mode:    the green tree above — byte-faithful, structural-sharing, error-flagged
```

## Engineering discipline

- **TDD with a real test pyramid** — unit tests per mechanism, property/fuzz-shaped streams
  (never panic, always a `Result`), golden/conformance corpora, differential oracles per pack,
  and dhat allocation-budget tests gating CI.
- **`clippy::pedantic` at zero warnings** with a documented, justified allow-list (issue 001);
  `unsafe_code = "forbid"` workspace-wide; `missing_docs` enforced.
- **Conventional commits → git-cliff changelog**; GitHub Actions CI on stable with
  fmt/clippy/test/bench gates and a release pipeline.
- **Issues as the unit of work** — every issue in [`issues/`](issues) carries work items, TDD
  requirements and exit criteria; completed issues move to [`issues/done/`](issues/done).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

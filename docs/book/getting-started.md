# Getting started

syntact is an incremental parser-toolkit workspace built around one
idea: every parser — hand-written reference or generated — emits the
same tiny event stream, and everything else (trees, recovery,
incremental reuse, queries) is a consumer of that stream.

This chapter gets you from zero to a lossless parse. The workspace has
three layers:

- **runtime-core** — the event protocol, the source input layer, the
  sinks, and green trees with structural sharing.
- **crates/** — the grammar pipeline: `gir-*` (the grammar IR and its
  KDL surface), `lexgen` (lexer compiler), `lr-shadow` (LR(1) shadow
  automaton), `descent` (Pratt backend), `recovery`, `codegen-rust`,
  `incremental`, `query`, `structural-index`, plus the `syntact` CLI.
- **packs/** — one crate per language (JSON, TOML, Python, TypeScript,
  Rust, C, SQL), each shipping a lossless reference parser on the event
  protocol.

## Parse your first document

Every pack exposes `parse(source) -> GreenNode`. The green tree is
lossless: rendering it reproduces the source byte-for-byte, including
comments and whitespace (which never ride token *events* — they flow
through trivia records at tree-build time).

The recipe below parses JSON, renders the tree back, and walks its
leaves:

```rust
// Recipe: parse JSON and walk the green tree.
//
// The end-to-end pack pipeline in six lines: `packs_json::parse`
// returns the green tree; rendering it reproduces the source
// byte-faithfully (losslessness), and the leaf walk shows how trivia
// and tokens interleave in DFS order.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

#[test]
fn parse_and_render_byte_faithfully() {
    let source = r#"{"name": "syntact", "n": 42}"#;
    let tree: Arc<GreenNode> = packs_json::parse(source).expect("lossless parse");

    // Losslessness: concatenating every leaf reproduces the source.
    let input = Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source);

    // The leaf walk: clean tokens plus trivia leaves (kind 14) plus the
    // synthesized EOF leaf — trivia never rides token *events*
    // (ADR-008), but the green tree carries it for byte-faithfulness.
    let leaves = flatten(&tree, 0..u32::try_from(source.len()).expect("fits u32"));
    let rendered: String = leaves
        .iter()
        .map(|(_, range)| &source[range.start as usize..range.end as usize])
        .collect();
    assert_eq!(rendered, source);
    let text_leaves = leaves
        .iter()
        .filter(|(kind, _)| *kind != 14 && *kind != runtime_core::EOF_KIND)
        .count();
    assert_eq!(text_leaves, 9, "9 real tokens in the object");
}

/// DFS flatten of a green tree into `(kind, range)` leaves.
fn flatten(node: &GreenNode, _fallback: Range<u32>) -> Vec<(u16, Range<u32>)> {
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

fn walk(node: &GreenNode, out: &mut Vec<(u16, Range<u32>)>) {
    for child in &node.children {
        match child {
            GreenChild::Node(inner) => walk(inner, out),
            GreenChild::Token { kind, range } => out.push((*kind, range.clone())),
        }
    }
}
```


## The CLI

The `syntact` binary drives the real pipeline (its integration tests
call the same `run` function):

- `syntact check GRAMMAR.gir` — validate a grammar (KDL → IR).
- `syntact build GRAMMAR.gir` — run the front half: IR, lexer DFA, LR(1)
  shadow automaton; prints per-stage stats.
- `syntact conflicts GRAMMAR.gir` — the determinism report: per-rule
  backend classification and every LL(1) conflict site.
- `syntact conformance FILES…` — gate `render(parse(x)) == x` over
  documents.
- `syntact trace PACK FILE` — per-region recovery events.

Next: [the event protocol](event-protocol.md) — the contract every
parser and consumer shares.

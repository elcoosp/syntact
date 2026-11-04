# Pack walkthrough

A pack is a self-contained parser crate. Its pieces:

- `grammar.kdl` — the grammar in the gir surface (tokens, rules, the
  separated-list sugar, `@node`/`@reuse` annotations).
- `reference.rs` — the hand-written parser on the event protocol. It is
  the differential oracle: generated parsers are conformance-tested
  against it (issue 021).
- `kinds` / `token_kinds` — the dense kind numbering shared by events,
  trees, and queries.

## The contract

A reference parser is judged by three gates (the harness runs them over
a corpus):

1. **Losslessness** — `render(parse(x)) == x` for every corpus
   document, recovery included.
2. **Determinism** — same input, same tree, always.
3. **Recovery discipline** — every syntax error is followed by an
   `ERROR_KIND` node, and parsing continues at the next sync token.

The recipe from [getting started](getting-started.md) exercises gate 1
end to end:

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


## Consuming trees

The green tree is immutable and position-independent. `render` gives
you the source back; `token_ranges` gives the flat `(kind, range)` list
queries and the structural index consume; `node_count`/`token_count`
feed the bench budgets. Because children are `Arc`-shared, incremental
reparsing (next chapter) reuses unchanged subtrees without copying.

Next: [incremental reparsing](incremental.md).

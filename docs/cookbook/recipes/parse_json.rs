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

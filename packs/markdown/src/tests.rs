//! packs-markdown test suite (issue 040, flagship bar): per-family
//! shapes, losslessness, determinism, recovery progress, and
//! per-fixture tolerated-error budgets — mirroring the other packs.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid Markdown-subset families plus malformed inputs
/// with per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- valid families -------------------------------------------------
    ("# Title\n", 0),                   // h1
    ("###### Deep\n", 0),               // h6
    ("#\n", 0),                         // bare marker heading
    ("#    spaced text\n", 0),          // marker + indent + text
    ("plain line\n", 0),                // paragraph
    ("one\ntwo\nthree\n", 0),           // paragraph lines
    ("# H\n\ntext\n", 0),               // heading + blank + paragraph
    ("\n\nleading blanks\n", 0),        // blank lines as trivia
    ("  indented text\n", 0),           // leading indent trivia
    ("#tag is text\n", 0),              // 1 hash + non-space = paragraph
    ("####### seven hashes\n", 0),      // 7 hashes = paragraph
    ("trailing no newline", 0),         // EOF without terminator
    ("", 0),                            // empty doc
    ("\n", 0),                          // blank-only doc
    ("# H\ntext\n### H3\nmore\n", 0),   // mixed blocks
    ("line with # hash mid-text\n", 0), // mid-line hash is text
    // -- malformed with tolerated-error budgets -------------------------
    ("#\r", 1), // stray carriage return at line end
];

/// Build the green tree through the full `parse` integration.
fn tree_for(source: &str) -> Arc<GreenNode> {
    crate::parse(source).expect("tree integration never fails structurally")
}

/// DFS shape of non-trivia tokens (kind, range).
fn token_shape(tree: &GreenNode) -> Vec<(u16, Range<u32>)> {
    fn walk(node: &GreenNode, out: &mut Vec<(u16, Range<u32>)>) {
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => walk(sub, out),
                GreenChild::Token { kind, range } => {
                    if *kind != tk::WHITESPACE {
                        out.push((*kind, range.clone()));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

#[test]
fn valid_corpus_is_lossless_and_error_free() {
    for (source, tolerated) in CORPUS {
        if *tolerated != 0 {
            continue;
        }
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, 0, "valid corpus must be error-free: {source:?}");
    }
}

#[test]
fn malformed_corpus_stays_lossless_within_budgets() {
    for (source, tolerated) in CORPUS {
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "lossless on {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert!(
            errors <= *tolerated,
            "tolerated {tolerated} errors, got {errors} on {source:?}"
        );
        if *tolerated > 0 {
            assert!(errors > 0, "malformed fixture must error: {source:?}");
        }
    }
}

#[test]
fn families_produce_expected_shapes() {
    // A heading: marker, text, terminator (the marker rides before the
    // node, toml-pack style; inline spaces are trivia).
    let (_, tokens, errors) = parse_events("# Hi\n");
    assert_eq!(errors, 0);
    assert_eq!(
        tokens,
        vec![(tk::ATX, 0..1), (tk::TEXT, 2..4), (tk::NEWLINE, 4..5)]
    );
    // A paragraph: text + terminator.
    let (_, tokens, errors) = parse_events("hello\n");
    assert_eq!(errors, 0);
    assert_eq!(tokens, vec![(tk::TEXT, 0..5), (tk::NEWLINE, 5..6)]);
    // Seven hashes are plain text (the subset caps headings at 6).
    let (_, tokens, errors) = parse_events("####### x\n");
    assert_eq!(errors, 0);
    assert_eq!(tokens[0], (tk::TEXT, 0..9));
}

#[test]
fn node_shapes_nest_correctly() {
    let tree = tree_for("# H\ntext\n");
    assert_eq!(tree.kind, ROOT_KIND);
    let node_kinds: Vec<u16> = tree
        .children
        .iter()
        .filter_map(|child| match child {
            GreenChild::Node(sub) => Some(sub.kind),
            GreenChild::Token { .. } => None,
        })
        .collect();
    assert_eq!(node_kinds, vec![kinds::HEADING, kinds::PARAGRAPH]);
    // The heading node wraps text + terminator (marker outside).
    let heading = tree
        .children
        .iter()
        .find_map(|child| match child {
            GreenChild::Node(sub) if sub.kind == kinds::HEADING => Some(sub.clone()),
            _ => None,
        })
        .expect("heading node");
    assert_eq!(
        token_shape(&heading),
        vec![(tk::TEXT, 2..3), (tk::NEWLINE, 3..4)]
    );
}

#[test]
fn blank_lines_and_indent_are_trivia_only() {
    let tree = tree_for("\n\n  text\n");
    let (_, _, errors) = parse_events("\n\n  text\n");
    assert_eq!(errors, 0);
    assert_eq!(tree.node_count(), 2, "root + one paragraph");
}

#[test]
fn parsing_is_deterministic() {
    for (source, _) in CORPUS {
        let a = parse_events(source);
        let b = parse_events(source);
        assert_eq!(a.0, b.0, "events deterministic for {source:?}");
        assert_eq!(a.1, b.1, "ranges deterministic for {source:?}");
        assert_eq!(a.2, b.2, "error count deterministic for {source:?}");
        assert_eq!(tree_for(source), tree_for(source), "tree deterministic");
    }
}

#[test]
fn recovery_always_makes_progress() {
    let source = "#\rtxt\n@\nmore\n";
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1, "the stray CR errors: {errors}");
    assert_eq!(
        tree_for(source).render(&Source::from_str_unchecked(source)),
        source
    );
}

#[test]
fn events_validate_through_process() {
    for (source, _) in CORPUS {
        let (events, _, _) = parse_events(source);
        let mut wrapped = Vec::with_capacity(events.len() + 2);
        wrapped.push(Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(events);
        wrapped.push(Event::Finish);
        assert!(process(&wrapped).is_ok(), "balanced stream for {source:?}");
    }
}

//! packs-csv test suite (issue 040, flagship bar): per-family shapes,
//! losslessness, determinism, recovery progress, and per-fixture
//! tolerated-error budgets — mirroring the other packs' discipline.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid CSV families plus malformed inputs with
/// per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- valid families -------------------------------------------------
    ("a,b,c\n", 0),              // plain row
    ("a,b", 0),                  // no trailing newline
    ("a\nb\nc\n", 0),            // multiple rows
    ("a,,b\n", 0),               // empty middle field
    (",\n", 0),                  // empty everything
    ("\n", 0),                   // one empty row
    ("\n\n", 0),                 // two empty rows
    ("\"a\",b\n", 0),            // quoted field
    ("\"a,\"\",b\",c\n", 0),     // escaped quotes inside quotes
    ("\"multi\nline\",2\n", 0),  // quoted field spanning lines
    ("a, \"b\" ,c\n", 0),        // stray spaces around fields
    ("\"\",x\n", 0),             // empty quoted field
    ("name,age\nalice,42\n", 0), // header-ish shape
    ("", 0),                     // empty doc
    ("a; b\n", 0),               // semicolon rides the bare run
    // -- malformed with tolerated-error budgets -------------------------
    ("\"unterminated\n", 1),    // unterminated quote consumes to EOF
    ("a,\"broken", 1),          // unterminated quote at EOF
    ("x,\"a\nb\"y,\"c\"\n", 1), // garbage after a quoted field ends
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
    // A plain row: bare, comma, bare, newline.
    let (_, tokens, errors) = parse_events("a,b\n");
    assert_eq!(errors, 0);
    assert_eq!(
        tokens,
        vec![
            (tk::BARE, 0..1),
            (tk::COMMA, 1..2),
            (tk::BARE, 2..3),
            (tk::NEWLINE, 3..4),
        ]
    );
    // A quoted field with an escaped quote and an embedded comma.
    let (_, tokens, errors) = parse_events("\"a,\"\",b\",c\n");
    assert_eq!(errors, 0);
    assert_eq!(
        tokens,
        vec![
            (tk::QUOTED, 0..8),
            (tk::COMMA, 8..9),
            (tk::BARE, 9..10),
            (tk::NEWLINE, 10..11),
        ]
    );
    // CRLF is one NEWLINE token covering both bytes.
    let (_, tokens, errors) = parse_events("a\r\n");
    assert_eq!(errors, 0);
    assert_eq!(tokens, vec![(tk::BARE, 0..1), (tk::NEWLINE, 1..3)]);
}

#[test]
fn node_shapes_nest_correctly() {
    let tree = tree_for("a,b\nc,d\n");
    assert_eq!(tree.kind, ROOT_KIND);
    let mut node_kinds = Vec::new();
    for child in &tree.children {
        if let GreenChild::Node(sub) = child {
            node_kinds.push(sub.kind);
        }
    }
    assert_eq!(node_kinds, vec![kinds::ROW, kinds::ROW]);
    // Every row holds one FIELD node per comma-separated column.
    let first = tree
        .children
        .iter()
        .find_map(|child| match child {
            GreenChild::Node(sub) if sub.kind == kinds::ROW => Some(sub.clone()),
            _ => None,
        })
        .expect("row node");
    let fields: Vec<u16> = first
        .children
        .iter()
        .filter_map(|child| match child {
            GreenChild::Node(sub) => Some(sub.kind),
            GreenChild::Token { .. } => None,
        })
        .collect();
    assert_eq!(fields, vec![kinds::FIELD, kinds::FIELD]);
    // The row's clean tokens: two bare fields, the comma, and the
    // row-terminating newline.
    assert_eq!(
        token_shape(&first),
        vec![
            (tk::BARE, 0..1),
            (tk::COMMA, 1..2),
            (tk::BARE, 2..3),
            (tk::NEWLINE, 3..4),
        ]
    );
}

#[test]
fn unterminated_quote_is_one_error_region_to_eof() {
    let source = "a,\"broken\nb\n";
    let (events, tokens, errors) = parse_events(source);
    assert_eq!(errors, 1);
    // The error token swallows everything from the stray quote to EOF.
    let last = tokens.last().expect("error token");
    assert_eq!(*last, (ERROR_MARKER, 2..12));
    let _ = events;
}

/// The error-region token kind (runtime-core sentinel).
const ERROR_MARKER: u16 = runtime_core::ERROR_KIND;

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
    // Unterminated quotes at every position still terminate.
    let source = "\"a\n\"b\n\"c\n";
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1);
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

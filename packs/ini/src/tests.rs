//! packs-ini test suite (issue 040, flagship bar): per-family shapes,
//! losslessness, determinism, recovery progress, and per-fixture
//! tolerated-error budgets — mirroring the packs-json / packs-toml
//! discipline.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::ERROR_KIND;
use runtime_core::events::process;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid INI families plus malformed inputs with
/// per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- valid families -------------------------------------------------
    ("a = 1", 0),                      // plain setting
    ("a=1", 0),                        // no spaces
    ("a = some value with spaces", 0), // value run
    ("a =", 0),                        // empty value
    ("[section]", 0),                  // bare header
    ("[section]\na = 1\nb = 2", 0),    // section + settings
    ("[a]\nx = 1\n[b]\ny = 2", 0),     // two sections
    ("; comment\na = 1", 0),           // leading comment
    ("a = 1 ; trailing comment", 0),   // trailing comment
    ("# hash comment\na = 1", 0),      // hash comment
    ("\n\na = 1\n\n", 0),              // blank lines
    ("", 0),                           // empty doc
    ("a = \"quoted = value\"", 0),     // `=` inside value
    ("[a.b.c]\nx = 1", 0),             // dotted section name
    ("no-value-key\na = 1", 1),        // missing `=` (error line, recovers)
    // -- malformed with tolerated-error budgets -------------------------
    ("= 1", 1),             // missing key
    ("[section", 1),        // unterminated header
    ("[section\nx = 1", 1), // unterminated header (newline sync)
    ("a\nb = 1", 1),        // key without `=`
    ("@@@\n", 1),           // garbage line
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
                    if *kind != tk::WHITESPACE && *kind != tk::COMMENT {
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
    // A section header brackets its name.
    let (_, tokens, errors) = parse_events("[host]\n");
    assert_eq!(errors, 0);
    assert_eq!(
        tokens,
        vec![(tk::LBRACKET, 0..1), (tk::BARE, 1..5), (tk::RBRACKET, 5..6),]
    );
    // A setting is key, `=`, value.
    let (_, tokens, errors) = parse_events("port = 8080");
    assert_eq!(errors, 0);
    assert_eq!(
        tokens,
        vec![
            (tk::BARE, 0..5), // includes the space (raw run, untrimmed)
            (tk::EQUALS, 5..6),
            (tk::VALUE, 7..11),
        ]
    );
}

#[test]
fn node_shapes_nest_correctly() {
    let tree = tree_for("[net]\nhost = local\n");
    assert_eq!(tree.kind, ROOT_KIND);
    // Root children: SECTION, SETTING (trivia leaves interleaved).
    let mut node_kinds = Vec::new();
    for child in &tree.children {
        if let GreenChild::Node(sub) = child {
            node_kinds.push(sub.kind);
        }
    }
    assert_eq!(node_kinds, vec![kinds::SECTION, kinds::SETTING]);
    // The section node wraps its name and closer; the opening bracket
    // rides as the preceding sibling token (toml-pack discipline).
    let section = tree
        .children
        .iter()
        .find_map(|child| match child {
            GreenChild::Node(sub) if sub.kind == kinds::SECTION => Some(sub.clone()),
            _ => None,
        })
        .expect("section node");
    assert_eq!(
        token_shape(&section),
        vec![(tk::BARE, 1..4), (tk::RBRACKET, 4..5)]
    );
}

#[test]
fn errors_are_wrapped_in_error_regions() {
    let tree = tree_for("@@@\n");
    let (_, _, errors) = parse_events("@@@\n");
    assert_eq!(errors, 1);
    let mut saw_error = false;
    for child in &tree.children {
        if let GreenChild::Node(sub) = child {
            if sub.kind == ERROR_KIND || sub.has_error {
                saw_error = true;
            }
        }
    }
    assert!(saw_error, "the garbage line is wrapped: {tree:?}");
}

#[test]
fn parsing_is_deterministic() {
    for (source, _) in CORPUS {
        let a = parse_events(source);
        let b = parse_events(source);
        assert_eq!(a.0, b.0, "events deterministic for {source:?}");
        assert_eq!(a.1, b.1, "ranges deterministic for {source:?}");
        assert_eq!(a.2, b.2, "error count deterministic for {source:?}");
        let tree_a = tree_for(source);
        let tree_b = tree_for(source);
        assert_eq!(tree_a, tree_b, "tree deterministic for {source:?}");
    }
}

#[test]
fn recovery_always_makes_progress() {
    // The pathological input: every line malformed. The parser must
    // consume the whole document and terminate.
    let source = "@\n=\n[\na\n] =\n";
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1, "garbage lines error: {errors}");
    let tree = tree_for(source);
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "still lossless"
    );
    // And the process() gate accepts the wrapped stream (balanced).
    let (events, _, _) = parse_events(source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(runtime_core::events::Event::Start {
        kind: ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events);
    wrapped.push(runtime_core::events::Event::Finish);
    assert!(process(&wrapped).is_ok(), "balanced event stream");
}

#[test]
fn events_validate_through_process() {
    for (source, _) in CORPUS {
        let (events, _, _) = parse_events(source);
        let mut wrapped = Vec::with_capacity(events.len() + 2);
        wrapped.push(runtime_core::events::Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(events);
        wrapped.push(runtime_core::events::Event::Finish);
        assert!(process(&wrapped).is_ok(), "balanced stream for {source:?}");
    }
}

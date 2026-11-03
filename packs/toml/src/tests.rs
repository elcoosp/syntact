//! packs-toml test suite (issue 025): per-family shapes, losslessness,
//! determinism, recovery progress, and per-fixture tolerated-error
//! budgets — mirroring the packs-json discipline.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::ERROR_KIND;
use runtime_core::events::{Event, Step, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid TOML across all families plus malformed inputs
/// with per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- valid families -------------------------------------------------
    ("a = 1", 0),
    ("a = -0x1F", 0),                      // hex int, sign
    ("a = 0b1010_1010", 0),                // bin int, underscores
    ("a = 0o755", 0),                      // oct int
    ("a = 3.14", 0),                       // float
    ("a = 6.626e-34", 0),                  // float exponent
    ("a = inf", 0),                        // special float
    ("a = -nan", 0),                       // signed special float
    ("a = true", 0),                       // boolean
    ("a = 1979-05-27T07:32:00Z", 0),       // offset date-time
    ("a = 1979-05-27", 0),                 // local date
    ("a = 07:32:00.999", 0),               // local time
    ("a = \" basic \\n escape\"", 0),      // basic string
    ("a = 'literal \\ no escape'", 0),     // literal string
    ("a = \"\"\"\nmulti\nline\"\"\"", 0),  // multi-line basic
    ("a = '''multi\nliteral'''", 0),       // multi-line literal
    ("a.b.c = 1", 0),                      // dotted key
    ("a.\"quoted key\" = 1", 0),           // quoted key part
    ("a = [1, 2, 3]", 0),                  // array
    ("a = [\n  1,\n  2, # comment\n]", 0), // multi-line array
    ("a = [1, [2, [3]]]", 0),              // nested arrays
    ("a = [1, 2,]", 0),                    // trailing comma
    ("a = { b = 1, c = \"x\" }", 0),       // inline table
    ("[table]\nx = 1", 0),                 // table header
    ("[a.b.c]\nx = 1", 0),                 // dotted header
    ("[[aot]]\nx = 1\n[[aot]]\nx = 2", 0), // array of tables
    ("# only a comment", 0),               // comment-only doc
    ("", 0),                               // empty doc
    // -- malformed with tolerated-error budgets -------------------------
    ("a = ", 1),               // missing value
    ("a = @", 1),              // garbage value
    ("= 1", 1),                // missing key
    ("[table\nx = 1", 1),      // unterminated header
    ("a = [1, 2", 1),          // unterminated array
    ("\"unterminated = 1", 1), // unterminated string key
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
fn families_produce_expected_token_kinds() {
    let cases: &[(&str, u16)] = &[
        ("a = 1", tk::INTEGER),
        ("a = 0x1f", tk::INTEGER),
        ("a = 3.14", tk::FLOAT),
        ("a = inf", tk::FLOAT),
        ("a = true", tk::BOOLEAN),
        ("a = 1979-05-27T07:32:00Z", tk::DATETIME),
        ("a = 07:32:00", tk::DATETIME),
        ("a = \"x\"", tk::STRING),
        ("a = 'x'", tk::STRING),
        ("a = \"\"\"x\"\"\"", tk::STRING),
        ("a = '''x'''", tk::STRING),
    ];
    for (source, kind) in cases {
        let (events, tokens, errors) = parse_events(source);
        assert_eq!(errors, 0, "valid: {source:?}");
        let value = tokens
            .iter()
            .find(|(k, _)| *k == *kind)
            .unwrap_or_else(|| panic!("{source:?}: expected token kind {kind}, got {tokens:?}"));
        let _ = value;
        // The token event count matches the range table.
        let token_events = events
            .iter()
            .filter(|e| matches!(e, Event::Token { .. }))
            .count();
        assert_eq!(token_events, tokens.len(), "parallel ranges: {source:?}");
    }
}

/// Collect every node kind in the tree (DFS).
fn node_kinds(node: &GreenNode, out: &mut Vec<u16>) {
    out.push(node.kind);
    for child in &node.children {
        if let GreenChild::Node(sub) = child {
            node_kinds(sub, out);
        }
    }
}

#[test]
fn document_shape_has_tables_and_pairs() {
    let source = "[table]\nx = 1\n[[aot]]\ny = [1, { z = true }]";
    let tree = tree_for(source);
    let shape = token_shape(&tree);
    // Key kinds appear in DFS order; spot-check the structural tokens.
    let kinds_seen: Vec<u16> = shape.iter().map(|(k, _)| *k).collect();
    assert!(kinds_seen.contains(&tk::LDOUBLE), "array-of-tables header");
    assert!(kinds_seen.contains(&tk::LBRACE), "inline table");
    // Node kinds present.
    let mut nodes = Vec::new();
    node_kinds(&tree, &mut nodes);
    for expected in [
        ROOT_KIND,
        kinds::TABLE,
        kinds::ARRAY_TABLE,
        kinds::KEY_VALUE,
        kinds::KEY,
        kinds::ARRAY,
        kinds::INLINE_TABLE,
    ] {
        assert!(nodes.contains(&expected), "missing node kind {expected}");
    }
}

#[test]
fn comments_are_trivia_never_tokens() {
    let source = "# header\na = 1 # trailing\n# footer";
    let tree = tree_for(source);
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
    let (_, tokens, _) = parse_events(source);
    assert!(
        tokens.iter().all(|(k, _)| *k != tk::COMMENT),
        "comments never enter the token table"
    );
}

#[test]
fn parsing_is_deterministic() {
    let sources = [
        "a = { b = [1, 2], c = \"x\" }\n[t]\ny = 0o17",
        "a = \"\"\"\nline1\nline2\"\"\"\nb = 6.626e-34",
        "a = @ broken = [",
    ];
    for source in sources {
        let first = parse_events(source);
        let second = parse_events(source);
        assert_eq!(first, second, "determinism on {source:?}");
    }
}

#[test]
fn recovery_always_makes_progress() {
    // Pathological garbage: the document loop must terminate and every
    // error must consume bytes (or hit EOF cleanly).
    let source = "@ a @ b @\n= = =\n[[ ]] @@@";
    let (events, tokens, errors) = parse_events(source);
    assert!(errors >= 1, "garbage must be reported");
    assert!(
        events.iter().any(|e| matches!(e, Event::Error { .. })),
        "error events present"
    );
    // Progress: the emitted token ranges advance monotonically.
    let mut last = 0_u32;
    for (_, range) in &tokens {
        assert!(range.start >= last, "token ranges must not go back");
        last = range.end.max(last);
    }
}

#[test]
fn event_stream_is_balanced() {
    for (source, _) in CORPUS {
        let (events, _, _) = parse_events(source);
        let mut wrapped = Vec::with_capacity(events.len() + 2);
        wrapped.push(Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(events.iter().copied());
        wrapped.push(Event::Finish);
        assert!(process(&wrapped).is_ok(), "balanced stream on {source:?}");
    }
}

#[test]
fn steps_round_trip_through_the_packed_stream() {
    let (events, tokens, _) = parse_events("a = [1, { b = \"x\" }]\n[t]\nc = 2.5");
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(Event::Finish);
    let output = process(&wrapped).expect("packs");
    let token_steps = output
        .steps()
        .filter(|s| matches!(s, Step::Token { .. }))
        .count();
    assert_eq!(token_steps, tokens.len());
    assert!(token_steps > 5, "fixture exercises the families");
}

#[test]
fn error_regions_are_error_kind_nodes() {
    let (events, tokens, errors) = parse_events("a = @");
    assert_eq!(errors, 1);
    let enters: Vec<u16> = events
        .iter()
        .filter_map(|e| match e {
            Event::Start { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert!(enters.contains(&ERROR_KIND), "error region present");
    assert!(
        tokens
            .iter()
            .any(|(k, r)| *k == ERROR_KIND && !r.is_empty()),
        "offending bytes ride the error region"
    );
}

//! query integration tests (issue 031 TDD): parse/validate with spans,
//! matching on hand-built streams, quantifier semantics, predicate
//! invocation, capture dedup, highlight + symbol fixtures, and the
//! O(1)-alloc-per-candidate contract.

use std::ops::Range;

use query::{Capture, KindResolver, PredicateArg, QueryCursor, compile};
use runtime_core::events::{self, Event};

/// The JSON pack's node-kind resolver (the pack's dense table).
struct JsonKinds;
impl KindResolver for JsonKinds {
    fn resolve(&self, name: &str) -> Option<u16> {
        use packs_json::kinds as k;
        Some(match name {
            "object" => k::OBJECT,
            "array" => k::ARRAY,
            "pair" => k::PAIR,
            "string" => k::STRING,
            "number" => k::NUMBER,
            "true" => k::TRUE,
            "false" => k::FALSE,
            "null" => k::NULL,
            _ => return None,
        })
    }
}

/// Token kinds of the JSON pack (leaves).
mod jt {
    pub const LBRACE: u16 = 8;
    pub const COLON: u16 = 12;
    pub const COMMA: u16 = 13;
    pub const STRING: u16 = 3;
    pub const NUMBER: u16 = 4;
}

/// A hand-built stream: `{"a": 1}` — plus the parallel token ranges.
fn stream_object_a1() -> (
    runtime_core::events::Output,
    Vec<(u16, Range<u32>)>,
    &'static str,
) {
    let events = vec![
        Event::Start {
            kind: 0,
            forward_parent: None,
        }, // object
        Event::Token {
            kind: jt::LBRACE,
            n_raw: 1,
        },
        Event::Start {
            kind: 2,
            forward_parent: None,
        }, // pair
        Event::Token {
            kind: jt::STRING,
            n_raw: 1,
        }, // "a"
        Event::Token {
            kind: jt::COLON,
            n_raw: 1,
        },
        Event::Token {
            kind: jt::NUMBER,
            n_raw: 1,
        }, // 1
        Event::Finish,
        Event::Finish,
    ];
    let ranges = vec![
        (jt::LBRACE, 0..1),
        (jt::STRING, 1..4),
        (jt::COLON, 4..5),
        (jt::NUMBER, 6..7),
    ];
    (
        events::process(&events).expect("balanced"),
        ranges,
        r#"{"a": 1}"#,
    )
}

/// A hand-built stream: `{"a": 1, "b": true}` (two pairs).
fn stream_two_pairs() -> (
    runtime_core::events::Output,
    Vec<(u16, Range<u32>)>,
    &'static str,
) {
    let events = vec![
        Event::Start {
            kind: 0,
            forward_parent: None,
        },
        Event::Token {
            kind: jt::LBRACE,
            n_raw: 1,
        },
        Event::Start {
            kind: 2,
            forward_parent: None,
        },
        Event::Token {
            kind: jt::STRING,
            n_raw: 1,
        }, // "a"
        Event::Token {
            kind: jt::COLON,
            n_raw: 1,
        },
        Event::Token {
            kind: jt::NUMBER,
            n_raw: 1,
        }, // 1
        Event::Finish,
        Event::Token {
            kind: jt::COMMA,
            n_raw: 1,
        },
        Event::Start {
            kind: 2,
            forward_parent: None,
        },
        Event::Token {
            kind: jt::STRING,
            n_raw: 1,
        }, // "b"
        Event::Token {
            kind: jt::COLON,
            n_raw: 1,
        },
        Event::Token { kind: 5, n_raw: 1 }, // `true` (kind 5)
        Event::Finish,
        Event::Finish,
    ];
    let ranges = vec![
        (jt::LBRACE, 0..1),
        (jt::STRING, 1..4),
        (jt::COLON, 4..5),
        (jt::NUMBER, 6..7),
        (jt::COMMA, 7..8),
        (jt::STRING, 9..12),
        (jt::COLON, 12..13),
        (5_u16, 14..18), // `true`
    ];
    (
        events::process(&events).expect("balanced"),
        ranges,
        r#"{"a": 1, "b": true}"#,
    )
}

#[test]
fn parse_errors_carry_spans() {
    // Unknown kind: the span points at the name.
    let error = compile("(nope)", &JsonKinds).expect_err("unknown kind");
    assert_eq!(error.span, 1..5, "span over `nope`");
    assert!(error.message.contains("unknown kind"), "{error:?}");
    // Unterminated pattern.
    let error = compile("(pair", &JsonKinds).expect_err("unterminated");
    assert!(error.message.contains("unterminated"), "{error:?}");
    // Nested sub-patterns beyond one level are unsupported, with span.
    let error = compile("(pair (string (@x)))", &JsonKinds).expect_err("nested");
    assert!(error.message.contains("unsupported"), "{error:?}");
    // Empty query.
    let error = compile("   ", &JsonKinds).expect_err("empty");
    assert!(error.message.contains("empty"), "{error:?}");
}

#[test]
fn patterns_match_hand_built_streams() {
    let compiled = compile("(pair (string) (number))", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1, "the pair matches: {matches:?}");
    assert_eq!(matches[0].span, 1..7, "the pair spans its tokens");
}

#[test]
fn captures_bind_node_and_child_spans() {
    let compiled = compile("(pair (string) (number))", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1);
    assert!(matches[0].captures.is_empty(), "no captures requested");
    // With captures:
    let compiled = compile("(pair (string) @key (number) @val)", &JsonKinds).expect("valid");
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1);
    let captures = &matches[0].captures;
    assert_eq!(captures.len(), 2, "captures: {captures:?}");
    assert_eq!(
        captures[0],
        Capture {
            slot: 0,
            span: 1..4
        }
    );
    assert_eq!(
        captures[1],
        Capture {
            slot: 1,
            span: 6..7
        }
    );
}

#[test]
fn quantifier_semantics() {
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    // `+`: one or more pair children — the two-pair object matches.
    let compiled = compile("(object (pair)+)", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1, "two pairs satisfy (pair)+: {matches:?}");
    // `?`: zero or one — the two-pair object still matches.
    let compiled = compile("(object (pair)?)", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1, "a leading pair satisfies (pair)?");
    // One-pair stream also matches (pair)?.
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1);
    // `*`: zero or more — matches everything object-shaped.
    let compiled = compile("(object (pair)*)", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1);
    // Exactness: (object (pair) (pair)) needs exactly two — the
    // one-pair object does NOT match.
    let compiled = compile("(object (pair) (pair))", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert!(matches.is_empty(), "one pair cannot fill two slots");
}

#[test]
fn predicates_gate_matches() {
    let mut scratch = Vec::new();
    // #eq? on the key capture: only `"a"` matches.
    // `#eq?` compares the node's exact text — the string capture
    // includes its quotes (tree-sitter semantics).
    let compiled = compile(
        "(pair (string) @key (number)) (#eq? @key \"\\\"a\\\"\")",
        &JsonKinds,
    )
    .expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1, "the key IS \"a\" (quoted text)");
    // The same query over two pairs: `"a": 1` matches, `"b": true`
    // (a true value, not a number) does not.
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1, "only the \"a\" pair survives");
    assert_eq!(matches[0].span, 1..7, "the first pair's span");
    // A custom closure predicate runs per candidate.
    let compiled = compile("(pair) (#always-false? \"x\")", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut called = 0_usize;
    let mut predicate = |name: &str, args: &[PredicateArg<'_>]| {
        assert_eq!(name, "always-false");
        assert!(matches!(args.first(), Some(PredicateArg::Text("x"))));
        called += 1;
        false
    };
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert!(matches.is_empty(), "the closure dropped the match");
    assert_eq!(called, 1, "the closure ran once per candidate");
}

#[test]
fn captures_dedup_across_and_within_matches() {
    let compiled = compile("(pair (string) @k) (#eq? @k \"\\\"a\\\"\")", &JsonKinds).expect("v");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 1);
    let spans: Vec<Range<usize>> = matches[0].captures.iter().map(|c| c.span.clone()).collect();
    let mut sorted = spans.clone();
    sorted.sort_by_key(|span| (span.start, span.end));
    assert_eq!(sorted, spans, "no duplicates within a match");
    // Re-running the same stream through the SAME cursor yields
    // nothing new — (slot, span) pairs dedup globally.
    let matches_again = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert!(matches_again.is_empty(), "dedup across matches");
}

/// The highlight fixture: property/number/keyword captures over the
/// two-pair stream.
#[test]
fn highlight_fixture() {
    let compiled = compile(
        "(pair (string) @property (number) @number) (pair (string) @property (true) @keyword)",
        &JsonKinds,
    )
    .expect("valid");
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    assert_eq!(matches.len(), 2, "one match per pair: {matches:?}");
    let mut highlights: Vec<(Range<usize>, u8)> = Vec::new();
    for matched in &matches {
        for capture in &matched.captures {
            highlights.push((capture.span.clone(), capture.slot));
        }
    }
    highlights.sort_by_key(|(span, _)| (span.start, span.end));
    // "a" → @property, 1 → @number, "b" → @property, true → @keyword.
    assert_eq!(highlights.len(), 4, "{highlights:?}");
    assert_eq!(highlights[0], (1..4, 0), "\"a\" as @property");
    assert_eq!(highlights[1], (6..7, 1), "1 as @number");
    assert_eq!(highlights[2], (9..12, 0), "\"b\" as @property");
    assert_eq!(highlights[3], (14..18, 2), "true as @keyword");
}

/// The symbol fixture: pair keys as document symbols.
#[test]
fn symbol_fixture() {
    let compiled = compile("(pair (string) @name)", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    let names: Vec<&str> = matches
        .iter()
        .flat_map(|m| m.captures.iter())
        .map(|c| &source[c.span.clone()])
        .collect();
    assert_eq!(names, [r#""a""#, r#""b""#], "symbols in source order");
}

/// The O(1)-alloc contract: the scratch buffer's capacity never grows
/// past the warm-up bound, however many candidates stream by.
#[test]
fn per_candidate_alloc_is_bounded() {
    let compiled = compile("(object (pair)*)", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_two_pairs();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    // Warm-up: the capacity reaches its stable bound.
    for _ in 0..4 {
        cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    }
    let warm_capacity = scratch.capacity();
    // 1000 candidates later: identical capacity — no reallocation.
    for _ in 0..1000 {
        cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    }
    assert_eq!(
        scratch.capacity(),
        warm_capacity,
        "O(1) alloc per candidate step"
    );
}

/// Wildcard roots match any kind.
#[test]
fn wildcard_roots_match_anything() {
    let compiled = compile("(_ (string))", &JsonKinds).expect("valid");
    let (output, ranges, source) = stream_object_a1();
    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);
    // The pair has a string child; the object does not.
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].span, 1..7);
}

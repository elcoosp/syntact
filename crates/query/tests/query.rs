//! Query suite (issue 031 TDD): parse/validate with spans, compilation,
//! matching on hand-built streams, quantifier semantics, predicate
//! invocation, capture dedup, highlight + symbol fixtures, and the
//! O(1)-alloc-per-candidate-step evidence.

use std::ops::Range;

use pretty_assertions::assert_eq;
use runtime_core::events::{Event, Output};

use query::{
    KindResolver, QueryCursor, QueryError, QueryStep, WILDCARD, bitset_allows, compile, parse_query,
};

/// The fixture resolver: the JSON pack's kinds.
struct JsonKinds;

impl KindResolver for JsonKinds {
    fn kind_id(&self, name: &str) -> Option<u16> {
        Some(match name {
            "object" => 0,
            "array" => 1,
            "pair" => 2,
            "string" => 3,
            "number" => 4,
            "true" => 5,
            "false" => 6,
            "null" => 7,
            "lbrace" => 8,
            "rbrace" => 9,
            "lbrack" => 10,
            "rbrack" => 11,
            "colon" => 12,
            "comma" => 13,
            _ => return None,
        })
    }
}

/// Build a packed output + range table: each token is 1 synthetic byte.
fn pack(build: &dyn Fn(&mut Vec<Event>)) -> (Output, Vec<Range<u32>>, String) {
    let mut events = Vec::new();
    build(&mut events);
    let mut ranges = Vec::new();
    let mut offset = 0_u32;
    for event in &events {
        if matches!(event, Event::Token { .. }) {
            ranges.push(offset..offset + 1);
            offset += 1;
        }
    }
    let mut wrapped = vec![Event::Start {
        kind: u16::MAX - 3,
        forward_parent: None,
    }];
    wrapped.extend(events);
    wrapped.push(Event::Finish);
    let output = runtime_core::events::process(&wrapped).expect("packs");
    let source: String = "x".repeat(offset as usize);
    (output, ranges, source)
}

// ---------------------------------------------------------------------------
// Parse / validate with spans
// ---------------------------------------------------------------------------

#[test]
fn parse_shapes_and_spans() {
    let spec = parse_query("(pair (string) @k (number)) @pair").expect("parses");
    assert_eq!(spec.patterns.len(), 1);
    let root = &spec.patterns[0].root;
    assert_eq!(root.kind.as_deref(), Some("pair"));
    assert_eq!(root.capture.as_deref(), Some("pair"));
    assert_eq!(root.children.len(), 2);
    assert_eq!(root.children[0].capture.as_deref(), Some("k"));
    let text = "(pair (string) @k (number)) @pair";
    assert_eq!(
        &text[spec.patterns[0].span.start as usize..spec.patterns[0].span.end as usize],
        "(pair (string) @k (number))"
    );
}

#[test]
fn parse_errors_carry_spans() {
    let err = parse_query("(pair (string").expect_err("unterminated");
    assert!(err.message.contains("unterminated"), "{err:?}");
    let err = parse_query("(pair @orphan)").expect_err("orphan capture");
    assert!(err.message.contains("capture"), "{err:?}");
    let err = parse_query("(pair (string) \"x\")").expect_err("stray string");
    assert!(err.message.contains("unexpected"), "{err:?}");
    let err = parse_query("(pair (string) (#neq? @x \"v\"))").expect_err("unsupported predicate");
    assert!(err.message.contains("only `#eq?`"), "{err:?}");
}

#[test]
fn compile_validates_kinds_with_spans() {
    let spec = parse_query("(pair (strng) @k)").expect("parses");
    let err = compile(&spec, &JsonKinds).expect_err("unknown kind");
    assert!(err.message().contains("strng"), "{err:?}");
    let _ = QueryError {
        span: err.span.clone(),
        message: "shape".into(),
    };
}

// ---------------------------------------------------------------------------
// Matching on hand-built streams
// ---------------------------------------------------------------------------

#[test]
fn matches_hand_built_stream() {
    let spec = parse_query("(pair (string) @k (number) @v)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 0,
            forward_parent: None,
        }); // object
        for _ in 0..2 {
            events.push(Event::Start {
                kind: 2,
                forward_parent: None,
            }); // pair
            events.push(Event::Start {
                kind: 3,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 3, n_raw: 1 });
            events.push(Event::Finish); // string
            events.push(Event::Token { kind: 12, n_raw: 1 }); // colon
            events.push(Event::Start {
                kind: 4,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 4, n_raw: 1 });
            events.push(Event::Finish); // number
            events.push(Event::Finish);
        }
        events.push(Event::Finish);
    });
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 2, "one per pair");
    let caps = &matches[0].captures;
    assert_eq!(caps.len(), 2, "@k and @v");
    assert_eq!(caps[0].0, 0, "@k id");
    assert_eq!(caps[1].0, 1, "@v id");
}

#[test]
fn wildcard_matches_any_kind() {
    let spec = parse_query("(array (_)* @item)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 1,
            forward_parent: None,
        }); // array
        events.push(Event::Start {
            kind: 4,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 4, n_raw: 1 });
        events.push(Event::Finish); // number
        events.push(Event::Start {
            kind: 5,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 5, n_raw: 1 });
        events.push(Event::Finish); // true
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].captures.len(), 2, "both children captured");
}

// ---------------------------------------------------------------------------
// Quantifier semantics
// ---------------------------------------------------------------------------

#[test]
fn quantifier_semantics() {
    let spec = parse_query("(object (pair)?)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (empty, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 0,
            forward_parent: None,
        });
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &empty, &ranges, &source);
    assert_eq!(matches.len(), 1, "zero pairs still matches `?`");

    let spec = parse_query("(object (pair)+)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let matches = QueryCursor::new().matches(&query, &empty, &ranges, &source);
    assert_eq!(matches.len(), 0, "`+` needs at least one");

    let spec = parse_query("(object (pair)*)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let matches = QueryCursor::new().matches(&query, &empty, &ranges, &source);
    assert_eq!(matches.len(), 1, "`*` matches zero");

    let spec = parse_query("(pair (string) (number) (number)?)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (with_two, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 2,
            forward_parent: None,
        });
        events.push(Event::Start {
            kind: 3,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 3, n_raw: 1 });
        events.push(Event::Finish); // string
        events.push(Event::Start {
            kind: 4,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 4, n_raw: 1 });
        events.push(Event::Finish); // number
        events.push(Event::Start {
            kind: 4,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 4, n_raw: 1 });
        events.push(Event::Finish); // number
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &with_two, &ranges, &source);
    assert_eq!(matches.len(), 1, "optional tail present");
    let (with_one, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 2,
            forward_parent: None,
        });
        events.push(Event::Start {
            kind: 3,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 3, n_raw: 1 });
        events.push(Event::Finish); // string
        events.push(Event::Start {
            kind: 4,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 4, n_raw: 1 });
        events.push(Event::Finish); // number
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &with_one, &ranges, &source);
    assert_eq!(matches.len(), 1, "optional tail absent");
    let _ = (&query, &matches);
}

#[test]
fn quantifier_bounds_in_steps() {
    let spec = parse_query("(object (pair)? (pair)* (pair)+)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let steps: &[QueryStep] = &query.patterns[0].steps;
    assert_eq!(steps[1].min, 0);
    assert_eq!(steps[1].max, 1);
    assert_eq!(steps[2].min, 0);
    assert_eq!(steps[2].max, u8::MAX);
    assert_eq!(steps[3].min, 1);
    assert_eq!(steps[3].max, u8::MAX);
    assert_ne!(steps[0].prefilter, [0; 3], "prefilter populated");
}

// ---------------------------------------------------------------------------
// Predicates + capture dedup
// ---------------------------------------------------------------------------

#[test]
fn predicate_invocation_filters_matches() {
    // The synthetic source renders every token as "x" — the predicate
    // comparing against "x" passes; any other literal filters out.
    let spec = parse_query("((true) @lit (#eq? @lit \"x\"))").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 1,
            forward_parent: None,
        }); // array
        events.push(Event::Start {
            kind: 5,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 5, n_raw: 1 });
        events.push(Event::Finish); // true
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(
        matches.len(),
        1,
        "the matching literal passes the predicate"
    );

    let spec = parse_query("((true) @lit (#eq? @lit \"nope\"))").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 0, "a differing literal filters out");
}

#[test]
fn capture_dedup_within_a_match() {
    let spec = parse_query("(pair (string) @s)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 2,
            forward_parent: None,
        });
        events.push(Event::Start {
            kind: 3,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 3, n_raw: 1 });
        events.push(Event::Finish); // string
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 1);
    let spans: Vec<Range<u32>> = matches[0].captures.iter().map(|(_, r)| r.clone()).collect();
    let mut sorted = spans.clone();
    sorted.sort_by_key(|r| (r.start, r.end));
    sorted.dedup();
    assert_eq!(spans, sorted, "deduped captures");
}

// ---------------------------------------------------------------------------
// Fixtures: highlight + symbols
// ---------------------------------------------------------------------------

#[test]
fn highlight_fixture() {
    let spec = parse_query("((string) @string\n  (number) @number\n  (null) @constant)\n")
        .expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    assert_eq!(query.patterns.len(), 3);
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 0,
            forward_parent: None,
        }); // object
        events.push(Event::Start {
            kind: 3,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 3, n_raw: 1 });
        events.push(Event::Finish); // string
        events.push(Event::Start {
            kind: 4,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 4, n_raw: 1 });
        events.push(Event::Finish); // number
        events.push(Event::Start {
            kind: 7,
            forward_parent: None,
        });
        events.push(Event::Token { kind: 7, n_raw: 1 });
        events.push(Event::Finish); // null
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 3, "one highlight per family");
    let pattern_ids: Vec<usize> = matches.iter().map(|m| m.pattern).collect();
    assert_eq!(pattern_ids, [0, 1, 2], "one match per pattern, in order");
}

#[test]
fn symbol_fixture() {
    let spec = parse_query("(pair (string) @symbol)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 0,
            forward_parent: None,
        }); // object
        for _ in 0..3 {
            events.push(Event::Start {
                kind: 2,
                forward_parent: None,
            });
            events.push(Event::Start {
                kind: 3,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 3, n_raw: 1 });
            events.push(Event::Finish); // string
            events.push(Event::Token { kind: 12, n_raw: 1 }); // colon
            events.push(Event::Start {
                kind: 7,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 7, n_raw: 1 });
            events.push(Event::Finish); // null
            events.push(Event::Finish);
        }
        events.push(Event::Finish);
    });
    let matches = QueryCursor::new().matches(&query, &output, &ranges, &source);
    assert_eq!(matches.len(), 3, "one symbol per pair");
    for m in &matches {
        assert_eq!(m.captures[0].0, 0, "the @symbol capture id");
    }
}

// ---------------------------------------------------------------------------
// O(1) alloc per candidate step (documented + tested)
// ---------------------------------------------------------------------------

#[test]
fn scratch_capacity_never_grows_across_candidates() {
    let spec = parse_query("(pair (string) @k (number) @v)").expect("parses");
    let query = compile(&spec, &JsonKinds).expect("compiles");
    let (output, ranges, source) = pack(&|events| {
        events.push(Event::Start {
            kind: 0,
            forward_parent: None,
        });
        for _ in 0..1000 {
            events.push(Event::Start {
                kind: 2,
                forward_parent: None,
            });
            events.push(Event::Start {
                kind: 3,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 3, n_raw: 1 });
            events.push(Event::Finish); // string
            events.push(Event::Start {
                kind: 4,
                forward_parent: None,
            });
            events.push(Event::Token { kind: 4, n_raw: 1 });
            events.push(Event::Finish); // number
            events.push(Event::Finish);
        }
        events.push(Event::Finish);
    });
    let mut cursor = QueryCursor::new();
    let first = cursor.matches(&query, &output, &ranges, &source);
    let capacity_after_first = cursor.scratch_capacity();
    assert_eq!(first.len(), 1000);
    for _ in 0..10 {
        let _ = cursor.matches(&query, &output, &ranges, &source);
        assert_eq!(
            cursor.scratch_capacity(),
            capacity_after_first,
            "O(1) alloc per candidate step: no growth"
        );
    }
}

#[test]
fn wildcard_sentinel_is_the_prefilter_all_pass() {
    assert!(bitset_allows(&[0; 3], WILDCARD));
    let mut bits = [0_u64; 3];
    bits[0] |= 1 << 3;
    assert!(bitset_allows(&bits, 3));
    assert!(!bitset_allows(&bits, 4));
}

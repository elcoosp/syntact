// Recipe: find nodes with the query compiler + streaming cursor.
//
// A query is compiled once against a pack's kind table
// (`KindResolver`) and then matched over any packed event stream —
// here a hand-built `{"a": 1}` object. Captures bind child spans.

use query::{Capture, KindResolver, PredicateArg, QueryCursor, compile};
use runtime_core::events::{self, Event, Output};
use std::ops::Range;

use packs_json::kinds as jk;
use packs_json::reference::token_kinds as jt;

/// The JSON pack's node-kind resolver: query kind names → dense ids.
struct JsonKinds;
impl KindResolver for JsonKinds {
    fn resolve(&self, name: &str) -> Option<u16> {
        Some(match name {
            "object" => jk::OBJECT,
            "array" => jk::ARRAY,
            "pair" => jk::PAIR,
            "string" => jk::STRING,
            "number" => jk::NUMBER,
            "true" => jk::TRUE,
            "false" => jk::FALSE,
            "null" => jk::NULL,
            _ => return None,
        })
    }
}

/// A hand-built stream for `{"a": 1}`: the packed events plus the
/// parallel `(kind, range)` token table `QueryCursor` reads.
fn stream_object_a1() -> (Output, Vec<(u16, Range<u32>)>, &'static str) {
    let events = vec![
        Event::Start {
            kind: jk::OBJECT,
            forward_parent: None,
        },
        Event::Token {
            kind: jt::LBRACE,
            n_raw: 1,
        },
        Event::Start {
            kind: jk::PAIR,
            forward_parent: None,
        },
        Event::Token {
            kind: jk::STRING,
            n_raw: 1,
        }, // "a"
        Event::Token {
            kind: jt::COLON,
            n_raw: 1,
        },
        Event::Token {
            kind: jk::NUMBER,
            n_raw: 1,
        }, // 1
        Event::Finish,
        Event::Finish,
    ];
    let ranges = vec![
        (jt::LBRACE, 0..1),
        (jk::STRING, 1..4),
        (jt::COLON, 4..5),
        (jk::NUMBER, 6..7),
    ];
    (
        events::process(&events).expect("balanced"),
        ranges,
        r#"{"a": 1}"#,
    )
}

#[test]
fn find_pairs_and_capture_their_children() {
    let compiled = compile("(pair (string) @key (number) @val)", &JsonKinds)
        .expect("query compiles against the resolver");
    let (output, ranges, source) = stream_object_a1();

    let mut cursor = QueryCursor::new(&compiled);
    let mut scratch = Vec::new();
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let matches = cursor.matches(&output, &ranges, source, &mut scratch, &mut predicate);

    // The pair matched; `@key`/`@val` bound the child token spans.
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].span, 1..7, "the pair spans its tokens");
    assert_eq!(
        matches[0].captures,
        vec![
            Capture {
                slot: 0,
                span: 1..4
            },
            Capture {
                slot: 1,
                span: 6..7
            },
        ]
    );
}

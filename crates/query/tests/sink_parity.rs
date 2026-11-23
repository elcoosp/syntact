//! `QuerySink` parity (M1 seam): the push-based sink over
//! [`runtime_core::sink::Sink`] must produce exactly the matches the
//! pull-based [`QueryCursor`] produces over the same packed stream —
//! same spans, same captures, same dedup, same predicate behavior —
//! on valid streams, malformed streams, and randomized streams.

use std::cell::Cell;
use std::ops::Range;

use query::{KindResolver, Match, PredicateArg, QueryCursor, QuerySink, compile};
use runtime_core::events::{self, Event, Output, Step};
use runtime_core::sink::{TeeSink, ValidationSink, drive_sink};

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

/// Parse `source` with the JSON pack and wrap it into the ROOT-framed
/// `Output` plus the parallel `(kind, range)` token table.
fn json_output(source: &str) -> (Output, Vec<(u16, Range<u32>)>) {
    let (events, ranges, _errors) = packs_json::reference::parse_events(source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped).expect("pack stream validates");
    (output, ranges)
}

/// Run the pull-based cursor over `output`.
fn pull_matches(
    compiled: &query::CompiledQuery,
    output: &Output,
    ranges: &[(u16, Range<u32>)],
    source: &str,
    accepted: &Cell<usize>,
) -> Vec<Match> {
    let mut cursor = QueryCursor::new(compiled);
    let mut scratch = Vec::new();
    let mut predicate = |name: &str, args: &[PredicateArg<'_>]| {
        accepted.set(accepted.get() + 1);
        if name == "non-empty" {
            return args
                .first()
                .is_some_and(|arg| matches!(arg, PredicateArg::Capture(text) if !text.is_empty()));
        }
        true
    };
    cursor.matches(output, ranges, source, &mut scratch, &mut predicate)
}

/// Run the push-based sink over `output` through `drive_sink`.
fn push_matches(
    compiled: &query::CompiledQuery,
    output: &Output,
    ranges: &[(u16, Range<u32>)],
    source: &str,
    accepted: &Cell<usize>,
) -> Vec<Match> {
    let mut predicate = |name: &str, args: &[PredicateArg<'_>]| {
        accepted.set(accepted.get() + 1);
        if name == "non-empty" {
            return args
                .first()
                .is_some_and(|arg| matches!(arg, PredicateArg::Capture(text) if !text.is_empty()));
        }
        true
    };
    let mut sink = QuerySink::new(compiled, source, ranges, &mut predicate);
    let plain: Vec<Range<u32>> = ranges.iter().map(|(_, range)| range.clone()).collect();
    drive_sink(output, &plain, &mut sink).expect("drive never fails on this stream");
    sink.into_matches()
}

/// Both match paths agree on the full corpus: valid documents, malformed
/// documents, and three query shapes (captures, quantifiers, predicates).
#[test]
fn sink_matches_cursor_on_valid_and_malformed_json() {
    let sources = [
        r#"{"a": 1, "b": [2, {"c": null}], "d": "str"}"#,
        r#"{"a": }"#,
        r"[1, 2",
        r#"{"x": "}", "y": "{"}"#,
        r#"{"nested": {"deep": [true, false, null]}}"#,
        "",
    ];
    let queries = [
        "(pair (string) @key (number) @val)",
        "(object (pair)+)",
        "(string)",
        "(array (number) @n*)",
        "(pair (string) @key) (#eq? @key \"a\")",
        "(pair (string) @key) (#non-empty? @key)",
    ];
    for source in sources {
        let (output, ranges) = json_output(source);
        for text in queries {
            let compiled = compile(text, &JsonKinds).expect("valid query");
            let pull = pull_matches(&compiled, &output, &ranges, source, &Cell::new(0));
            let push = push_matches(&compiled, &output, &ranges, source, &Cell::new(0));
            assert_eq!(pull, push, "parity for `{text}` on {source:?}");
        }
    }
}

/// Predicate dispatch order is part of the contract: the sink must call
/// the host closure exactly as often as the cursor does (DFS completion
/// order, once per surviving candidate).
#[test]
fn predicate_call_counts_match() {
    let source = r#"{"a": 1, "b": "two", "c": 3}"#;
    let (output, ranges) = json_output(source);
    let compiled =
        compile("(pair (string) @key) (#non-empty? @key)", &JsonKinds).expect("valid query");
    let pull_calls = Cell::new(0);
    let pull = pull_matches(&compiled, &output, &ranges, source, &pull_calls);
    let push_calls = Cell::new(0);
    let push = push_matches(&compiled, &output, &ranges, source, &push_calls);
    assert_eq!(pull, push);
    assert_eq!(pull_calls.get(), push_calls.get(), "same dispatch count");
}

/// Malformed hand-packed streams (exit underflow, stray tokens, dangling
/// enters) must drive the sink without panicking and without producing
/// matches beyond what the cursor sees.
#[test]
fn sink_matches_cursor_on_malformed_hand_packed_stream() {
    // Exit underflow + a stray token outside any node + unclosed enter.
    let steps = [
        Step::Exit,
        Step::Token { kind: 1, n_raw: 1 },
        Step::Enter { kind: 2 },
        Step::Token { kind: 3, n_raw: 2 },
        Step::Exit,
        Step::Exit,
        Step::Enter { kind: 4 },
    ];
    let mut stream = Vec::new();
    for step in &steps {
        stream.push(match *step {
            Step::Enter { kind } => Output::pack_enter(kind),
            Step::Exit => Output::pack_exit(),
            Step::Token { kind, n_raw } => Output::pack_token(kind, n_raw),
            Step::Error { diag } => Output::pack_error(diag),
        });
    }
    let output = Output { stream };
    let ranges = vec![(1_u16, 0_u32..4), (3_u16, 4_u32..9)];
    let source = "abcdefghij";
    let compiled = compile("(_)", &JsonKinds).expect("wildcard query");
    let pull = pull_matches(&compiled, &output, &ranges, source, &Cell::new(0));
    let push = push_matches(&compiled, &output, &ranges, source, &Cell::new(0));
    assert_eq!(pull, push, "malformed stream parity");
}

/// One pass, two consumers: a [`TeeSink`] drives the tree-side validation
/// sink and the query sink simultaneously — the §7.2 "one stream, many
/// sinks" contract.
#[test]
fn tee_sink_serves_validation_and_query_in_one_pass() {
    let source = r#"{"a": 1, "b": 2}"#;
    let (output, ranges) = json_output(source);
    let compiled = compile("(pair (string) @key (number) @val)", &JsonKinds).expect("valid");
    let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
    let query_sink = QuerySink::new(&compiled, source, &ranges, &mut predicate);
    let validation = ValidationSink::default();
    let plain: Vec<Range<u32>> = ranges.iter().map(|(_, range)| range.clone()).collect();
    let mut tee = TeeSink::new(validation, query_sink);
    drive_sink(&output, &plain, &mut tee).expect("drives");
    // The validation half saw the whole stream.
    let summary = tee.first.summary();
    assert!(summary.balanced);
    assert!(summary.tokens >= 4, "tokens counted: {summary:?}");
    // The query half matched both pairs.
    let matches = tee.second.into_matches();
    assert_eq!(matches.len(), 2, "both pairs matched: {matches:?}");
    let texts: Vec<&str> = matches
        .iter()
        .filter_map(|m| m.captures.first())
        .map(|c| &source[c.span.clone()])
        .collect();
    assert_eq!(texts, vec!["\"a\"", "\"b\""]);
}

/// Deterministic LCG — reproducible without an external RNG (same shape
/// as the runtime-core sink property tests).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) % n.max(1)
    }
}

/// Randomized streams (valid and malformed) and parallel random range
/// tables: pull and push must agree match-for-match on every round.
#[test]
fn property_sink_parity_on_random_streams() {
    let mut rng = Lcg(0x0246_8ACE_1357_9BDF);
    let source = "the quick brown fox jumps over the lazy dog and then some";
    let compiled = compile("(_ (_ ) @any*)", &JsonKinds).expect("wildcard query");
    for _ in 0..300 {
        let mut steps = Vec::new();
        let mut ranges: Vec<(u16, Range<u32>)> = Vec::new();
        let mut cursor_pos = 0_u32;
        for _ in 0..6 + rng.below(40) {
            match rng.below(6) {
                0 | 1 => {
                    let kind = u16::try_from(rng.below(50)).expect("kind fits u16");
                    steps.push(Step::Enter { kind });
                }
                2 => {
                    steps.push(Step::Exit);
                }
                3 | 4 => {
                    let kind = u16::try_from(rng.below(20)).expect("kind fits u16");
                    let n_raw = u8::try_from(1 + rng.below(3)).expect("n_raw fits u8");
                    steps.push(Step::Token { kind, n_raw });
                    let start = cursor_pos + u32::try_from(rng.below(3)).unwrap_or(0);
                    let end = start + u32::try_from(rng.below(10)).unwrap_or(0);
                    cursor_pos = end;
                    ranges.push((kind, start..end));
                }
                _ => {
                    let diag = u32::try_from(rng.below(8)).expect("diag fits u32");
                    steps.push(Step::Error { diag });
                }
            }
        }
        let mut stream = Vec::with_capacity(steps.len());
        for step in &steps {
            stream.push(match *step {
                Step::Enter { kind } => Output::pack_enter(kind),
                Step::Exit => Output::pack_exit(),
                Step::Token { kind, n_raw } => Output::pack_token(kind, n_raw),
                Step::Error { diag } => Output::pack_error(diag),
            });
        }
        let output = Output { stream };
        let pull = pull_matches(&compiled, &output, &ranges, source, &Cell::new(0));
        let push = push_matches(&compiled, &output, &ranges, source, &Cell::new(0));
        assert_eq!(pull, push, "property parity on random stream");
    }
}

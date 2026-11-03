//! packs-json test suite (issues 019/021): shape, losslessness, recovery,
//! determinism, and a lossless-coverage property over mutated documents.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::ERROR_KIND;
use runtime_core::events::{Output, Step, process};
use runtime_core::input::Source;
use runtime_core::sink::{TokenSink, ValidationSink, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

use crate::kinds;
use crate::reference::{self, ROOT_KIND, error_message, parse_events, token_kinds};

/// Embedded corpus: valid JSON across all families plus malformed inputs
/// with per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    ("{}", 0),
    ("[]", 0),
    (r#"{"a": 1}"#, 0),
    ("[1, 2, 3]", 0),
    (
        r#"{"name": "syntact", "version": 1, "ok": true, "bits": [true, false, null]}"#,
        0,
    ),
    ("3.14e-2", 0),
    ("-17", 0),
    (r#""str with \u00e9 escape""#, 0),
    ("  [ 1 , 2 ]  ", 0),
    (r#"{"a": [1, 2, {"b": null}], "c": true}"#, 0),
    ("{\"a\": 1,}", 0), // trailing comma: accepted silently by recovery
    ("{\"a\": }", 2),   // missing value: error region eats the stray closer
    ("[1, 2", 1),       // unterminated array
    ("1 2", 0),         // sibling roots (scalar documents)
    ("tru", 1),         // bad literal
    ("", 0),            // empty document (empty ROOT)
    ("@#$", 1),         // garbage document
];

/// Build the green tree through the full `parse` integration.
fn tree_for(source: &str) -> Arc<GreenNode> {
    crate::parse(source).expect("tree integration never fails structurally")
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
            "losslessness holds through recovery for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert!(
            errors <= *tolerated,
            "{source:?}: {errors} errors > tolerated {tolerated}"
        );
    }
}

#[test]
fn roots_are_wrapped_in_implicit_root() {
    let tree = tree_for("{\"a\": 1}");
    assert_eq!(tree.kind, ROOT_KIND);
    // node accounting: ROOT + OBJECT + PAIR (validate counts the inner ones).
    let (events, _, _) = parse_events("{\"a\": 1}");
    let output = process(&events).expect("balanced");
    let mut validation = ValidationSink::default();
    drive_sink(&output, &[], &mut validation).expect("drives");
    assert_eq!(
        tree.node_count(),
        usize::try_from(validation.summary().nodes).expect("nodes fit usize") + 1,
        "tree nodes == validate nodes + implicit ROOT"
    );
}

#[test]
fn ranges_are_clean_and_ascending() {
    for (source, _) in CORPUS {
        let (_, tagged, _) = parse_events(source);
        let ranges: Vec<Range<u32>> = tagged.iter().map(|(_, r)| r.clone()).collect();
        // Ranges ascend without overlapping (clean tokens; the trivia gaps
        // are covered by the parser's records and asserted via losslessness).
        for pair in ranges.windows(2) {
            assert!(
                pair[0].start <= pair[1].start,
                "ranges ascend for {source:?}"
            );
        }
    }
}

#[test]
fn scan_tokens_match_full_parse_kinds() {
    for (source, _) in CORPUS {
        let (events, tagged, _) = parse_events(source);
        let output = process(&events).expect("balanced stream");
        let ranges: Vec<Range<u32>> = tagged.iter().map(|(_, r)| r.clone()).collect();
        let mut scan = TokenSink::default();
        drive_sink(&output, &ranges, &mut scan).expect("drives");
        assert_eq!(scan.tokens, tagged, "scan == tagged ranges for {source:?}");
    }
}

#[test]
fn determinism_same_input_same_stream() {
    for (source, _) in CORPUS {
        let a = parse_events(source);
        let b = parse_events(source);
        assert_eq!(a.0, b.0, "events deterministic for {source:?}");
        assert_eq!(a.1, b.1, "ranges deterministic for {source:?}");
        assert_eq!(a.2, b.2, "error count deterministic for {source:?}");
    }
}

#[test]
fn error_regions_use_error_kinds() {
    let (events, tagged, errors) = parse_events("tru");
    assert_eq!(errors, 1);
    let output = process(&events).expect("balanced with recovery");
    let has_error_node = output
        .steps()
        .any(|s| matches!(s, Step::Enter { kind } if kind == ERROR_KIND));
    assert!(has_error_node, "recovery emits an ERROR node");
    assert!(
        tagged.iter().any(|(kind, _)| *kind == ERROR_KIND),
        "the offending region rides an ERROR-kind token"
    );
}

#[test]
fn recovery_progress_on_garbage_terminates() {
    // Every mutated document must parse without hanging and stay lossless.
    let seeds = ["[1, 2, 3]", "{\"k\": [true]}"];
    for seed in seeds {
        for mutation in 0..seed.len() {
            let mut bytes = seed.as_bytes().to_vec();
            bytes[mutation] = b'!';
            let mutated = String::from_utf8_lossy(&bytes).into_owned();
            let tree = tree_for(&mutated);
            assert_eq!(
                tree.render(&Source::from_str_unchecked(&mutated)),
                mutated,
                "lossless under mutation {mutation} of {seed:?}"
            );
        }
    }
}

#[test]
fn property_fuzz_lossless_coverage() {
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
    let mut rng = Lcg(0x05EE_D5EE_D5EE_0001);
    let alphabet: &[&str] = &["{", "}", "[", "]", ":", ",", "1", "\"a\"", "true", " ", "x"];
    for _ in 0..200 {
        let mut doc = String::new();
        for _ in 0..rng.below(12) {
            let pick = usize::try_from(rng.below(alphabet.len() as u64)).unwrap_or(0);
            doc.push_str(alphabet[pick]);
        }
        let tree = tree_for(&doc);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(&doc)),
            doc,
            "fuzz losslessness for {doc:?}"
        );
    }
}

#[test]
fn error_message_resolves_the_side_table() {
    assert_eq!(error_message(reference::DIAG_SYNTAX), "syntax error");
    assert_eq!(error_message(999), "unknown diagnostic");
}

#[test]
fn token_kinds_are_dense_after_value_kinds() {
    assert_eq!(token_kinds::LBRACE, 8);
    assert_eq!(token_kinds::WHITESPACE, 14);
    let ordered = [kinds::NULL, token_kinds::LBRACE, token_kinds::WHITESPACE];
    assert!(
        ordered.windows(2).all(|w| w[0] < w[1]),
        "dense continuation stays ordered"
    );
}

#[test]
fn tree_integration_end_to_end_matrix() {
    // The pipeline the bench exercises: events → process → TreeBuilder.
    for (source, _) in CORPUS {
        let (events, tagged, _) = parse_events(source);
        let mut wrapped = Vec::with_capacity(events.len() + 2);
        wrapped.push(runtime_core::events::Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(events.iter().copied());
        wrapped.push(runtime_core::events::Event::Finish);
        let output = process(&wrapped).expect("wrapped stream balances");
        let src = Source::from_str_unchecked(source);
        #[allow(clippy::arc_with_non_send_sync)]
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &src);
        let ranges: Vec<Range<u32>> = tagged.iter().map(|(_, r)| r.clone()).collect();
        drive_sink(&output, &ranges, &mut builder).expect("drives");
        let tree = builder.finish().expect("single root");
        assert_eq!(tree.kind, ROOT_KIND);
    }
}

#[test]
fn output_packing_helper_shape() {
    // The Step-shaped helper used across the workspace packs/tests.
    let output = Output {
        stream: vec![Output::pack_enter(kinds::OBJECT), Output::pack_exit()],
    };
    let steps: Vec<Step> = output.steps().collect();
    assert_eq!(steps, vec![Step::Enter { kind: 0 }, Step::Exit]);
}

//! The viewport-parsing gates (roadmap §8 API sketch `parse_partial`,
//! the lezer `stopAt` analogue of §2.8, and the §12.3 P3 gate "partial
//! parse works"): "Partial parsing (`PartialParse::stopAt(pos)`):
//! viewport parsing — parse the visible region first."
//!
//! The suites pin the protocol-level contract over the JSON pack:
//!
//! - cutting the raw event stream at a visible-end offset yields a
//!   well-formed stream (every `Start` closed) whose tree renders the
//!   covered source prefix byte-for-byte, with `resume_offset` pointing
//!   at the first token beyond the viewport;
//! - a viewport spanning the whole document reproduces the fresh parse
//!   exactly (tree equality with the pack's `parse`) — partial parsing
//!   never diverges from the full pipeline;
//! - a viewport ending inside a token resumes at that token's start;
//! - a cut never splits a forward-parent chain (the §2.3 precede wrap):
//!   when the chain's target lies beyond the natural cut, the cut
//!   extends so the stream stays `process()`able;
//! - trivia inside the covered prefix is sliced in — the prefix stays
//!   byte-faithful including whitespace.

use packs_json::reference::{ROOT_KIND, parse_events, token_kinds};
use runtime_core::input::Source;
use runtime_core::partial::{build, cut_events};
use runtime_core::sink::TriviaRecord;

/// A no-whitespace document (render assertions stay pure): whitespace
/// rides trivia records (ADR-008), never token events, so a doc without
/// it needs no trivia input to stay byte-faithful.
const DOC: &str = r#"{"a":[1,true],"b":{"c":null}}"#;

/// Token boundary offsets in [`DOC`]: the comma after the array ends at
/// 14; `"b"` starts there.
const CUT_BEFORE_B: u32 = 14;

#[test]
fn viewport_cut_matches_the_source_prefix() {
    let (events, tokens, _errors) = parse_events(DOC);
    let cut = cut_events(&events, &tokens, CUT_BEFORE_B);

    assert!(cut.truncated, "the viewport ends mid-document");
    assert_eq!(cut.resume_offset, CUT_BEFORE_B, "resume at the next token");
    for range in &cut.tokens {
        assert!(
            range.end <= CUT_BEFORE_B,
            "kept tokens stay in the viewport"
        );
    }

    let source = Source::from_str_unchecked(DOC);
    let parsed = build(ROOT_KIND, &cut, &[], &source).expect("well-formed cut");
    assert!(parsed.truncated);
    assert_eq!(
        parsed
            .tree
            .render(&Source::from_str_unchecked(&DOC[..CUT_BEFORE_B as usize])),
        &DOC[..CUT_BEFORE_B as usize],
        "the partial tree renders the covered prefix byte-for-byte"
    );
}

#[test]
fn full_viewport_equals_the_fresh_parse() {
    let (events, tokens, _errors) = parse_events(DOC);
    let cut = cut_events(&events, &tokens, DOC.len() as u32);
    assert!(!cut.truncated, "nothing beyond the viewport");
    assert_eq!(cut.resume_offset, DOC.len() as u32);
    assert_eq!(cut.tokens.len(), tokens.len(), "no token dropped");

    let source = Source::from_str_unchecked(DOC);
    let parsed = build(ROOT_KIND, &cut, &[], &source).expect("complete stream");
    let fresh = packs_json::parse(DOC).expect("fresh parse");
    assert_eq!(parsed.tree, fresh, "partial == fresh at full viewport");
}

#[test]
fn viewport_inside_a_token_resumes_at_its_start() {
    // `true` spans 8..12; a viewport ending at 9 drops it and resumes
    // at its start.
    let (events, tokens, _errors) = parse_events(DOC);
    let cut = cut_events(&events, &tokens, 9);
    assert!(cut.truncated);
    assert_eq!(cut.resume_offset, 8);
    assert_eq!(cut.tokens.last().map(|range| range.end), Some(8));

    let source = Source::from_str_unchecked(DOC);
    let parsed = build(ROOT_KIND, &cut, &[], &source).expect("well-formed cut");
    assert_eq!(
        parsed.tree.render(&Source::from_str_unchecked(&DOC[..8])),
        &DOC[..8]
    );
}

#[test]
fn empty_viewport_keeps_nothing() {
    let (events, tokens, _errors) = parse_events(DOC);
    let cut = cut_events(&events, &tokens, 0);
    assert!(cut.truncated);
    assert_eq!(cut.resume_offset, 0);
    assert_eq!(cut.tokens.len(), 0);

    let source = Source::from_str_unchecked(DOC);
    let parsed = build(ROOT_KIND, &cut, &[], &source).expect("well-formed cut");
    assert_eq!(parsed.tree.render(&Source::from_str_unchecked("")), "");
    // The builder synthesizes one zero-width EOF leaf at the covered end;
    // nothing else may be present.
    assert!(
        parsed
            .tree
            .token_ranges()
            .into_iter()
            .all(|(_, range)| range.is_empty()),
        "no sized token inside the viewport"
    );
}

#[test]
fn cut_never_splits_a_forward_parent_chain() {
    // A synthetic stream: the `Start` at index 2 forwards its final kind
    // to the `Start` at index 4 — two events beyond the natural cut
    // (before the token 5..15 that crosses the viewport at 10).
    use runtime_core::events::Event;
    use std::num::NonZeroU32;

    let events = vec![
        Event::Start {
            kind: 10,
            forward_parent: None,
        },
        Event::Token { kind: 1, n_raw: 1 }, // 0..5 (inside)
        Event::Start {
            kind: 20,
            forward_parent: NonZeroU32::new(2),
        }, // chain target = index 4
        Event::Token { kind: 1, n_raw: 1 }, // 5..15 (crosses 10)
        Event::Start {
            kind: 30,
            forward_parent: None,
        }, // the chain target itself
        Event::Token { kind: 2, n_raw: 1 }, // 15..20 (beyond)
        Event::Finish,
        Event::Finish,
        Event::Finish,
    ];
    let tokens = vec![(1_u16, 0_u32..5), (1_u16, 5..15), (2_u16, 15..20)];
    let cut = cut_events(&events, &tokens, 10);
    // The natural cut (before the crossing token) would orphan the
    // forward reference; the cut extends past the chain target, keeping
    // the crossing token.
    assert!(
        cut.events.len() >= 5,
        "the cut extends to keep the chain target"
    );
    assert!(matches!(
        cut.events[4],
        runtime_core::events::Event::Start { kind: 30, .. }
    ));
    assert_eq!(cut.resume_offset, 15, "the next dropped token starts at 15");

    let source = Source::from_str_unchecked("0123456789abcdefghij");
    let parsed = build(9, &cut, &[], &source).expect("the extended cut processes");
    assert!(parsed.truncated);
    let sized = parsed
        .tree
        .token_ranges()
        .into_iter()
        .filter(|(_, range)| !range.is_empty())
        .count();
    assert_eq!(sized, 2, "both kept tokens ride along");
}

#[test]
fn trivia_inside_the_prefix_stays_byte_faithful() {
    // Whitespace rides TriviaRecords (ADR-008), never token events; the
    // record covering [8..9] ends exactly at the resume offset and must
    // survive so the prefix keeps its byte.
    const DOC_WS: &str = r#"{"a": 1, "b": 2}"#;
    let (events, tokens, _errors) = parse_events(DOC_WS);
    let whitespace = [5..6, 8..9, 13..14].map(|range| TriviaRecord {
        kind: token_kinds::WHITESPACE,
        range: range.clone(),
        trailing: true,
    });

    let cut = cut_events(&events, &tokens, 9);
    assert_eq!(
        cut.resume_offset, 9,
        "\"b\" (9..12) is the first dropped token"
    );

    let source = Source::from_str_unchecked(DOC_WS);
    let parsed = build(ROOT_KIND, &cut, &whitespace, &source).expect("well-formed cut");
    assert_eq!(
        parsed
            .tree
            .render(&Source::from_str_unchecked(&DOC_WS[..9])),
        &DOC_WS[..9],
        "the comma's trailing space survives the cut"
    );
}

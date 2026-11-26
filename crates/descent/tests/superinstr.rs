//! Superinstruction mining suites (roadmap §12.2 remainder "superinstruction
//! mining", §10.2): the profile corpora mined for hot adjacent token-shift
//! pairs feed a fused fast path in the descent engine — "fused token-shift
//! sequences like `parse_identifier_equals()`" that are "conformance-gated"
//! (§10.2, the acceptance criterion pinned by the identity suites below):
//! the fused run MUST emit exactly the generic interpreter's events, token
//! records, trivia, errors, and mode crossings, byte for byte, on valid and
//! malformed input alike.

use std::ops::Range;

use descent::superinstr::mine;
use descent::{Desc, Pat, RuleDesc, parse_events, parse_events_with};
use descent_lower::lower;
use gir_syntax::load_grammar;
use pretty_assertions::assert_eq;

// ---------------------------------------------------------------------------
// Fixture grammar: one node rule whose body is a hot token-shift pair
// ---------------------------------------------------------------------------

/// Fixture token kinds (dense; never trivia — the feed carries only clean
/// tokens and the engine re-derives trivia from the gaps).
mod tk {
    /// the `A` token
    pub const A: u16 = 1;
    /// the `B` token
    pub const B: u16 = 2;
    /// the `C` token (never in the grammar — the malformed-input sentinel)
    pub const C: u16 = 3;
}

/// A one-rule descriptor grammar: `pair = A B` inside node kind 10, and a
/// document sweep that recovers anything else — the minimal shape with a
/// leading all-`Tok` `Seq` run of length two (the fusion site).
fn fixture_desc() -> Desc {
    Desc {
        rules: vec![RuleDesc {
            node_kind: 10,
            body: Pat::Seq(vec![Pat::Tok(tk::A), Pat::Tok(tk::B)]),
            sync: vec![tk::C],
            mode_hook: None,
        }],
        exprs: Vec::new(),
        start: Pat::Seq(vec![Pat::Rule(0), Pat::Rep(Box::new(Pat::Recover))]),
    }
}

/// Lex a tiny source of `A`/`B`/`C` characters into a clean feed (one char,
/// one token; the fusion semantics only care about kinds and adjacency).
fn lex(source: &str) -> Vec<(u16, Range<u32>)> {
    let bytes = source.as_bytes();
    let mut feed = Vec::new();
    let mut start = 0_u32;
    for (index, byte) in bytes.iter().enumerate() {
        let kind = match byte {
            b'A' => tk::A,
            b'B' => tk::B,
            _ => tk::C,
        };
        let end = u32::try_from(index + 1).expect("fixture fits u32");
        feed.push((kind, start..end));
        start = end;
    }
    feed
}

/// The profile corpus for the fixture: many `AB` repetitions, so the
/// adjacent pair `(A, B)` is the hot token shift.
fn profile() -> Vec<(u16, Range<u32>)> {
    lex("ABABABABAB")
}

// ---------------------------------------------------------------------------
// Mining: counts, ordering, cutoffs
// ---------------------------------------------------------------------------

#[test]
fn mine_counts_adjacent_pairs_with_hot_first() {
    let feed = profile();
    let set = mine(&[&feed], 4, 64);
    let entries = set.entries();
    assert!(!entries.is_empty(), "the AB corpus has adjacent pairs");
    let top = &entries[0];
    assert_eq!(top.kinds, [tk::A, tk::B], "AB is the most frequent pair");
    assert_eq!(top.count, 5, "ABABABABAB shifts A→B five times");
    assert!(
        set.is_hot_pair(tk::A, tk::B),
        "the mined pair gates the fusion"
    );
}

#[test]
fn mine_orders_by_count_then_kinds() {
    // Two hot pairs of DIFFERENT counts, plus a tie broken by kind order.
    let hot = lex("ABABAB");
    let second = lex("BCBC");
    let set = mine(&[&hot, &second], 2, 64);
    let counts: Vec<u64> = set.entries().iter().map(|entry| entry.count).collect();
    let mut sorted = counts.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(counts, sorted, "entries are count-descending");
    for pair in set.entries().windows(2) {
        if pair[0].count == pair[1].count {
            assert!(
                pair[0].kinds <= pair[1].kinds,
                "equal counts break ties by kind order"
            );
        }
    }
}

#[test]
fn mine_respects_min_count_and_cap() {
    let feed = profile();
    // (A, B) occurs 9 times; (B, A) occurs 4 times. A minimum above one of
    // them cuts exactly that entry out.
    let strict = mine(&[&feed], 5, 64);
    assert!(strict.is_hot_pair(tk::A, tk::B));
    assert!(!strict.is_hot_pair(tk::B, tk::A), "below the cutoff");
    // The cap keeps only the hottest entries.
    let capped = mine(&[&feed], 1, 1);
    assert_eq!(capped.entries().len(), 1, "capped to the hottest pair");
    assert_eq!(capped.entries()[0].kinds, [tk::A, tk::B]);
    // An empty cap mines nothing.
    assert!(mine(&[&feed], 1, 0).is_empty());
}

#[test]
fn mine_is_empty_without_adjacent_shifts() {
    let single = lex("A");
    assert!(mine(&[&single], 1, 64).is_empty(), "one token, no pairs");
    assert!(mine(&[], 1, 64).is_empty(), "no feeds, no pairs");
    let cold = lex("AABB");
    assert!(
        mine(&[&cold], 2, 64).is_empty(),
        "every pair occurs once, below the cutoff"
    );
}

// ---------------------------------------------------------------------------
// Fusion identity: the conformance gate (§10.2)
// ---------------------------------------------------------------------------

/// Every fixture input must parse identically through the fused fast path
/// and the generic interpreter — including the mid-run mismatch (`A` then
/// `C`) where the generic `Seq` keeps its committed prefix.
#[test]
fn fused_run_matches_generic_on_fixture_inputs() {
    let desc = fixture_desc();
    let set = mine(&[&profile()], 2, 64);
    assert!(set.is_hot_pair(tk::A, tk::B), "the fixture pair is hot");
    for source in ["AB", "ABAB", "ABABAB", "AC", "A", "ABC", "C", ""] {
        let feed = lex(source);
        let generic = parse_events(&desc, &feed, source);
        let fused = parse_events_with(&desc, &feed, source, Some(&set));
        assert_eq!(generic, fused, "source {source:?}: fused == generic");
    }
}

/// Without a mined table (or with one that does not cover the run) the
/// engine must take the generic path — same output, no fusion.
#[test]
fn unmined_table_leaves_the_generic_path() {
    let desc = fixture_desc();
    let source = "ABAB";
    let feed = lex(source);
    let generic = parse_events(&desc, &feed, source);
    let cold = mine(&[&lex("CC")], 1, 64);
    let fused = parse_events_with(&desc, &feed, source, Some(&cold));
    assert_eq!(generic, fused, "a cold table fuses nothing");
}

/// The end-to-end conformance gate on the real pipeline: the lowered JSON
/// fixture (`descent-lower` → engine) parses valid AND malformed documents
/// identically with and without the mined table — this is the §10.2
/// "conformance-gated" acceptance criterion.
#[test]
fn json_pipeline_fused_identity() {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    for source in [
        r#"{"a": 1, "b": [true, false, null], "c": {"d": "e"}}"#,
        r#"[1, 2, {"k": }]"#,
        r#"{"trailing": ,}"#,
        r"{]",
    ] {
        let feed = lowered.feed(&scanner, source);
        let generic = parse_events(&lowered.desc, &feed, source);
        // Profile corpora: two documents of the same shape (the mined pairs
        // are the pipeline's hot token shifts, e.g. string-then-colon).
        let corpus_a = lowered.feed(&scanner, r#"{"x": 1, "y": [2, 3]}"#);
        let corpus_b = lowered.feed(&scanner, r#"{"z": {"w": true}}"#);
        let set = mine(&[&corpus_a, &corpus_b], 4, 64);
        assert!(
            !set.entries().is_empty(),
            "the JSON profile mines at least one hot pair"
        );
        let fused = parse_events_with(&lowered.desc, &feed, source, Some(&set));
        assert_eq!(generic, fused, "JSON source {source:?}: fused == generic");
    }
}

//! Tree-level changed-ranges integration tests: real JSON pack parses
//! driven through [`changed_tree_ranges`]/[`diff_trees`] — precision on
//! the common edit shapes (token text, token insertion, value reshape),
//! structural invariants (sorted, coalesced, in-bounds, token-covered),
//! and seeded fuzz pinning with a determinism re-run.
//!
//! The token-level counterpart (`diff::changed_ranges`) is pinned in
//! `fuzz.rs`; these tests pin the tree-level seam the roadmap §7.7
//! prescribes for downstream invalidation.

use std::ops::Range;
use std::sync::Arc;

use incremental::{DiffVisitor, RangeCollector, SubtreeVerdict, changed_tree_ranges, diff_trees};
use packs_json::parse as parse_json;
use runtime_core::tree::GreenNode;

/// Parse with the JSON pack, asserting losslessness on the way in.
fn parse_ok(source: &str) -> Arc<GreenNode> {
    let tree = parse_json(source).expect("lossless parse");
    assert_eq!(
        tree.render(&runtime_core::input::Source::from_str_unchecked(source)),
        source,
        "parse must be byte-faithful"
    );
    tree
}

/// The byte span of `needle` in `haystack` (panic with context when absent).
fn span_of(haystack: &str, needle: &str) -> Range<u32> {
    let start = haystack
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` absent from {haystack:?}"));
    let end = start + needle.len();
    u32::try_from(start).unwrap_or(u32::MAX)..u32::try_from(end).unwrap_or(u32::MAX)
}

#[test]
fn identical_parse_has_no_tree_changes() {
    let source = r#"{"name": "syntact", "values": [1, 2, 3]}"#;
    let old = parse_ok(source);
    let new = parse_ok(source);
    assert_eq!(changed_tree_ranges(&old, source, &new, source), vec![]);
}

#[test]
fn token_edit_marks_exactly_the_number_token() {
    let old_source = r#"{"a": 1, "b": 2}"#;
    let new_source = r#"{"a": 1, "b": 42}"#;
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    assert_eq!(
        changed_tree_ranges(&old, old_source, &new, new_source),
        vec![span_of(new_source, "42")],
        "the changed number token is the only reported range"
    );
}

#[test]
fn token_shrink_marks_the_shortened_token() {
    let old_source = r#"{"a": 42}"#;
    let new_source = r#"{"a": 4}"#;
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    assert_eq!(
        changed_tree_ranges(&old, old_source, &new, new_source),
        vec![span_of(new_source, "4")],
        "deletion inside a token stays token-precise"
    );
}

#[test]
fn appended_element_marks_the_inserted_region() {
    let old_source = "[1, 2, 3]";
    let new_source = "[1, 2, 3, 4]";
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    // The inserted comma opens the refused region; the realigned closing
    // bracket sits in the unverified middle, so the conservative report
    // runs from the new comma through the bracket (", 4]").
    let start = new_source.find(", 4]").expect("inserted tail");
    let end = start + ", 4]".len();
    assert_eq!(
        changed_tree_ranges(&old, old_source, &new, new_source),
        vec![u32::try_from(start).unwrap_or(u32::MAX)..u32::try_from(end).unwrap_or(u32::MAX)],
    );
}

#[test]
fn value_reshape_marks_the_new_value_region() {
    let old_source = r#"{"a": 1}"#;
    let new_source = r#"{"a": [1]}"#;
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    // The old value rides as a token child of the pair; the new value is
    // a node child (with its leading trivia flushed inside) — the mixed
    // token-vs-node refusal covers the new child's span " [1]".
    assert_eq!(
        changed_tree_ranges(&old, old_source, &new, new_source),
        vec![span_of(new_source, " [1]")],
        "number → array reshapes exactly the new value node's span"
    );
}

#[test]
fn sibling_edits_coalesce_into_one_range() {
    let old_source = r#"{"a": 1, "b": 2, "c": 3}"#;
    // Two neighbouring numbers change; the structurally identical
    // siblings (keys, punctuation) around them must not be reported.
    let new_source = r#"{"a": 10, "b": 20, "c": 3}"#;
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    let ranges = changed_tree_ranges(&old, old_source, &new, new_source);
    let first = span_of(new_source, "10");
    let second = span_of(new_source, "20");
    assert!(
        ranges.len() <= 2,
        "coalescing keeps the report minimal: {ranges:?}"
    );
    assert!(
        ranges.contains(&first) || covers(&ranges, &first),
        "{ranges:?}"
    );
    assert!(
        ranges.contains(&second) || covers(&ranges, &second),
        "{ranges:?}"
    );
    // The untouched "c": 3 tail must stay outside every range.
    let tail = span_of(new_source, r#""c": 3"#);
    assert!(
        !covers(&ranges, &tail),
        "untouched sibling reported: {ranges:?}"
    );
}

/// Whether `range` is fully inside any of `ranges`.
fn covers(ranges: &[Range<u32>], range: &Range<u32>) -> bool {
    ranges
        .iter()
        .any(|r| r.start <= range.start && range.end <= r.end)
}

#[test]
fn diff_trees_reports_may_differ_then_matches_on_identical_trees() {
    let source = "[1, 2]";
    let old = parse_ok(source);
    let new = parse_ok(source);
    let mut collector = EventLog::default();
    let flow = diff_trees(&old, source, &new, source, &mut collector);
    assert_eq!(flow, std::ops::ControlFlow::Continue(()));
    assert!(
        collector
            .events
            .iter()
            .any(|(v, _, _)| *v == SubtreeVerdict::MayDiffer),
        "descending pairs are reported as MayDiffer: {:?}",
        collector.events
    );
    assert!(
        collector
            .events
            .iter()
            .all(|(v, _, _)| *v != SubtreeVerdict::Differs),
        "identical trees never report Differs"
    );
}

/// A test visitor recording every classification event.
#[derive(Default)]
struct EventLog {
    events: Vec<(SubtreeVerdict, Range<u32>, Range<u32>)>,
}
impl DiffVisitor for EventLog {
    fn subtree(
        &mut self,
        verdict: SubtreeVerdict,
        old_span: Range<u32>,
        new_span: Range<u32>,
    ) -> std::ops::ControlFlow<()> {
        self.events.push((verdict, old_span, new_span));
        std::ops::ControlFlow::Continue(())
    }
}

/// A visitor that breaks after the first `Differs` event.
struct FirstFault(RangeCollector);
impl DiffVisitor for FirstFault {
    fn subtree(
        &mut self,
        verdict: SubtreeVerdict,
        old_span: Range<u32>,
        new_span: Range<u32>,
    ) -> std::ops::ControlFlow<()> {
        let flow = self.0.subtree(verdict, old_span, new_span);
        if verdict == SubtreeVerdict::Differs {
            return std::ops::ControlFlow::Break(());
        }
        flow
    }
}

#[test]
fn visitor_early_exit_halts_the_walk() {
    let old_source = r#"{"a": 1, "b": 2}"#;
    let new_source = r#"{"a": 1, "b": 42}"#;
    let old = parse_ok(old_source);
    let new = parse_ok(new_source);
    // Break after the first Differs event: the walk must stop there.
    let mut visitor = FirstFault(RangeCollector::new());
    let flow = diff_trees(&old, old_source, &new, new_source, &mut visitor);
    assert_eq!(
        flow,
        std::ops::ControlFlow::Break(()),
        "walk halts on break"
    );
    assert_eq!(
        visitor.0.into_ranges(),
        vec![span_of(new_source, "42")],
        "exactly the first fault is collected before the halt"
    );
}

/// Seeded fuzz: structural invariants hold for every mutation of the
/// JSON corpus, and length-preserving single-token edits stay
/// token-precise.
#[test]
fn fuzz_json_tree_diff_invariants() {
    let corpus = [
        r#"{"a": 1}"#,
        r"[1, 2, 3]",
        r#"{"deep": {"deeper": [true, null]}, "b": "x"}"#,
        r#"[{"k": "v"}, [1, [2]], 42]"#,
        r#""top level string""#,
    ];
    let mut rng = Rng::new(0x7EED_1FF5);
    for round in 0..150 {
        let old_source = corpus[round % corpus.len()];
        let old = parse_ok(old_source);
        let edit = random_edit(&mut rng, old_source);
        let new_source = edit.apply(old_source);
        let new = parse_ok(&new_source);

        let ranges = changed_tree_ranges(&old, old_source, &new, &new_source);
        // Determinism: a second walk (collector form) produces the same ranges.
        let mut rerun = RangeCollector::new();
        let _ = diff_trees(&old, old_source, &new, &new_source, &mut rerun);
        assert_eq!(
            rerun.into_ranges(),
            ranges,
            "round {round}: nondeterministic"
        );

        // Sorted, coalesced, in-bounds.
        for (a, b) in ranges.iter().zip(ranges.iter().skip(1)) {
            assert!(a.end <= b.start, "round {round}: overlapping {a:?} {b:?}");
        }
        for r in &ranges {
            assert!(
                r.end <= new_source.len() as u32,
                "round {round}: out of bounds {r:?}"
            );
        }

        // Token coverage: every reported byte rides a new-tree token.
        let tokens = new.token_ranges();
        assert!(
            token_covered(&tokens, &ranges),
            "round {round}: uncovered {ranges:?}"
        );

        // Equal sources ⇒ empty report; differing sources ⇒ non-empty.
        if new_source == old_source {
            assert!(
                ranges.is_empty(),
                "round {round}: phantom changes {ranges:?}"
            );
        } else {
            assert!(!ranges.is_empty(), "round {round}: missed change");
        }

        // Precision: when the two parses are structurally isomorphic
        // (same node kinds and token kinds+texts in DFS order) and
        // exactly one token entry differs, the tree diff is that token's
        // span — sibling and node structure survived untouched.
        let new_tokens = new.token_ranges();
        let old_shape = shape(&old, old_source);
        let new_shape = shape(&new, &new_source);
        if old_shape.len() == new_shape.len() {
            let differing: Vec<usize> = old_shape
                .iter()
                .zip(new_shape.iter())
                .enumerate()
                .filter(|(_, (o, n))| o != n)
                .map(|(i, _)| i)
                .collect();
            if differing.len() == 1 && !new_shape[differing[0]].0 {
                let token_index = new_shape[..differing[0]].iter().filter(|e| !e.0).count();
                let expected = new_tokens[token_index].1.clone();
                assert_eq!(
                    ranges,
                    vec![expected],
                    "round {round}: single-token edit lost precision on {new_source:?}"
                );
            }
        }
    }
}

/// The DFS shape sequence: `(is_node, kind, token text)` per walk event,
/// the isomorphism witness the precision gate compares.
fn shape(tree: &Arc<GreenNode>, source: &str) -> Vec<(bool, u16, String)> {
    runtime_core::view::walk(tree)
        .filter_map(|event| match event {
            runtime_core::view::WalkEvent::Enter(node) => Some((true, node.kind, String::new())),
            runtime_core::view::WalkEvent::Token { kind, range } => {
                Some((false, kind, token_text(source, &range).to_owned()))
            }
            runtime_core::view::WalkEvent::Leave(_) => None,
        })
        .collect()
}

/// Token text for a range, clipped to the source.
fn token_text<'s>(source: &'s str, range: &Range<u32>) -> &'s str {
    let start = (range.start as usize).min(source.len());
    let end = (range.end as usize).min(source.len()).max(start);
    &source[start..end]
}

/// Every byte of `ranges` must lie inside some token span.
fn token_covered(tokens: &[(u16, Range<u32>)], ranges: &[Range<u32>]) -> bool {
    let in_token = |pos: u32| tokens.iter().any(|(_, r)| r.start <= pos && pos < r.end);
    ranges.iter().all(|r| (r.start..r.end).all(&in_token))
}

/// Deterministic xorshift64* — no external RNG dependency (mirrors fuzz.rs).
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            let bound64 = u64::try_from(bound).unwrap_or(u64::MAX);
            usize::try_from(self.next() % bound64).unwrap_or(0)
        }
    }
}

/// A random length-preserving or arbitrary single-point mutation.
fn random_edit(rng: &mut Rng, source: &str) -> incremental::Edit {
    let len = source.len();
    let start = rng.below(len + 1);
    match rng.next() % 3 {
        0 => {
            let chars = ["x", "1", ",", "\"", "}", " ", ":"];
            let text = chars[rng.below(chars.len())];
            incremental::Edit::insert(start, text)
        }
        1 => {
            let end = (start + 1 + rng.below(3)).min(len);
            incremental::Edit::delete(start, end)
        }
        _ => {
            let end = (start + 1 + rng.below(3)).min(len);
            incremental::Edit::replace(start, end, "z9")
        }
    }
}

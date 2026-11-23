//! Tree-level changed-ranges diffing (roadmap §7.7 step 6): the parallel
//! old/new green-tree walk that classifies every compared subtree as
//! [`SubtreeVerdict::Matches`]/[`SubtreeVerdict::MayDiffer`]/[`SubtreeVerdict::Differs`]
//! (the tree-sitter `get_changed_ranges.c` predicate shape, roadmap §2.2)
//! and reports the coalesced byte ranges where the trees disagree.
//!
//! # The seam
//!
//! [`diff_trees`] drives a [`DiffVisitor`] over the classification
//! events; [`changed_tree_ranges`] is the built-in consumer that
//! collects the new-source spans of every differing unit through
//! [`RangeCollector`]. The visitor is the observability point (the same
//! role [`crate::diff::changed_ranges`] plays for token streams) and its
//! `ControlFlow` return is the early-exit channel — the convention
//! already established by `structural_index::RegionSink` and
//! `runtime_core::view::visit`.
//!
//! # Comparison contract
//!
//! Two subtree pairs compare as follows, in DFS order, one visitor event
//! per comparison:
//!
//! - **Header refusal** — kind or error-flag disagreement refuses the
//!   whole subtree pair: one `Differs` event over both spans, no
//!   descent. The error flag rides parse diagnostics (a diagnostic that
//!   appears or clears is a change even when the bytes are identical),
//!   so its flip refuses the subtree.
//! - **Childless pair** — both sides have no children: `Matches`.
//! - **Descent** — headers agree with children present: one
//!   `MayDiffer` event, then the children align (below). No summary
//!   event fires for the pair itself; the children's events tell the
//!   story.
//! - **Token pair** — equal kind *and* text (over each side's own
//!   source) is `Matches`; anything else is `Differs` over the token
//!   spans. Tokens are compared by content, so a token that only
//!   *moved* (edit drift) still matches.
//! - **Mixed pair** (node vs token): `Differs` over the child spans.
//!
//! Children align one of two ways:
//!
//! - **Equal counts**: strict pairwise, every pair compared, sibling
//!   precision preserved.
//! - **Unequal counts** (insertion/deletion): maximal identical prefix
//!   and suffix are absorbed (full comparison, precise events); the
//!   first non-identical pair on each side is precisely handled and
//!   excluded; the unverified middle between them is refused as one
//!   `Differs` event over the middle spans. Sound and conservative:
//!   every changed byte lies inside a reported span, but elements
//!   realigned across an insertion point may be marked even though
//!   their content survived.
//!
//! A deletion (empty new middle) reports an empty new span, which
//! [`RangeCollector`] drops — downstream invalidation works on the new
//! tree, where deleted bytes leave nothing to invalidate. Purity: equal
//! sources always produce an empty report; differing sources always
//! produce at least one range.

use std::ops::ControlFlow;
use std::ops::Range;
use std::sync::Arc;

use runtime_core::tree::{GreenChild, GreenNode};

/// The classification of one compared subtree pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtreeVerdict {
    /// The subtrees are structurally and textually identical.
    Matches,
    /// The headers agree — the walk descends into the children to
    /// decide (no summary is issued for the pair itself).
    MayDiffer,
    /// The subtrees disagree; `new_span` is the region to invalidate.
    Differs,
}

/// Observes the classification of every compared subtree pair.
///
/// One `subtree` call per comparison (see the module contract for when
/// each verdict fires). Returning
/// [`ControlFlow::Break`] halts the walk
/// immediately — the built-in [`RangeCollector`] never breaks, so the
/// default drive always visits every aligned pair.
pub trait DiffVisitor {
    /// One compared subtree pair: the verdict plus both byte spans
    /// (old-source and new-source coordinates).
    fn subtree(
        &mut self,
        verdict: SubtreeVerdict,
        old_span: Range<u32>,
        new_span: Range<u32>,
    ) -> ControlFlow<()>;
}

/// The built-in [`DiffVisitor`]: collects the new-source span of every
/// `Differs` event, canonicalizing (sorting, then coalescing
/// adjacent/overlapping spans) on read and dropping empty spans —
/// deletions leave nothing to invalidate in the new tree.
/// Canonicalize-on-read because the event order is DFS order, and a
/// refused middle fires *after* the suffix pair that stopped ahead of it.
#[derive(Debug, Default)]
pub struct RangeCollector {
    /// Changed ranges in new-source coordinates (push order).
    ranges: Vec<Range<u32>>,
}

impl RangeCollector {
    /// A fresh collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical (sorted, coalesced) ranges collected so far.
    #[must_use]
    pub fn ranges(&self) -> Vec<Range<u32>> {
        canonical(&self.ranges)
    }

    /// Consume the collector into its canonical ranges.
    #[must_use]
    pub fn into_ranges(self) -> Vec<Range<u32>> {
        canonical(&self.ranges)
    }

    /// How many raw spans were collected (before canonicalization).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Whether nothing changed (the common no-edit case).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

impl DiffVisitor for RangeCollector {
    fn subtree(
        &mut self,
        verdict: SubtreeVerdict,
        _old_span: Range<u32>,
        new_span: Range<u32>,
    ) -> ControlFlow<()> {
        if verdict == SubtreeVerdict::Differs && !new_span.is_empty() {
            self.ranges.push(new_span);
        }
        ControlFlow::Continue(())
    }
}

/// Sort by start, then merge adjacent/overlapping spans (empty dropped).
fn canonical(ranges: &[Range<u32>]) -> Vec<Range<u32>> {
    let mut sorted: Vec<Range<u32>> = ranges.iter().filter(|r| r.start < r.end).cloned().collect();
    sorted.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<u32>> = Vec::with_capacity(sorted.len());
    for span in sorted {
        match merged.last_mut() {
            Some(last) if last.end >= span.start => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

/// Diff two green trees, driving `visitor` over every classification
/// event in DFS order.
///
/// `old_source`/`new_source` back the token texts of each tree (green
/// trees are position-independent; token leaves carry ranges into their
/// own source). Spans reported to the visitor are absolute byte ranges
/// into the respective sources.
///
/// Returns [`ControlFlow::Break`] when the visitor broke the walk, and
/// [`ControlFlow::Continue`] when every aligned pair was visited.
pub fn diff_trees(
    old_tree: &Arc<GreenNode>,
    old_source: &str,
    new_tree: &Arc<GreenNode>,
    new_source: &str,
    visitor: &mut dyn DiffVisitor,
) -> ControlFlow<()> {
    compare(
        old_tree,
        0..old_tree.text_len,
        old_source,
        new_tree,
        0..new_tree.text_len,
        new_source,
        visitor,
    )
    .0
}

/// The [`RangeCollector`] convenience over [`diff_trees`]: the coalesced
/// new-source byte ranges where the trees disagree.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use incremental::changed_tree_ranges;
/// use packs_json::parse;
///
/// let old = parse(r#"{"n": 1}"#).expect("parse");
/// let new = parse(r#"{"n": 42}"#).expect("parse");
/// assert_eq!(
///     changed_tree_ranges(&old, r#"{"n": 1}"#, &new, r#"{"n": 42}"#),
///     vec![6..8],
/// );
/// ```
#[must_use]
pub fn changed_tree_ranges(
    old_tree: &Arc<GreenNode>,
    old_source: &str,
    new_tree: &Arc<GreenNode>,
    new_source: &str,
) -> Vec<Range<u32>> {
    let mut collector = RangeCollector::new();
    let _ = diff_trees(old_tree, old_source, new_tree, new_source, &mut collector);
    collector.into_ranges()
}

/// Compare one subtree pair. Returns the visitor's flow (a break
/// propagates untouched) and whether the pair is fully identical.
fn compare(
    old: &GreenNode,
    old_span: Range<u32>,
    old_source: &str,
    new: &GreenNode,
    new_span: Range<u32>,
    new_source: &str,
    visitor: &mut dyn DiffVisitor,
) -> (ControlFlow<()>, bool) {
    // Header refusal: kind or error-flag disagreement. The error flag
    // rides diagnostics, so a diagnostic that appears or clears is a
    // change even when the bytes are identical.
    if old.kind != new.kind || old.has_error != new.has_error {
        let flow = visitor.subtree(SubtreeVerdict::Differs, old_span, new_span);
        return (flow, false);
    }
    if old.children.is_empty() && new.children.is_empty() {
        let flow = visitor.subtree(SubtreeVerdict::Matches, old_span, new_span);
        return (flow, true);
    }
    // Headers agree — descend; the children's events tell the story.
    let old_start = old_span.start;
    let new_start = new_span.start;
    let flow = visitor.subtree(SubtreeVerdict::MayDiffer, old_span, new_span);
    if flow.is_break() {
        return (flow, false);
    }
    let mut identical = true;
    if old.children.len() == new.children.len() {
        // Strict pairwise: every pair compared, sibling precision kept.
        let mut old_cursor = old_start;
        let mut new_cursor = new_start;
        for (o, n) in old.children.iter().zip(new.children.iter()) {
            let olen = o.text_len();
            let nlen = n.text_len();
            let (flow, same) = compare_pair(
                o,
                old_cursor..old_cursor.saturating_add(olen),
                old_source,
                n,
                new_cursor..new_cursor.saturating_add(nlen),
                new_source,
                visitor,
            );
            if flow.is_break() {
                return (flow, false);
            }
            identical &= same;
            old_cursor = old_cursor.saturating_add(olen);
            new_cursor = new_cursor.saturating_add(nlen);
        }
        return (ControlFlow::Continue(()), identical);
    }
    // Count misalignment: absorb identical prefix and suffix (full
    // comparison, precise events), refuse the unverified middle. The
    // first non-identical pair on each side is precisely handled and
    // excluded from the middle.
    let olen = old.children.len();
    let nlen = new.children.len();
    let min = olen.min(nlen);
    let old_bounds = child_offsets(old_start, &old.children);
    let new_bounds = child_offsets(new_start, &new.children);
    let mut p = 0usize;
    let mut prefix_stopped = false;
    while p < min {
        let (flow, same) = compare_pair(
            &old.children[p],
            old_bounds[p]..old_bounds[p + 1],
            old_source,
            &new.children[p],
            new_bounds[p]..new_bounds[p + 1],
            new_source,
            visitor,
        );
        if flow.is_break() {
            return (flow, false);
        }
        identical &= same;
        if !same {
            prefix_stopped = true;
            break;
        }
        p += 1;
    }
    let handled = if prefix_stopped { p + 1 } else { p };
    // Suffix: full comparison from the back over the remainder.
    let mut s = 0usize;
    let mut suffix_stopped = false;
    while let (Some(oi), Some(ni)) = (olen.checked_sub(s + 1), nlen.checked_sub(s + 1)) {
        if oi < handled || ni < handled {
            break;
        }
        let (flow, same) = compare_pair(
            &old.children[oi],
            old_bounds[oi]..old_bounds[oi + 1],
            old_source,
            &new.children[ni],
            new_bounds[ni]..new_bounds[ni + 1],
            new_source,
            visitor,
        );
        if flow.is_break() {
            return (flow, false);
        }
        if !same {
            suffix_stopped = true;
            break;
        }
        s += 1;
    }
    // The refused middle (inverted bounds mean one side has none; the
    // span end clamps to the start so the event stays well-formed).
    let old_mid_start = handled.min(olen);
    let new_mid_start = handled.min(nlen);
    let old_mid_end = if suffix_stopped {
        olen - 1 - s
    } else {
        olen - s
    };
    let new_mid_end = if suffix_stopped {
        nlen - 1 - s
    } else {
        nlen - s
    };
    let old_mid_end = old_mid_end.max(old_mid_start);
    let new_mid_end = new_mid_end.max(new_mid_start);
    if old_mid_start < old_mid_end || new_mid_start < new_mid_end {
        let old_mid = old_bounds[old_mid_start]..old_bounds[old_mid_end];
        let new_mid = new_bounds[new_mid_start]..new_bounds[new_mid_end];
        let flow = visitor.subtree(SubtreeVerdict::Differs, old_mid, new_mid);
        if flow.is_break() {
            return (flow, false);
        }
        identical = false;
    }
    (ControlFlow::Continue(()), identical)
}

/// Compare one child pair (node/node, token/token, or mixed).
fn compare_pair(
    old: &GreenChild,
    old_span: Range<u32>,
    old_source: &str,
    new: &GreenChild,
    new_span: Range<u32>,
    new_source: &str,
    visitor: &mut dyn DiffVisitor,
) -> (ControlFlow<()>, bool) {
    match (old, new) {
        (GreenChild::Node(o), GreenChild::Node(n)) => {
            compare(o, old_span, old_source, n, new_span, new_source, visitor)
        }
        (
            GreenChild::Token {
                kind: old_kind,
                range: old_range,
            },
            GreenChild::Token {
                kind: new_kind,
                range: new_range,
            },
        ) => {
            // Tokens compare by kind and text over each side's own
            // source; the stored range is the token's authoritative span
            // (what render and `token_ranges` report).
            let same = old_kind == new_kind
                && token_text(old_source, old_range) == token_text(new_source, new_range);
            let verdict = if same {
                SubtreeVerdict::Matches
            } else {
                SubtreeVerdict::Differs
            };
            let flow = visitor.subtree(verdict, old_range.clone(), new_range.clone());
            (flow, same)
        }
        // A node aligned against a token: no common structure to descend.
        _ => {
            let flow = visitor.subtree(SubtreeVerdict::Differs, old_span, new_span);
            (flow, false)
        }
    }
}

/// The cumulative child boundaries of `children` starting at `start`
/// (`len + 1` entries; `bounds[i]..bounds[i+1]` is child `i`'s span).
fn child_offsets(start: u32, children: &[GreenChild]) -> Vec<u32> {
    let mut out = Vec::with_capacity(children.len() + 1);
    let mut cursor = start;
    out.push(cursor);
    for child in children {
        cursor = cursor.saturating_add(child.text_len());
        out.push(cursor);
    }
    out
}

/// Token text for a byte span, clipped to the source.
fn token_text<'s>(source: &'s str, range: &Range<u32>) -> &'s str {
    let start = (range.start as usize).min(source.len());
    let end = (range.end as usize).min(source.len()).max(start);
    &source[start..end]
}

#[cfg(test)]
mod tree_diff_tests {
    use std::sync::Arc;

    use super::{DiffVisitor, RangeCollector, SubtreeVerdict, changed_tree_ranges, diff_trees};
    use runtime_core::tree::{GreenChild, GreenNode};

    /// A hand-built interior node (`text_len` derived from the children).
    fn node(kind: u16, children: Vec<GreenChild>) -> Arc<GreenNode> {
        let text_len = children.iter().map(GreenChild::text_len).sum();
        Arc::new(GreenNode {
            kind,
            text_len,
            has_error: false,
            children,
        })
    }

    /// A hand-built interior child (the `GreenChild::Node` wrapper).
    fn n(kind: u16, children: Vec<GreenChild>) -> GreenChild {
        GreenChild::Node(node(kind, children))
    }

    /// A hand-built token leaf over an absolute source range.
    fn tok(kind: u16, range: std::ops::Range<u32>) -> GreenChild {
        GreenChild::Token { kind, range }
    }

    /// A test visitor recording every classification event.
    #[derive(Default)]
    struct Log {
        events: Vec<(SubtreeVerdict, (u32, u32), (u32, u32))>,
    }
    impl DiffVisitor for Log {
        fn subtree(
            &mut self,
            verdict: SubtreeVerdict,
            old_span: std::ops::Range<u32>,
            new_span: std::ops::Range<u32>,
        ) -> std::ops::ControlFlow<()> {
            self.events.push((
                verdict,
                (old_span.start, old_span.end),
                (new_span.start, new_span.end),
            ));
            std::ops::ControlFlow::Continue(())
        }
    }

    #[test]
    fn identical_hand_built_trees_have_no_ranges() {
        let old = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        let new = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        assert_eq!(changed_tree_ranges(&old, "abcd", &new, "abcd"), vec![]);
    }

    #[test]
    fn token_text_change_marks_exactly_the_token() {
        let old = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        let new = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        assert_eq!(changed_tree_ranges(&old, "abcd", &new, "abXY"), vec![2..4]);
    }

    #[test]
    fn token_kind_change_marks_exactly_the_token() {
        let old = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        let new = node(0, vec![tok(1, 0..2), tok(3, 2..4)]);
        assert_eq!(changed_tree_ranges(&old, "abcd", &new, "abcd"), vec![2..4]);
    }

    #[test]
    fn child_append_marks_the_appended_middle() {
        let old = node(0, vec![tok(1, 0..2)]);
        let new = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "abxy"), vec![2..4]);
    }

    #[test]
    fn child_deletion_reports_no_new_bytes() {
        let old = node(0, vec![tok(1, 0..2), tok(2, 2..4)]);
        let new = node(0, vec![tok(1, 0..2)]);
        // The deletion's new middle is empty: nothing to invalidate in
        // the new tree, so the collector drops the empty span.
        assert_eq!(changed_tree_ranges(&old, "abcd", &new, "ab"), vec![]);
    }

    #[test]
    fn mixed_child_pair_marks_the_new_child_span() {
        let old = node(0, vec![tok(1, 0..2)]);
        let new = node(0, vec![n(1, vec![tok(2, 0..2)])]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "ab"), vec![0..2]);
    }

    #[test]
    fn error_flag_flip_refuses_the_whole_subtree() {
        let old = node(0, vec![tok(1, 0..2)]);
        let mut flagged = node(0, vec![tok(1, 0..2)]);
        let root = Arc::get_mut(&mut flagged).expect("hand-built root is unique");
        root.has_error = true;
        assert_eq!(changed_tree_ranges(&old, "ab", &flagged, "ab"), vec![0..2]);
    }

    #[test]
    fn root_kind_mismatch_marks_the_whole_tree() {
        let old = node(0, vec![tok(1, 0..2)]);
        let new = node(7, vec![tok(1, 0..2)]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "ab"), vec![0..2]);
    }

    #[test]
    fn adjacent_changed_tokens_coalesce() {
        let old = node(0, vec![tok(1, 0..1), tok(2, 1..2)]);
        let new = node(0, vec![tok(1, 0..1), tok(2, 1..2)]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "xy"), vec![0..2]);
    }

    #[test]
    fn shifted_tokens_match_by_text_not_position() {
        // The inserted byte shifts the second token's range but its text
        // survives: tokens compare by content, so drift produces no
        // phantom change (real pack trees cover the gap with a trivia
        // leaf, which is where the insertion is then reported).
        let old = node(0, vec![tok(1, 0..1), tok(2, 1..2)]);
        let new = node(0, vec![tok(1, 0..1), tok(2, 2..3)]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "aXb"), vec![]);
    }

    #[test]
    fn nested_descend_reports_only_the_changed_leaf() {
        // root { inner { tok(1, 0..1) } tok(2, 1..2) }: the inner leaf's
        // text changes ("a" → "b"), the outer sibling survives untouched
        // — the report must cover only the leaf.
        let old = node(0, vec![n(1, vec![tok(1, 0..1)]), tok(2, 1..2)]);
        let new = node(0, vec![n(1, vec![tok(1, 0..1)]), tok(2, 1..2)]);
        assert_eq!(changed_tree_ranges(&old, "ab", &new, "bb"), vec![0..1]);
    }

    /// A visitor counting events and breaking after the first `Differs`.
    struct FirstFault(RangeCollector, usize);
    impl DiffVisitor for FirstFault {
        fn subtree(
            &mut self,
            verdict: SubtreeVerdict,
            old_span: std::ops::Range<u32>,
            new_span: std::ops::Range<u32>,
        ) -> std::ops::ControlFlow<()> {
            self.1 += 1;
            let flow = self.0.subtree(verdict, old_span, new_span);
            if verdict == SubtreeVerdict::Differs {
                return std::ops::ControlFlow::Break(());
            }
            flow
        }
    }

    #[test]
    fn visitor_break_stops_the_walk_after_the_first_fault() {
        // Three sibling tokens over "abc" → "aXc": token 2 differs,
        // token 3 matches; breaking after the first Differs must halt
        // the walk with exactly that one range.
        let old = node(0, vec![tok(1, 0..1), tok(2, 1..2), tok(3, 2..3)]);
        let new = node(0, vec![tok(1, 0..1), tok(2, 1..2), tok(3, 2..3)]);
        let mut visitor = FirstFault(RangeCollector::new(), 0);
        let flow = diff_trees(&old, "abc", &new, "aXc", &mut visitor);
        assert_eq!(flow, std::ops::ControlFlow::Break(()));
        assert_eq!(visitor.0.into_ranges(), vec![1..2]);
        assert!(visitor.1 < 8, "walk stopped early: {} events", visitor.1);
    }

    #[test]
    fn may_differ_precedes_the_children_events() {
        let old = node(0, vec![n(1, vec![tok(1, 0..1)])]);
        let new = node(0, vec![n(1, vec![tok(1, 0..1)])]);
        let mut log = Log::default();
        let flow = diff_trees(&old, "a", &new, "a", &mut log);
        assert_eq!(flow, std::ops::ControlFlow::Continue(()));
        // root MayDiffer → inner MayDiffer → token Matches
        assert_eq!(
            log.events,
            vec![
                (SubtreeVerdict::MayDiffer, (0, 1), (0, 1)),
                (SubtreeVerdict::MayDiffer, (0, 1), (0, 1)),
                (SubtreeVerdict::Matches, (0, 1), (0, 1)),
            ]
        );
    }
}

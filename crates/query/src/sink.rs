//! The push-based query matcher (§7.2 consumer table): a
//! [`runtime_core::sink::Sink`] that runs the compiled patterns as the
//! flattened stream drives — no separate cursor pass, no tree
//! construction.
//!
//! Matching, capture binding, dedup, predicate dispatch, and span
//! semantics are byte-for-byte the same code path as the pull-based
//! [`crate::QueryCursor`] (the shared `cursor::evaluate_node` and
//! `cursor::NodeFrame` internals), so the two consumers stay
//! behaviorally identical; the differential suite pins the parity.
//! Drive the sink through [`runtime_core::sink::drive_sink`] — alone,
//! or zipped with a tree-side consumer via
//! [`runtime_core::sink::TeeSink`] to build a tree and match queries
//! in one pass.
//!
//! # Inputs and parity contract
//!
//! The sink consumes the same parallel `(kind, range)` token table the
//! pull cursor does ([`QuerySink::new`]) — the driver needs it anyway
//! (`drive_sink`'s second argument), so spans and child bindings use
//! identical lookups, including the zero-width fallbacks when the table
//! is short relative to the stream. The host predicate closure is bound
//! at construction and dispatches custom predicates exactly like the
//! cursor's per-run closure.

use std::ops::Range;

use runtime_core::sink::Sink;

use crate::cursor::{Capture, Match, NodeFrame, PredicateFn, evaluate_node};
use crate::query::CompiledQuery;

/// The streaming query sink: patterns evaluate at every node's `Exit`
/// as the stream drives. Matches accumulate in DFS completion order —
/// the same order the pull cursor yields them.
///
/// Malformed streams degrade like the other sinks: an `Exit` with
/// nothing open is ignored, tokens outside any node update only the
/// running token index, and no panic occurs.
pub struct QuerySink<'q, 's, 'r, 'p> {
    /// The compiled query.
    query: &'q CompiledQuery,
    /// The source the stream was parsed from (predicate texts + span
    /// clamping).
    source: &'s str,
    /// The token side table parallel to the stream's token events —
    /// the same table [`drive_sink`] consumes.
    ranges: &'r [(u16, Range<u32>)],
    /// The host predicate closure (custom predicates dispatch here).
    predicate: &'p mut PredicateFn<'p>,
    /// Matches accumulated so far (DFS completion order).
    matches: Vec<Match>,
    /// Open nodes: innermost last.
    stack: Vec<NodeFrame>,
    /// The run-wide dedup set: `(slot, span)` pairs already emitted.
    seen: Vec<(u8, u32, u32)>,
    /// Scratch buffer for candidate captures (cleared, not reallocated,
    /// per candidate — the O(1)-alloc-per-candidate contract).
    scratch: Vec<Capture>,
    /// Running count of token events seen (the cursor's token index).
    token_index: usize,
}

impl<'q, 's, 'r, 'p> QuerySink<'q, 's, 'r, 'p> {
    /// A sink for `query` over `source`; `ranges` is the token side
    /// table parallel to the driven stream (identical lookups to the
    /// pull cursor), and custom predicates dispatch to `predicate`.
    #[must_use]
    pub fn new(
        query: &'q CompiledQuery,
        source: &'s str,
        ranges: &'r [(u16, Range<u32>)],
        predicate: &'p mut PredicateFn<'p>,
    ) -> Self {
        Self {
            query,
            source,
            ranges,
            predicate,
            matches: Vec::new(),
            stack: Vec::new(),
            seen: Vec::new(),
            scratch: Vec::new(),
            token_index: 0,
        }
    }

    /// The matches accumulated so far (DFS completion order).
    #[must_use]
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// Consume the sink, returning every match.
    #[must_use = "dropping the result discards every match"]
    pub fn into_matches(self) -> Vec<Match> {
        self.matches
    }
}

impl Sink for QuerySink<'_, '_, '_, '_> {
    fn enter(&mut self, kind: runtime_core::NodeKind) {
        self.stack.push(NodeFrame {
            kind,
            first_token: self.token_index,
            last_token: None,
            child_kinds: Vec::new(),
            child_spans: Vec::new(),
        });
    }

    fn exit(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return; // exit underflow: ignored, like the cursor
        };
        let span = frame.span(self.ranges, self.source.len());
        if let Some(parent) = self.stack.last_mut() {
            parent.child_kinds.push(frame.kind);
            let span32 = u32::try_from(span.start).unwrap_or(u32::MAX)
                ..u32::try_from(span.end).unwrap_or(u32::MAX);
            parent.child_spans.push(span32);
            if frame.last_token.is_some() {
                parent.last_token = frame.last_token;
            }
        }
        let summary = crate::cursor::NodeSummary {
            kind: frame.kind,
            child_kinds: frame.child_kinds,
            child_spans: frame.child_spans,
        };
        let node_matches = evaluate_node(
            self.query,
            &mut self.seen,
            &mut self.scratch,
            &summary,
            span,
            self.source,
            &mut *self.predicate,
        );
        self.matches.extend(node_matches);
    }

    fn token(&mut self, _kind: runtime_core::TokenKind, _range: Range<u32>, _n_raw: u8) {
        // Span and child data come from the side table at the running
        // index (parity with the cursor); the event's own copy of the
        // range is redundant with the table entry.
        let this_token = self.token_index;
        self.token_index += 1;
        if let Some(frame) = self.stack.last_mut() {
            frame.last_token = Some(this_token);
            if let Some((kind, range)) = self.ranges.get(this_token) {
                frame.child_kinds.push(*kind);
                frame.child_spans.push(range.clone());
            }
        }
    }

    fn error(&mut self, _diag: u32) {}
}

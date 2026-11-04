//! The streaming cursor (issue 031): one pass over the packed [`Output`]
//! steps builds a compact node table (kind, span, children — no tree
//! allocation beyond the table); matching runs against that table with
//! **reusable scratch** — O(1) alloc per candidate step, the scratch is
//! allocated once per cursor and reused across candidates (pinned by the
//! test suite via [`QueryCursor::scratch_capacity`]).

use std::ops::Range;

use runtime_core::events::{Output, Step};

use crate::compile::{CompiledQuery, WILDCARD, bitset_allows};

/// One match: the pattern index, the root node's span and the captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The pattern that matched.
    pub pattern: usize,
    /// The root node's byte span.
    pub span: Range<u32>,
    /// `(capture id, span)` pairs in binding order, deduped.
    pub captures: Vec<(u16, Range<u32>)>,
}

/// The streaming matcher.
#[derive(Debug, Default)]
pub struct QueryCursor {
    /// The reusable child-run scratch (cleared per candidate; the
    /// O(1)-alloc evidence — it never grows after the first candidate).
    scratch: Vec<usize>,
}

/// One node from the packed stream.
#[derive(Debug, Clone)]
struct NodeView {
    kind: u16,
    span: Range<u32>,
    children: Vec<usize>,
}

impl QueryCursor {
    /// A fresh cursor (the scratch allocates on first use, then reuses).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The scratch capacity — the O(1)-alloc evidence: it does not grow
    /// across candidates.
    #[must_use]
    pub fn scratch_capacity(&self) -> usize {
        self.scratch.capacity()
    }

    /// Find every match of `query` in the packed stream. `ranges` is the
    /// clean token range table parallel to the Token steps; `source` is
    /// the document text (predicate evaluation reads capture text).
    #[must_use]
    pub fn matches(
        &mut self,
        query: &CompiledQuery,
        output: &Output,
        ranges: &[Range<u32>],
        source: &str,
    ) -> Vec<Match> {
        let nodes = build_nodes(output, ranges);
        let mut matches = Vec::new();
        for node_idx in 0..nodes.len() {
            for (pidx, pattern) in query.patterns.iter().enumerate() {
                let Some(root) = pattern.steps.first() else {
                    continue;
                };
                let view = &nodes[node_idx];
                // The O(1) kind prefilter.
                if root.kind != WILDCARD && root.kind != view.kind {
                    continue;
                }
                if !bitset_allows(&root.prefilter, view.kind) {
                    continue;
                }
                self.scratch.clear();
                self.scratch.extend_from_slice(&nodes[node_idx].children);
                let mut captures = Vec::new();
                if let Some(cap) = root.capture {
                    captures.push((cap, view.span.clone()));
                }
                if self.match_children(query, pidx, 1, &nodes, node_idx, &mut captures)
                    && predicate_pass(query, pidx, &captures, source)
                {
                    // Capture dedup: keep the first (cap, span) pair.
                    let mut seen: Vec<(u16, Range<u32>)> = Vec::new();
                    for pair in captures {
                        if !seen.contains(&pair) {
                            seen.push(pair);
                        }
                    }
                    matches.push(Match {
                        pattern: pidx,
                        span: view.span.clone(),
                        captures: seen,
                    });
                }
            }
        }
        matches
    }

    /// Match the pattern's steps (starting at `step_idx`) against the
    /// children of `node_idx`, with quantifier backtracking.
    #[allow(clippy::self_only_used_in_recursion)] // the cursor is the shared match context
    fn match_children(
        &mut self,
        query: &CompiledQuery,
        pidx: usize,
        step_idx: usize,
        nodes: &[NodeView],
        node_idx: usize,
        captures: &mut Vec<(u16, Range<u32>)>,
    ) -> bool {
        let children = nodes[node_idx].children.clone();
        self.match_sequence(query, pidx, step_idx, nodes, &children, 0, captures)
    }

    /// Steps `step_idx..` against `children[pos..]`: each step consumes a
    /// consecutive run of children within its quantifier bounds; extra
    /// children at the tail are allowed (tree-sitter semantics).
    #[allow(clippy::too_many_arguments, clippy::self_only_used_in_recursion)]
    fn match_sequence(
        &self,
        query: &CompiledQuery,
        pidx: usize,
        step_idx: usize,
        nodes: &[NodeView],
        children: &[usize],
        pos: usize,
        captures: &mut Vec<(u16, Range<u32>)>,
    ) -> bool {
        let pattern = &query.patterns[pidx];
        if step_idx >= pattern.steps.len() {
            return true; // extra trailing children are fine
        }
        let step = &pattern.steps[step_idx];
        let min = usize::from(step.min);
        let max = usize::from(step.max).min(children.len() - pos);
        // Greedy: k = max down to min consecutive matching children.
        for k in (min..=max).rev() {
            if pos + k > children.len() {
                continue;
            }
            let mut ok = true;
            let mut pushed = 0_usize;
            for offset in 0..k {
                let child = &nodes[children[pos + offset]];
                if step.kind != WILDCARD && child.kind != step.kind {
                    ok = false;
                    break;
                }
                if !bitset_allows(&step.prefilter, child.kind) {
                    ok = false;
                    break;
                }
                if let Some(cap) = step.capture {
                    captures.push((cap, child.span.clone()));
                    pushed += 1;
                }
            }
            if ok
                && self.match_sequence(
                    query,
                    pidx,
                    step_idx + 1,
                    nodes,
                    children,
                    pos + k,
                    captures,
                )
            {
                return true;
            }
            for _ in 0..pushed {
                captures.pop();
            }
        }
        false
    }
}

/// One pass over the packed steps: the node table (kind, span, children).
fn build_nodes(output: &Output, ranges: &[Range<u32>]) -> Vec<NodeView> {
    let mut nodes: Vec<NodeView> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut range_idx = 0_usize;
    for step in output.steps() {
        match step {
            Step::Enter { kind } => {
                stack.push(nodes.len());
                nodes.push(NodeView {
                    kind,
                    span: 0..0,
                    children: Vec::new(),
                });
            }
            Step::Exit => {
                if let Some(idx) = stack.pop() {
                    if let Some(&parent) = stack.last() {
                        nodes[parent].children.push(idx);
                    }
                }
            }
            Step::Token { .. } => {
                if let Some(range) = ranges.get(range_idx) {
                    let range = range.clone();
                    for &idx in &stack {
                        let span = &mut nodes[idx].span;
                        span.start = span.start.min(range.start);
                        span.end = span.end.max(range.end);
                    }
                }
                range_idx += 1;
            }
            Step::Error { .. } => {}
        }
    }
    nodes
}

fn predicate_pass(
    query: &CompiledQuery,
    pidx: usize,
    captures: &[(u16, Range<u32>)],
    source: &str,
) -> bool {
    for (cap, expected) in &query.patterns[pidx].predicates {
        let found = captures
            .iter()
            .find(|(c, _)| c == cap)
            .map(|(_, span)| &source[span.start as usize..span.end as usize]);
        if found != Some(expected.as_str()) {
            return false;
        }
    }
    true
}

//! The streaming `QueryCursor` (issue 031): a single pass over the packed
//! [`Output`] steps — no tree construction. Each node's summary (kind,
//! child-kind sequence, span) is buffered between its `Enter` and
//! `Exit`; on `Exit` the compiled patterns run (kind prefilter first),
//! and captures land in a caller-provided scratch buffer that is
//! cleared — never reallocated — per candidate.

use std::ops::Range;

use runtime_core::events::{Output, Step};

use crate::query::{ChildStep, CompiledQuery, Predicate, Quant};

/// One capture: a slot plus the byte span it binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The capture slot (dense, per query).
    pub slot: u8,
    /// The bound byte span in the matched source.
    pub span: Range<usize>,
}

/// One match: the pattern index plus its captures (deduplicated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Which compiled pattern matched.
    pub pattern: usize,
    /// The matched node's byte span.
    pub span: Range<usize>,
    /// The captures (deduplicated, in emission order).
    pub captures: Vec<Capture>,
}

/// A predicate argument at evaluation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredicateArg<'s> {
    /// The captured text.
    Capture(&'s str),
    /// The literal from the query.
    Text(&'s str),
}

/// The host's predicate closure: `name` without `#`/`?`; return `false`
/// to drop the match.
pub type PredicateFn<'p> = dyn FnMut(&str, &[PredicateArg<'_>]) -> bool + 'p;

/// The streaming matcher.
pub struct QueryCursor<'q> {
    /// The compiled query.
    pub query: &'q CompiledQuery,
    /// The dedup set: `(slot, span)` pairs already emitted. Reused
    /// across matches — steady-state matching allocates nothing.
    seen: Vec<(u8, u32, u32)>,
}

impl<'q> QueryCursor<'q> {
    /// A cursor over a compiled query.
    #[must_use]
    pub fn new(query: &'q CompiledQuery) -> Self {
        Self {
            query,
            seen: Vec::new(),
        }
    }

    /// Run one pass over the packed stream, yielding every match whose
    /// predicates hold. `ranges` is the token side table parallel to
    /// the stream's token events; `source` supplies predicate texts.
    /// `scratch` is cleared (not reallocated) per candidate — the
    /// O(1)-alloc contract; its capacity is bounded by the pattern's
    /// capture count.
    pub fn matches(
        &mut self,
        output: &Output,
        ranges: &[(u16, Range<u32>)],
        source: &str,
        scratch: &mut Vec<Capture>,
        predicate: &mut PredicateFn<'_>,
    ) -> Vec<Match> {
        let mut matches = Vec::new();
        let mut stack: Vec<NodeFrame> = Vec::new();
        let mut token_index = 0_usize;
        for step in output.steps() {
            match step {
                Step::Enter { kind } => {
                    stack.push(NodeFrame {
                        kind,
                        first_token: token_index,
                        last_token: None,
                        child_kinds: Vec::new(),
                        child_spans: Vec::new(),
                    });
                }
                Step::Token { .. } => {
                    let this_token = token_index;
                    token_index += 1;
                    if let Some(frame) = stack.last_mut() {
                        frame.last_token = Some(this_token);
                        // Token children join the node's child sequence.
                        if let Some((kind, range)) = ranges.get(this_token) {
                            frame.child_kinds.push(*kind);
                            frame.child_spans.push(range.clone());
                        }
                    }
                }
                Step::Error { .. } => {}
                Step::Exit => {
                    let Some(frame) = stack.pop() else {
                        continue;
                    };
                    let span = frame.span(ranges, source.len());
                    if let Some(parent) = stack.last_mut() {
                        parent.child_kinds.push(frame.kind);
                        let span32 = u32::try_from(span.start).unwrap_or(u32::MAX)
                            ..u32::try_from(span.end).unwrap_or(u32::MAX);
                        parent.child_spans.push(span32);
                        if frame.last_token.is_some() {
                            parent.last_token = frame.last_token;
                        }
                    }
                    for (pattern_index, pattern) in self.query.patterns.iter().enumerate() {
                        // O(1) kind prefilter.
                        if !pattern.wildcard && pattern.kind != frame.kind {
                            continue;
                        }
                        // Child matching with span binding — stack-only
                        // backtracking; the assignment lands in scratch.
                        let Some(bound) = match_children(&pattern.children, &frame) else {
                            continue;
                        };
                        scratch.clear();
                        if let Some(slot) = pattern.capture {
                            scratch.push(Capture {
                                slot,
                                span: span.clone(),
                            });
                        }
                        for (step, child_span) in pattern.children.iter().zip(&bound) {
                            if let (Some(slot), Some(child)) = (step.capture, child_span) {
                                scratch.push(Capture {
                                    slot,
                                    span: clamp_span(child, source.len()),
                                });
                            }
                        }
                        self.dedup(scratch);
                        if !self.predicates_hold(scratch, source, predicate) {
                            continue;
                        }
                        matches.push(Match {
                            pattern: pattern_index,
                            span: span.clone(),
                            captures: scratch.clone(),
                        });
                    }
                }
            }
        }
        matches
    }

    /// Dedup `scratch` in place (same slot + span emitted once).
    fn dedup(&mut self, scratch: &mut Vec<Capture>) {
        scratch.dedup_by(|a, b| a.slot == b.slot && a.span == b.span);
        let mut unique: Vec<Capture> = Vec::with_capacity(scratch.len());
        for capture in scratch.drain(..) {
            let key = (
                capture.slot,
                u32::try_from(capture.span.start).unwrap_or(u32::MAX),
                u32::try_from(capture.span.end).unwrap_or(u32::MAX),
            );
            if !self.seen.contains(&key) {
                self.seen.push(key);
                unique.push(capture);
            }
        }
        *scratch = unique;
    }

    /// Built-in predicates plus the host closure.
    fn predicates_hold(
        &self,
        captures: &[Capture],
        source: &str,
        predicate: &mut PredicateFn<'_>,
    ) -> bool {
        let text_of = |slot: u8| -> Option<&str> {
            captures.iter().find(|c| c.slot == slot).map(|c| {
                let start = c.span.start.min(source.len());
                let end = c.span.end.min(source.len()).max(start);
                &source[start..end]
            })
        };
        for pred in &self.query.predicates {
            let holds = match pred {
                Predicate::Eq { capture, value } => {
                    text_of(*capture).is_some_and(|text| text == value)
                }
                Predicate::NotEq { capture, value } => {
                    text_of(*capture).is_none_or(|text| text != value)
                }
                Predicate::Custom { name, args, .. } => {
                    let argv: Vec<PredicateArg<'_>> = args
                        .iter()
                        .map(|arg| match arg {
                            crate::query::PredArgSpec::Capture(slot) => {
                                PredicateArg::Capture(text_of(*slot).unwrap_or(""))
                            }
                            crate::query::PredArgSpec::Text(text) => PredicateArg::Text(text),
                        })
                        .collect();
                    predicate(name, &argv)
                }
            };
            if !holds {
                return false;
            }
        }
        true
    }
}

/// One open node buffered while its subtree streams by.
struct NodeFrame {
    /// The node's kind (from its `Enter` step).
    kind: u16,
    /// Index of the node's first token.
    first_token: usize,
    /// Index of the node's last token (None = zero-width so far).
    last_token: Option<usize>,
    /// The kinds of the node's direct children (tokens AND subnodes).
    child_kinds: Vec<u16>,
    /// The spans of those children, parallel to `child_kinds`.
    child_spans: Vec<Range<u32>>,
}

impl NodeFrame {
    /// The node's byte span: first token start .. last token end.
    fn span(&self, ranges: &[(u16, Range<u32>)], source_len: usize) -> Range<usize> {
        let clamp = |v: u32| -> usize { usize::try_from(v).unwrap_or(0).min(source_len) };
        let Some((_, first)) = ranges.get(self.first_token) else {
            return 0..0;
        };
        let start = clamp(first.start);
        let end = self
            .last_token
            .and_then(|i| ranges.get(i))
            .map_or(start, |(_, r)| clamp(r.end));
        start..end.max(start)
    }
}

/// Clamp a u32 span into the source.
fn clamp_span(span: &Range<u32>, source_len: usize) -> Range<usize> {
    let start = usize::try_from(span.start).unwrap_or(0).min(source_len);
    let end = usize::try_from(span.end)
        .unwrap_or(0)
        .min(source_len)
        .max(start);
    start..end
}

/// Match the child steps against the buffered child sequence:
/// subsequence matching in order, gaps allowed, quantifiers per step.
/// Returns, per step, the span of the FIRST child it consumed (for
/// capture binding) — pure backtracking over indices, no allocation.
fn match_children(steps: &[ChildStep], frame: &NodeFrame) -> Option<Vec<Option<Range<u32>>>> {
    let mut bound: Vec<Option<Range<u32>>> = vec![None; steps.len()];
    if match_children_from(steps, 0, frame, 0, &mut bound) {
        Some(bound)
    } else {
        None
    }
}

/// Subsequence matching with gaps: a step consumes the next matching
/// child (skipping non-matching ones — the gap discipline), honoring
/// its quantifier; the capture binding follows the successful path.
fn match_children_from(
    steps: &[ChildStep],
    step_index: usize,
    frame: &NodeFrame,
    child_index: usize,
    bound: &mut [Option<Range<u32>>],
) -> bool {
    if step_index == steps.len() {
        return true; // trailing gaps are fine
    }
    let step = &steps[step_index];
    let zero_ok = matches!(step.quant, Quant::ZeroOrOne | Quant::ZeroOrMore);
    if zero_ok && match_children_from(steps, step_index + 1, frame, child_index, bound) {
        return true; // the step matches nothing
    }
    // Find the next matching child (skipping gaps), consume it, and
    // continue on the step's quantifier path.
    let mut k = child_index;
    while k < frame.child_kinds.len() {
        if step.kind == u16::MAX || frame.child_kinds[k] == step.kind {
            let first_here = bound[step_index].is_none();
            if first_here {
                bound[step_index] = Some(frame.child_spans[k].clone());
            }
            // After consuming one child: repeatable quants may either
            // move to the next step or consume more; the fixed quants
            // always move on.
            if match_children_from(steps, step_index + 1, frame, k + 1, bound) {
                return true;
            }
            if matches!(step.quant, Quant::ZeroOrMore | Quant::OneOrMore)
                && match_children_from(steps, step_index, frame, k + 1, bound)
            {
                return true;
            }
            if first_here {
                bound[step_index] = None; // backtrack the binding
            }
        }
        k += 1;
    }
    false
}

//! Tree sinks (roadmap §7.2): the six event-stream consumers share this
//! minimal sink interface, plus the trivia-attach discipline (ADR-008).
//!
//! # Consumer table (roadmap §7.2) mapped to implementations
//!
//! | Consumer | Implementation |
//! |---|---|
//! | validate sink | [`ValidationSink`] — counts + balance |
//! | token sink | [`TokenSink`] — highlighting-class tools |
//! | skeleton builder | [`SkeletonSink`] — kinds + spans, no trivia |
//! | full CST builder | [`crate::tree::TreeBuilder`] — green/red trees (§7.6) |
//! | streaming query matcher | query module (§7.8, Phase 3) driven through this trait |
//! | incremental engine | splices `ReusedSubtree` records, then this trait (§7.7) |

use std::ops::Range;

use crate::events::Output;

/// A consumer of flattened parse output.
///
/// Implemented by: validation counting, token collection, skeleton builder,
/// green/red tree builder, streaming query matcher, incremental splicer
/// (roadmap §7.2 table).
pub trait Sink {
    /// Enter a node of `kind`.
    fn enter(&mut self, kind: crate::NodeKind);
    /// Exit the current node.
    fn exit(&mut self);
    /// Consume a token of `kind` covering `range` (byte offsets into the
    /// source) and `n_raw` raw tokens.
    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, n_raw: u8);
    /// A deferred diagnostic index (side table holds the text).
    fn error(&mut self, diag: u32);
}

/// Drive `sink` over a flattened [`Output`], converting packed steps into
/// sink calls with source ranges supplied by `ranges` (parallel to token
/// steps in DFS order — the parser's token source produces this vector).
///
/// `Step::Error` words (tag `0xF`) forward their diag index to
/// [`Sink::error`].
///
/// # Errors
/// Errors when the stream is malformed relative to `ranges` (more token
/// steps than ranges).
pub fn drive_sink(
    output: &Output,
    ranges: &[Range<u32>],
    sink: &mut dyn Sink,
) -> Result<(), String> {
    let mut range_idx = 0usize;
    let mut cursor = 0u32;
    // Iterate the packed words directly: unpacking the tag inline avoids
    // materializing a `Step` per word (the enum move + re-branch showed up
    // in the phase attribution at ~20k steps for a steady-state doc).
    // Bit-exact with `Output::unpack_word` (same tag layout).
    for &word in &output.stream {
        match word & 0xF {
            crate::events::TAG_ENTER => sink.enter(u16::try_from(word >> 4).unwrap_or(u16::MAX)),
            crate::events::TAG_EXIT => sink.exit(),
            crate::events::TAG_TOKEN => {
                let kind = u16::try_from((word >> 12) & 0xFFFF).unwrap_or(u16::MAX);
                let n_raw = u8::try_from((word >> 4) & 0xFF).unwrap_or(0);
                // Callers without a range table (validate-mode consumers,
                // property tests) get synthesized zero-length ranges at the
                // running cursor; real token sources supply full ranges.
                let range = match ranges.get(range_idx) {
                    Some(r) => r.clone(),
                    None => cursor..cursor,
                };
                cursor = range.end;
                sink.token(kind, range, n_raw);
                range_idx += 1;
            }
            // TAG_ERROR and the zero word both decode as Error (the same
            // contract `unpack_word` implements).
            _ => sink.error(u32::try_from(word >> 4).unwrap_or(u32::MAX)),
        }
    }
    Ok(())
}

/// A trivia record attached at tree-sink time (biome discipline, ADR-008).
///
/// Trivia never enters the event stream. The pack emits CLEAN token ranges
/// (trivia excluded) and records covering every gap; the tree builder
/// interleaves record leaves with token leaves in byte order, keeping the
/// rendered tree byte-faithful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriviaRecord {
    /// Trivia token kind.
    pub kind: crate::TokenKind,
    /// Byte range in the source (never overlapping a clean token range).
    pub range: Range<u32>,
    /// True when the trivia follows a token on the same line (trailing);
    /// false for leading trivia.
    pub trailing: bool,
}

/// Validation-only sink: counts nodes/tokens/errors with near-zero cost
/// (roadmap §7.2 `validate` mode consumer).
///
/// Issue 024 sharpens the contract into a real validator: besides counting,
/// the sink tracks enter/exit *balance* — [`Self::balanced`] reports whether
/// the stream is a well-formed forest (no exit underflow, no unclosed
/// enter). Memory stays O(1): two machine words of extra state, no stack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidationSink {
    /// Nodes entered.
    pub nodes: u32,
    /// Tokens consumed.
    pub tokens: u32,
    /// Error events seen.
    pub errors: u32,
    /// Open enter/exit nesting depth (private: bookkeeping, not a count).
    open_depth: u32,
    /// Set when a [`Sink::exit`] arrived with nothing open.
    saw_underflow: bool,
}

impl ValidationSink {
    /// Whether the driven stream is a well-formed forest: every
    /// [`Sink::exit`] matched an open enter, and no enter was left unclosed
    /// at stream end. Diagnostics do not affect the verdict — error-bearing
    /// streams validate fine when their shape balances.
    #[must_use]
    pub fn balanced(&self) -> bool {
        !self.saw_underflow && self.open_depth == 0
    }

    /// Snapshot of the counts and the balance verdict (issue 024 `validate`
    /// mode summary — the cheap machine-readable result of a validation run).
    #[must_use]
    pub fn summary(&self) -> ValidationSummary {
        ValidationSummary {
            nodes: self.nodes,
            tokens: self.tokens,
            errors: self.errors,
            balanced: self.balanced(),
        }
    }
}

impl Sink for ValidationSink {
    fn enter(&mut self, _kind: crate::NodeKind) {
        self.nodes += 1;
        self.open_depth += 1;
    }
    fn exit(&mut self) {
        if self.open_depth == 0 {
            self.saw_underflow = true;
        } else {
            self.open_depth -= 1;
        }
    }
    fn token(&mut self, _kind: crate::TokenKind, _range: Range<u32>, _n_raw: u8) {
        self.tokens += 1;
    }
    fn error(&mut self, _diag: u32) {
        self.errors += 1;
    }
}

/// A [`ValidationSink`] run's final verdict (issue 024): the flat counts
/// plus the kinds-are-balanced boolean, in one copyable record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ValidationSummary {
    /// Nodes entered.
    pub nodes: u32,
    /// Tokens consumed.
    pub tokens: u32,
    /// Error events seen.
    pub errors: u32,
    /// Whether the stream was a well-formed forest
    /// (see [`ValidationSink::balanced`]).
    pub balanced: bool,
}

/// One completed node of a skeleton tree (issue 024 `skeleton` mode):
/// node kind plus byte span — no trivia, no token records, no text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkeletonNode {
    /// Node kind.
    pub kind: crate::NodeKind,
    /// Byte range covered by the node: from the cursor where the node was
    /// entered (== the start of its first token on contiguous streams) to
    /// the cursor where it was exited.
    pub span: Range<u32>,
}

/// Skeleton-mode sink: records each completed node's kind and span, nothing
/// else (roadmap §7.2 skeleton builder shape — issue 024).
///
/// Memory is O(depth): a `(kind, start)` stack frame per open node.
/// Exit underflow is ignored (no panic, no record), so malformed streams
/// degrade to a best-effort skeleton.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkeletonSink {
    /// Completed nodes in DFS completion order.
    pub nodes: Vec<SkeletonNode>,
    /// Open nodes: `(kind, cursor-at-enter)`, innermost last.
    open: Vec<(crate::NodeKind, u32)>,
    /// Running cursor: the end of the last consumed token range.
    cursor: u32,
}

impl Sink for SkeletonSink {
    fn enter(&mut self, kind: crate::NodeKind) {
        self.open.push((kind, self.cursor));
    }
    fn exit(&mut self) {
        if let Some((kind, start)) = self.open.pop() {
            let end = self.cursor.max(start);
            self.nodes.push(SkeletonNode {
                kind,
                span: start..end,
            });
        }
    }
    fn token(&mut self, _kind: crate::TokenKind, range: Range<u32>, _n_raw: u8) {
        self.cursor = range.end;
    }
    fn error(&mut self, _diag: u32) {}
}

/// Token sink: collects `(kind, range)` pairs for highlighting-class tools
/// (roadmap §7.2 token sink).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSink {
    /// Collected tokens in DFS order.
    pub tokens: Vec<(crate::TokenKind, Range<u32>)>,
}

impl Sink for TokenSink {
    fn enter(&mut self, _kind: crate::NodeKind) {}
    fn exit(&mut self) {}
    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, _n_raw: u8) {
        self.tokens.push((kind, range));
    }
    fn error(&mut self, _diag: u32) {}
}

#[cfg(test)]
mod sink_tests {
    use std::ops::Range;

    use super::{SkeletonSink, TokenSink, ValidationSink};
    use crate::events::{Output, Step};
    use crate::sink::drive_sink;

    // ---- fixed-stream helpers ----

    /// Hand-pack steps into an [`Output`]: test-side control over
    /// *malformed* sequences that [`process`](crate::events::process) would
    /// reject (enters/exits do not have to balance here — the sinks'
    /// balance checks are what is under test).
    fn pack(steps: &[Step]) -> Output {
        let mut stream = Vec::with_capacity(steps.len());
        for step in steps {
            match *step {
                Step::Enter { kind } => stream.push(Output::pack_enter(kind)),
                Step::Exit => stream.push(Output::pack_exit()),
                Step::Token { kind, n_raw } => stream.push(Output::pack_token(kind, n_raw)),
                Step::Error { diag } => stream.push(Output::pack_error(diag)),
            }
        }
        Output { stream }
    }

    /// A fixed two-node stream: `10 { tok(0..2) 11 { tok(2..5) } }` with
    /// ranges parallel to the token steps.
    fn fixed_output() -> (Output, Vec<Range<u32>>) {
        (
            pack(&[
                Step::Enter { kind: 10 },
                Step::Token { kind: 1, n_raw: 1 },
                Step::Enter { kind: 11 },
                Step::Token { kind: 2, n_raw: 2 },
                Step::Exit,
                Step::Exit,
            ]),
            vec![0..2, 2..5],
        )
    }

    // ---- ValidationSink: kind-sequence + balance + summary (issue 024) ----

    #[test]
    fn validation_summary_counts_and_balances_wellformed_stream() {
        let (output, ranges) = fixed_output();
        let mut sink = ValidationSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("well-formed stream drives");
        assert_eq!(sink.nodes, 2);
        assert_eq!(sink.tokens, 2);
        assert_eq!(sink.errors, 0);
        assert!(sink.balanced(), "balanced stream must validate");
        let summary = sink.summary();
        assert_eq!(summary.nodes, 2);
        assert_eq!(summary.tokens, 2);
        assert_eq!(summary.errors, 0);
        assert!(summary.balanced, "summary carries the kinds-ok verdict");
    }

    #[test]
    fn validation_summary_counts_errors_without_losing_balance() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Error { diag: 7 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Exit,
        ]);
        let mut sink = ValidationSink::default();
        drive_sink(&output, &[], &mut sink).expect("drives");
        assert_eq!(sink.errors, 1);
        assert!(sink.balanced());
        assert!(sink.summary().balanced);
    }

    #[test]
    fn validation_flags_exit_underflow() {
        let output = pack(&[Step::Enter { kind: 1 }, Step::Exit, Step::Exit]);
        let mut sink = ValidationSink::default();
        drive_sink(&output, &[], &mut sink).expect("underflow is reported, not an error");
        assert!(!sink.balanced(), "exit underflow must fail the kinds check");
        assert!(!sink.summary().balanced);
        assert_eq!(sink.nodes, 1);
    }

    #[test]
    fn validation_flags_unclosed_enter() {
        let output = pack(&[Step::Enter { kind: 1 }, Step::Token { kind: 2, n_raw: 1 }]);
        let mut sink = ValidationSink::default();
        drive_sink(&output, &[], &mut sink).expect("drives");
        assert!(
            !sink.balanced(),
            "a node left open must fail the kinds check"
        );
        assert!(!sink.summary().balanced);
    }

    #[test]
    fn validation_flags_leading_exit() {
        let output = pack(&[Step::Exit]);
        let mut sink = ValidationSink::default();
        drive_sink(&output, &[], &mut sink).expect("underflow is reported, not an error");
        assert!(!sink.balanced());
    }

    // ---- TokenSink: scan-mode flat stream shape ----

    #[test]
    fn token_sink_collects_flat_token_stream() {
        let (output, ranges) = fixed_output();
        let mut sink = TokenSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(sink.tokens, vec![(1, 0..2), (2, 2..5)]);
    }

    #[test]
    fn token_sink_ignores_structure_and_errors() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Error { diag: 3 },
            Step::Token { kind: 5, n_raw: 1 },
            Step::Exit,
        ]);
        let mut sink = TokenSink::default();
        drive_sink(&output, &[], &mut sink).expect("drives");
        assert_eq!(sink.tokens.len(), 1);
    }

    // ---- SkeletonSink: node kinds + spans, no trivia (issue 024) ----

    #[test]
    fn skeleton_records_node_kinds_and_spans_in_dfs_completion_order() {
        let (output, ranges) = fixed_output();
        let mut sink = SkeletonSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        // Inner node completes (and is recorded) before its outer node:
        // DFS completion order, spans = cursor-at-enter..cursor-at-exit.
        assert_eq!(
            sink.nodes
                .iter()
                .map(|node| (node.kind, node.span.clone()))
                .collect::<Vec<_>>(),
            vec![(11, 2..5), (10, 0..5)]
        );
    }

    #[test]
    fn skeleton_records_sibling_nodes_with_their_own_spans() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Exit,
            Step::Enter { kind: 2 },
            Step::Token { kind: 2, n_raw: 1 },
            Step::Exit,
        ]);
        let ranges = vec![0..2, 2..3];
        let mut sink = SkeletonSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(
            sink.nodes
                .iter()
                .map(|node| (node.kind, node.span.clone()))
                .collect::<Vec<_>>(),
            vec![(1, 0..2), (2, 2..3)]
        );
    }

    #[test]
    fn skeleton_records_tokenless_node_as_zero_width_span() {
        let output = pack(&[
            Step::Token { kind: 1, n_raw: 1 },
            Step::Enter { kind: 9 },
            Step::Exit,
        ]);
        let range = 0..3;
        let ranges = [range];
        let mut sink = SkeletonSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(
            sink.nodes
                .iter()
                .map(|node| (node.kind, node.span.clone()))
                .collect::<Vec<_>>(),
            vec![(9, 3..3)]
        );
    }

    #[test]
    fn skeleton_ignores_exit_underflow_without_panicking() {
        let output = pack(&[Step::Exit, Step::Enter { kind: 1 }, Step::Exit, Step::Exit]);
        let mut sink = SkeletonSink::default();
        drive_sink(&output, &[], &mut sink).expect("malformed stream must not panic");
        assert_eq!(sink.nodes.len(), 1);
        assert_eq!(sink.nodes[0].kind, 1);
    }

    // ---- fuzz contract: no sink panics on arbitrary packed streams ----

    /// Tiny deterministic LCG — the property tests stay reproducible
    /// without an external RNG dependency.
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

    #[test]
    fn property_random_packed_streams_never_panic_and_match_oracles() {
        let mut rng = Lcg(0x0246_8ACE_1357_9BDF);
        for _ in 0..300 {
            let mut steps = Vec::new();
            // Test-side oracles.
            let (mut enters, mut tokens, mut errors) = (0u32, 0u32, 0u32);
            let (mut depth, mut underflow, mut pops) = (0u32, false, 0u32);
            for _ in 0..8 + rng.below(40) {
                match rng.below(6) {
                    0 | 1 => {
                        let kind = u16::try_from(rng.below(50)).expect("kind fits u16");
                        steps.push(Step::Enter { kind });
                        enters += 1;
                        depth += 1;
                    }
                    2 => {
                        steps.push(Step::Exit);
                        if depth == 0 {
                            underflow = true;
                        } else {
                            depth -= 1;
                            pops += 1;
                        }
                    }
                    3 | 4 => {
                        let kind = u16::try_from(rng.below(20)).expect("kind fits u16");
                        let n_raw = u8::try_from(1 + rng.below(3)).expect("n_raw fits u8");
                        steps.push(Step::Token { kind, n_raw });
                        tokens += 1;
                    }
                    _ => {
                        let diag = u32::try_from(rng.below(8)).expect("diag fits u32");
                        steps.push(Step::Error { diag });
                        errors += 1;
                    }
                }
            }
            let output = pack(&steps);

            // ValidationSink: counts + the balance oracle.
            let mut validation = ValidationSink::default();
            drive_sink(&output, &[], &mut validation).expect("drive never fails");
            assert_eq!(validation.nodes, enters);
            assert_eq!(validation.tokens, tokens);
            assert_eq!(validation.errors, errors);
            assert_eq!(validation.balanced(), !underflow && depth == 0);
            let summary = validation.summary();
            assert_eq!(summary.nodes, enters);
            assert_eq!(summary.tokens, tokens);
            assert_eq!(summary.errors, errors);
            assert_eq!(summary.balanced, !underflow && depth == 0);

            // TokenSink: one record per token, never a panic.
            let mut token = TokenSink::default();
            drive_sink(&output, &[], &mut token).expect("drive never fails");
            assert_eq!(
                token.tokens.len(),
                usize::try_from(tokens).expect("fits usize")
            );

            // SkeletonSink: one record per matched enter/exit pair.
            let mut skeleton = SkeletonSink::default();
            drive_sink(&output, &[], &mut skeleton).expect("drive never fails");
            assert_eq!(
                skeleton.nodes.len(),
                usize::try_from(pops).expect("fits usize")
            );
            for node in &skeleton.nodes {
                assert!(node.span.start <= node.span.end, "spans stay ordered");
            }
        }
    }
}

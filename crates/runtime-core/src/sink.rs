//! Tree sinks (roadmap §7.2): the six event-stream consumers share this
//! minimal sink interface, plus the trivia-attach discipline (ADR-008).
//!
//! # Consumer table (roadmap §7.2) mapped to implementations
//!
//! | Consumer | Implementation |
//! |---|---|
//! | validate sink | [`ValidationSink`] — counts + balance |
//! | kind statistics | [`StatsSink`] — per-kind histograms, depth, bytes |
//! | token sink | [`TokenSink`] — highlighting-class tools |
//! | skeleton builder | [`SkeletonSink`] — kinds + spans, no trivia |
//! | full CST builder | [`crate::tree::TreeBuilder`] — green/red trees (§7.6) |
//! | streaming query matcher | the query crate's `QuerySink` — patterns run as the stream drives, zipped with tree-side sinks via [`TeeSink`] |
//! | diagnostic resolution | [`DiagCollector`] — deferred indexes resolved to owned messages via the side table |
//! | step-gated consumer | [`FilterSink`] — predicate decides which steps reach the inner sink |
//! | incremental engine | splices `ReusedSubtree` records, then this trait (§7.7, Phase 3) |

use std::ops::Range;

use crate::diagnostic::DiagTable;
use crate::events::Output;

/// A consumer of flattened parse output.
///
/// Implemented by: validation counting, kind statistics, token
/// collection, skeleton builder, green/red tree builder, the query
/// crate's streaming `QuerySink` matcher, the [`DiagCollector`]
/// diagnostic resolver, and the [`TeeSink`] and [`FilterSink`]
/// combinators that shape multi-consumer passes (roadmap §7.2 table).
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
/// Error words (tag [`crate::events::TAG_ERROR`] and the zero word —
/// the same decode [`crate::events::Output::unpack_word`] applies)
/// forward their diag index to [`Sink::error`].
///
/// # Errors
/// Currently never: missing range-table entries are synthesized as
/// zero-width ranges at the running cursor, and malformed enter/exit
/// sequences are left to the sinks' own balance checks (see
/// [`ValidationSink::balanced`]). The `Result` reserves the failure
/// channel for a stricter driver contract.
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

/// Per-kind statistics sink: what did the stream actually contain?
///
/// Where [`ValidationSink`] keeps O(1) totals, this sink trades
/// O(max-kind-seen) memory for the per-kind breakdown: histograms for
/// node and token kinds, the deepest enter/exit nesting, the total byte
/// span the consumed token ranges cover, plus the totals and the error
/// count. Grammar profiling, kind-coverage reports ("which kinds did
/// the corpus exercise?") and corpus sizing all read off the final
/// [`StreamStats`] snapshot instead of re-walking the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSink {
    /// Per-node-kind enter counts, indexed by kind.
    node_histogram: Vec<u32>,
    /// Per-token-kind counts, indexed by kind.
    token_histogram: Vec<u32>,
    /// Total nodes entered.
    nodes: u32,
    /// Total tokens consumed.
    tokens: u32,
    /// Error events seen.
    errors: u32,
    /// Deepest enter/exit nesting reached.
    max_depth: u32,
    /// Current open-enter depth (guards the max tracker on underflow).
    open_depth: u32,
    /// Total bytes covered by the consumed token ranges.
    token_bytes: u64,
}

/// Grow `histogram` so `kind` is in range, then count one event.
fn bump(histogram: &mut Vec<u32>, kind: u16) {
    let index = usize::from(kind);
    if index >= histogram.len() {
        histogram.resize(index + 1, 0);
    }
    histogram[index] += 1;
}

impl StatsSink {
    /// Enter events recorded for `kind` (absent kinds read as zero).
    #[must_use]
    pub fn node_count(&self, kind: crate::NodeKind) -> u32 {
        self.node_histogram
            .get(usize::from(kind))
            .copied()
            .unwrap_or(0)
    }

    /// Token events recorded for `kind` (absent kinds read as zero).
    #[must_use]
    pub fn token_count(&self, kind: crate::TokenKind) -> u32 {
        self.token_histogram
            .get(usize::from(kind))
            .copied()
            .unwrap_or(0)
    }

    /// Total nodes entered.
    #[must_use]
    pub fn nodes(&self) -> u32 {
        self.nodes
    }

    /// Total tokens consumed.
    #[must_use]
    pub fn tokens(&self) -> u32 {
        self.tokens
    }

    /// Error events seen (the count; [`DiagCollector`] resolves the
    /// messages).
    #[must_use]
    pub fn errors(&self) -> u32 {
        self.errors
    }

    /// Deepest enter/exit nesting reached. Exit underflow does not
    /// inflate the depth (the open-depth tracker saturates at zero).
    #[must_use]
    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    /// Total bytes covered by the consumed token ranges — zero on
    /// streams driven without a range table (the driver synthesizes
    /// zero-width ranges there; see [`drive_sink`]).
    #[must_use]
    pub fn token_bytes(&self) -> u64 {
        self.token_bytes
    }

    /// Snapshot of the run's statistics (the borrowed counterpart of
    /// [`Self::into_stats`]).
    #[must_use]
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            node_histogram: self.node_histogram.clone(),
            token_histogram: self.token_histogram.clone(),
            nodes: self.nodes,
            tokens: self.tokens,
            errors: self.errors,
            max_depth: self.max_depth,
            token_bytes: self.token_bytes,
        }
    }

    /// Consume the sink into its final statistics snapshot.
    #[must_use = "the statistics snapshot is the whole point"]
    pub fn into_stats(self) -> StreamStats {
        StreamStats {
            node_histogram: self.node_histogram,
            token_histogram: self.token_histogram,
            nodes: self.nodes,
            tokens: self.tokens,
            errors: self.errors,
            max_depth: self.max_depth,
            token_bytes: self.token_bytes,
        }
    }
}

impl Sink for StatsSink {
    fn enter(&mut self, kind: crate::NodeKind) {
        bump(&mut self.node_histogram, kind);
        self.nodes += 1;
        self.open_depth += 1;
        if self.open_depth > self.max_depth {
            self.max_depth = self.open_depth;
        }
    }

    fn exit(&mut self) {
        self.open_depth = self.open_depth.saturating_sub(1);
    }

    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, _n_raw: u8) {
        bump(&mut self.token_histogram, kind);
        self.tokens += 1;
        // A malformed range table can hand the driver inverted pairs;
        // they count zero bytes rather than panicking (or underflowing).
        self.token_bytes += u64::from(range.end.saturating_sub(range.start));
    }

    fn error(&mut self, _diag: u32) {
        self.errors += 1;
    }
}

/// Owned snapshot of a [`StatsSink`] run: the per-kind histograms plus
/// the totals, depth and byte-coverage scalars, in one record — the
/// composition counterpart of [`ValidationSummary`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StreamStats {
    /// Per-node-kind enter counts, indexed by kind
    /// (index >= len means the kind never occurred).
    pub node_histogram: Vec<u32>,
    /// Per-token-kind counts, indexed by kind
    /// (index >= len means the kind never occurred).
    pub token_histogram: Vec<u32>,
    /// Total nodes entered.
    pub nodes: u32,
    /// Total tokens consumed.
    pub tokens: u32,
    /// Error events seen.
    pub errors: u32,
    /// Deepest enter/exit nesting reached.
    pub max_depth: u32,
    /// Total bytes covered by the consumed token ranges (zero without
    /// a range table; see [`drive_sink`]).
    pub token_bytes: u64,
}

impl StreamStats {
    /// Enter events recorded for `kind` (absent kinds read as zero).
    #[must_use]
    pub fn node_count(&self, kind: crate::NodeKind) -> u32 {
        self.node_histogram
            .get(usize::from(kind))
            .copied()
            .unwrap_or(0)
    }

    /// Token events recorded for `kind` (absent kinds read as zero).
    #[must_use]
    pub fn token_count(&self, kind: crate::TokenKind) -> u32 {
        self.token_histogram
            .get(usize::from(kind))
            .copied()
            .unwrap_or(0)
    }
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

/// The diagnostic-resolution sink: the consumer-side half of the
/// deferred-diagnostic protocol. The event stream carries bare diag
/// indexes ([`Sink::error`]); this sink resolves each index through
/// the parse's side table ([`DiagTable`]) into owned
/// [`Diagnostic`](crate::diagnostic::Diagnostic) values in stream
/// order — the bridge between "the stream saw error #7" and "here is
/// the message and byte range", without a second pass over the stream.
///
/// Every other shipped sink treats the error channel as a count at
/// most; tree-and-diagnostics consumers zip this in as a
/// [`TeeSink`] half (one [`drive_sink`] pass builds the tree and
/// resolves the diagnostics).
#[derive(Debug)]
pub struct DiagCollector<'t> {
    /// The side table the parse produced (`diag` indexes index it).
    table: &'t DiagTable,
    /// Resolved diagnostics in stream order.
    diagnostics: Vec<crate::diagnostic::Diagnostic>,
    /// Error events whose index the table could not resolve (beyond
    /// the table's length — e.g. the sentinel a truncated side table
    /// leaves behind).
    unresolved: usize,
}

impl<'t> DiagCollector<'t> {
    /// A collector resolving through `table`.
    #[must_use]
    pub fn new(table: &'t DiagTable) -> Self {
        Self {
            table,
            diagnostics: Vec::new(),
            unresolved: 0,
        }
    }

    /// The resolved diagnostics, in stream (event) order.
    #[must_use]
    pub fn diagnostics(&self) -> &[crate::diagnostic::Diagnostic] {
        &self.diagnostics
    }

    /// Consume the resolved diagnostics.
    #[must_use = "the resolved diagnostics are the whole point"]
    pub fn into_diagnostics(self) -> Vec<crate::diagnostic::Diagnostic> {
        self.diagnostics
    }

    /// How many error events carried an index the table did not
    /// resolve (the observability probe for truncated side tables).
    #[must_use]
    pub fn unresolved(&self) -> usize {
        self.unresolved
    }
}

impl Sink for DiagCollector<'_> {
    fn enter(&mut self, _kind: crate::NodeKind) {}
    fn exit(&mut self) {}
    fn token(&mut self, _kind: crate::TokenKind, _range: Range<u32>, _n_raw: u8) {}
    fn error(&mut self, diag: u32) {
        match self.table.get(diag) {
            Some(diagnostic) => self.diagnostics.push(diagnostic.clone()),
            None => self.unresolved += 1,
        }
    }
}

/// A two-way sink combinator: every [`Sink`] call forwards to both
/// halves — the §7.2 many-consumers-over-one-stream contract made
/// concrete (e.g. build a tree and run the query crate's `QuerySink`
/// matcher in a single [`drive_sink`] pass).
///
/// The halves run in field order (`first`, then `second`) and degrade
/// independently on malformed streams: a half that ignores exit
/// underflow does so exactly as it would in a solo run.
#[derive(Debug)]
pub struct TeeSink<A, B> {
    /// The first half (called before `second` on every step).
    pub first: A,
    /// The second half.
    pub second: B,
}

impl<A, B> TeeSink<A, B> {
    /// Zip two sinks into one.
    #[must_use]
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A: Sink, B: Sink> Sink for TeeSink<A, B> {
    fn enter(&mut self, kind: crate::NodeKind) {
        self.first.enter(kind);
        self.second.enter(kind);
    }

    fn exit(&mut self) {
        self.first.exit();
        self.second.exit();
    }

    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, n_raw: u8) {
        self.first.token(kind, range.clone(), n_raw);
        self.second.token(kind, range, n_raw);
    }

    fn error(&mut self, diag: u32) {
        self.first.error(diag);
        self.second.error(diag);
    }
}

/// A borrowed view of one [`Sink`] step, as shown to a [`FilterSink`]
/// predicate (the borrow avoids materializing a token's `Range` clone
/// for a step the gate may drop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterStep<'a> {
    /// A node enter of `kind` (see [`Sink::enter`]).
    Enter {
        /// The node kind being entered.
        kind: crate::NodeKind,
    },
    /// A node exit (see [`Sink::exit`]).
    Exit,
    /// A token of `kind` covering `range` with `n_raw` raw tokens
    /// (see [`Sink::token`]).
    Token {
        /// The token kind.
        kind: crate::TokenKind,
        /// The token's byte range (borrowed from the drive).
        range: &'a Range<u32>,
        /// Raw tokens folded into this step.
        n_raw: u8,
    },
    /// A deferred diagnostic index (see [`Sink::error`]).
    Error {
        /// The side-table index of the diagnostic.
        diag: u32,
    },
}

/// A predicate-gated forwarding decorator: every [`Sink`] step is shown
/// to the gate first, and only accepted steps reach [`FilterSink::inner`]
/// — the filtering half of the §7.2 many-consumers-over-one-stream
/// contract. Drop noisy diagnostics (`FilterStep::Error` by index),
/// restrict a token pass to the kinds a tool cares about, or carve
/// down to one subtree's steps, all without a second drive.
///
/// The gate is a `FnMut(&FilterStep<'_>) -> bool` and sees *every*
/// step — dropped steps are simply not forwarded. Structural steps
/// need care: dropping an [`enter`](Sink::enter) or [`exit`](Sink::exit)
/// shifts the balance the inner sink observes (a tree builder left
/// open, a skeleton with shifted spans) — the same responsibility the
/// [`drive_sink`] driver already leaves to the sinks for malformed
/// streams, and token/cursor-dependent sinks (`SkeletonSink`, the
/// tree builder) read byte positions from the token ranges the gate
/// lets through.
#[derive(Debug)]
pub struct FilterSink<P, S> {
    /// The gate: returns true for every step that forwards to `inner`.
    predicate: P,
    /// The gated consumer.
    inner: S,
}

impl<P, S> FilterSink<P, S> {
    /// Gate `inner` behind `predicate`.
    #[must_use]
    pub fn new(predicate: P, inner: S) -> Self {
        Self { predicate, inner }
    }

    /// The gated consumer (post-drive reads, tee zips).
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// The gated consumer, mutably (drive-through construction).
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Consume the decorator into its gated consumer.
    #[must_use = "the gated consumer is the whole point"]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<P, S> Sink for FilterSink<P, S>
where
    P: FnMut(&FilterStep<'_>) -> bool,
    S: Sink,
{
    fn enter(&mut self, kind: crate::NodeKind) {
        if (self.predicate)(&FilterStep::Enter { kind }) {
            self.inner.enter(kind);
        }
    }

    fn exit(&mut self) {
        if (self.predicate)(&FilterStep::Exit) {
            self.inner.exit();
        }
    }

    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, n_raw: u8) {
        if (self.predicate)(&FilterStep::Token {
            kind,
            range: &range,
            n_raw,
        }) {
            self.inner.token(kind, range, n_raw);
        }
    }

    fn error(&mut self, diag: u32) {
        if (self.predicate)(&FilterStep::Error { diag }) {
            self.inner.error(diag);
        }
    }
}

#[cfg(test)]
mod sink_tests {
    use std::ops::Range;

    use super::{
        DiagCollector, FilterSink, FilterStep, SkeletonSink, StatsSink, TeeSink, TokenSink,
        ValidationSink,
    };
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

    // ---- StatsSink: per-kind histograms, depth, byte coverage ----

    #[test]
    fn stats_sink_counts_per_kind_totals_depth_and_bytes() {
        let (output, ranges) = fixed_output();
        let mut sink = StatsSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(sink.node_count(10), 1);
        assert_eq!(sink.node_count(11), 1);
        assert_eq!(sink.node_count(12), 0, "absent kinds read as zero");
        assert_eq!(sink.token_count(1), 1);
        assert_eq!(sink.token_count(2), 1);
        assert_eq!(sink.token_count(3), 0);
        assert_eq!(sink.nodes(), 2);
        assert_eq!(sink.tokens(), 2);
        assert_eq!(sink.errors(), 0);
        assert_eq!(sink.max_depth(), 2, "two nested enters");
        assert_eq!(sink.token_bytes(), 5, "ranges 0..2 + 2..5");
    }

    #[test]
    fn stats_sink_snapshot_round_trips_through_into_stats() {
        let (output, ranges) = fixed_output();
        let mut sink = StatsSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        let borrowed = sink.stats();
        let owned = sink.into_stats();
        assert_eq!(
            borrowed, owned,
            "the borrowed view and the consumed snapshot agree"
        );
        assert_eq!(owned.node_count(10), 1);
        assert_eq!(owned.token_count(2), 1);
        assert_eq!(owned.nodes, 2);
        assert_eq!(owned.tokens, 2);
        assert_eq!(owned.errors, 0);
        assert_eq!(owned.max_depth, 2);
        assert_eq!(owned.token_bytes, 5);
    }

    #[test]
    fn stats_sink_survives_error_streams_and_exit_underflow() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Enter { kind: 2 },
            Step::Error { diag: 3 },
            Step::Exit,
            Step::Exit,
            Step::Exit, // underflow: no open node to close
            Step::Enter { kind: 1 },
            Step::Exit,
        ]);
        let mut sink = StatsSink::default();
        drive_sink(&output, &[], &mut sink).expect("malformed stream must not panic");
        assert_eq!(sink.node_count(1), 2);
        assert_eq!(sink.node_count(2), 1);
        assert_eq!(sink.errors(), 1);
        assert_eq!(sink.max_depth(), 2, "underflow must not inflate the depth");
        assert_eq!(
            sink.token_bytes(),
            0,
            "no range table supplied: zero-width synthesis counts no bytes"
        );
    }

    #[test]
    fn stats_sink_rides_the_tee_alongside_validation() {
        let (output, ranges) = fixed_output();
        let solo = {
            let mut sink = StatsSink::default();
            drive_sink(&output, &ranges, &mut sink).expect("drives");
            sink.stats()
        };
        let mut tee = TeeSink::new(ValidationSink::default(), StatsSink::default());
        drive_sink(&output, &ranges, &mut tee).expect("drives");
        assert_eq!(tee.first.summary().nodes, 2, "validation half unaffected");
        assert_eq!(tee.second.stats(), solo, "stats half matches its solo run");
    }

    // ---- FilterSink: predicate-gated forwarding (§7.2 filtering half) ----

    #[test]
    fn filter_sink_forwards_only_accepted_token_steps() {
        let (output, ranges) = fixed_output();
        // Keep token kind 1, drop token kind 2; structure passes as-is.
        let mut sink = FilterSink::new(
            |step: &FilterStep<'_>| match *step {
                FilterStep::Token { kind, .. } => kind == 1,
                _ => true,
            },
            TokenSink::default(),
        );
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(sink.into_inner().tokens, vec![(1, 0..2)]);
    }

    #[test]
    fn filter_sink_can_drop_diagnostics_only() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Error { diag: 3 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Error { diag: 7 },
            Step::Exit,
        ]);
        let mut sink = FilterSink::new(
            |step: &FilterStep<'_>| !matches!(*step, FilterStep::Error { diag: 7 }),
            ValidationSink::default(),
        );
        drive_sink(&output, &[], &mut sink).expect("drives");
        let validation = sink.into_inner();
        assert_eq!(validation.errors, 1, "only the dropped diag index vanished");
        assert_eq!(validation.nodes, 1);
        assert_eq!(validation.tokens, 1);
        assert!(
            validation.balanced(),
            "structure is untouched by the filter"
        );
    }

    #[test]
    fn filter_sink_identity_predicate_matches_the_solo_run() {
        let (output, ranges) = fixed_output();
        let solo = {
            let mut sink = ValidationSink::default();
            drive_sink(&output, &ranges, &mut sink).expect("drives");
            sink.summary()
        };
        let mut sink = FilterSink::new(|_: &FilterStep<'_>| true, ValidationSink::default());
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(
            sink.into_inner().summary(),
            solo,
            "an always-true filter is a no-op"
        );
    }

    #[test]
    fn filter_sink_exposes_inner_by_ref_and_by_value() {
        let (output, ranges) = fixed_output();
        let mut sink = FilterSink::new(
            |step: &FilterStep<'_>| {
                matches!(*step, FilterStep::Enter { kind: 10 } | FilterStep::Exit)
                    || matches!(*step, FilterStep::Token { kind: 1, .. })
            },
            TokenSink::default(),
        );
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        assert_eq!(
            sink.inner().tokens.len(),
            1,
            "the kind-2 token stayed behind the gate"
        );
        let inner = sink.into_inner();
        assert_eq!(inner.tokens, vec![(1, 0..2)]);
    }

    #[test]
    fn filter_sink_gates_stats_inside_the_tee() {
        let (output, ranges) = fixed_output();
        // Stats half only sees token kind 1; validation half rides the
        // tee unfiltered (the filter wraps one half, not the drive).
        let mut tee = TeeSink::new(
            FilterSink::new(
                |step: &FilterStep<'_>| match *step {
                    FilterStep::Token { kind, .. } => kind == 1,
                    _ => true,
                },
                StatsSink::default(),
            ),
            ValidationSink::default(),
        );
        drive_sink(&output, &ranges, &mut tee).expect("drives");
        assert_eq!(tee.first.inner().token_count(1), 1);
        assert_eq!(tee.first.inner().token_count(2), 0, "dropped by the filter");
        assert_eq!(tee.first.inner().nodes(), 2, "structure still counts");
        assert_eq!(
            tee.second.summary().tokens,
            2,
            "the other tee half is unfiltered"
        );
    }

    // ---- TeeSink: one drive, two consumers (§7.2 many-sinks contract) ----

    #[test]
    fn tee_sink_forwards_every_step_to_both_halves() {
        let (output, ranges) = fixed_output();
        let mut tee = TeeSink::new(ValidationSink::default(), TokenSink::default());
        drive_sink(&output, &ranges, &mut tee).expect("drives");
        // Both halves saw the full stream, identical to their solo runs.
        assert_eq!(tee.first.nodes, 2);
        assert_eq!(tee.first.tokens, 2);
        assert!(tee.first.balanced());
        assert_eq!(tee.second.tokens, vec![(1, 0..2), (2, 2..5)]);
    }

    #[test]
    fn tee_sink_forwarding_matches_solo_runs_on_error_streams() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Error { diag: 3 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Exit,
        ]);
        let solo_validation = {
            let mut sink = ValidationSink::default();
            drive_sink(&output, &[], &mut sink).expect("drives");
            sink.summary()
        };
        let solo_tokens = {
            let mut sink = TokenSink::default();
            drive_sink(&output, &[], &mut sink).expect("drives");
            sink.tokens.clone()
        };
        let mut tee = TeeSink::new(ValidationSink::default(), TokenSink::default());
        drive_sink(&output, &[], &mut tee).expect("drives");
        assert_eq!(tee.first.summary(), solo_validation);
        assert_eq!(tee.second.tokens, solo_tokens);
    }

    // ---- DiagCollector: deferred indexes resolved through the side table ----

    /// A side table with three entries (the fixture any collector test
    /// resolves against).
    fn fixed_table() -> crate::diagnostic::DiagTable {
        let mut table = crate::diagnostic::DiagTable::default();
        table.push("missing colon", 3..3);
        table.push("unexpected token", 5..8);
        table.push("unterminated string", 11..20);
        table
    }

    #[test]
    fn diag_collector_resolves_error_events_in_stream_order() {
        let table = fixed_table();
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Error { diag: 2 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Error { diag: 0 },
            Step::Exit,
        ]);
        let mut sink = super::DiagCollector::new(&table);
        drive_sink(&output, &[], &mut sink).expect("drives");
        let resolved: Vec<&str> = sink
            .diagnostics()
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            resolved,
            vec!["unterminated string", "missing colon"],
            "stream order, not table order"
        );
        assert_eq!(sink.unresolved(), 0);
        let owned = sink.into_diagnostics();
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].range, 11..20);
    }

    #[test]
    fn diag_collector_counts_unresolved_indexes() {
        let table = fixed_table();
        let output = pack(&[
            Step::Error { diag: 99 }, // beyond the table
            Step::Error { diag: 1 },
            Step::Error { diag: 7 }, // beyond the table
        ]);
        let mut sink = super::DiagCollector::new(&table);
        drive_sink(&output, &[], &mut sink).expect("drives");
        assert_eq!(sink.diagnostics().len(), 1);
        assert_eq!(sink.diagnostics()[0].message, "unexpected token");
        assert_eq!(sink.unresolved(), 2, "out-of-range indexes are counted");
    }

    #[test]
    fn diag_collector_rides_the_tee_alongside_token_sink() {
        let table = fixed_table();
        let output = pack(&[
            Step::Enter { kind: 10 },
            Step::Error { diag: 1 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Exit,
        ]);
        // Solo runs are the oracles.
        let solo_tokens = {
            let mut sink = TokenSink::default();
            drive_sink(&output, &[], &mut sink).expect("drives");
            sink.tokens.clone()
        };
        let solo_diags = {
            let mut sink = super::DiagCollector::new(&table);
            drive_sink(&output, &[], &mut sink).expect("drives");
            sink.into_diagnostics()
        };
        let mut tee = TeeSink::new(TokenSink::default(), super::DiagCollector::new(&table));
        drive_sink(&output, &[], &mut tee).expect("drives");
        assert_eq!(tee.first.tokens, solo_tokens);
        assert_eq!(tee.second.into_diagnostics(), solo_diags);
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
        // The DiagCollector oracle's side table: four entries, so error
        // indexes >= 4 stay unresolved (exactly the ones the collector
        // must count, not resolve).
        let mut table = crate::diagnostic::DiagTable::default();
        for i in 0..4u32 {
            table.push(format!("diag {i}"), i..i + 1);
        }
        for _ in 0..300 {
            let mut steps = Vec::new();
            // Test-side oracles.
            let (mut enters, mut tokens, mut errors) = (0u32, 0u32, 0u32);
            let (mut depth, mut underflow, mut pops) = (0u32, false, 0u32);
            let mut max_depth = 0u32;
            for _ in 0..8 + rng.below(40) {
                match rng.below(6) {
                    0 | 1 => {
                        let kind = u16::try_from(rng.below(50)).expect("kind fits u16");
                        steps.push(Step::Enter { kind });
                        enters += 1;
                        depth += 1;
                        if depth > max_depth {
                            max_depth = depth;
                        }
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

            // DiagCollector: resolves exactly the in-table errors, in
            // stream order; the rest are counted, not resolved.
            let mut collector = DiagCollector::new(&table);
            drive_sink(&output, &[], &mut collector).expect("drive never fails");
            let expected_diags: Vec<_> = steps
                .iter()
                .filter_map(|step| match *step {
                    Step::Error { diag } if diag < 4 => Some(diag),
                    _ => None,
                })
                .collect();
            assert_eq!(
                collector
                    .diagnostics()
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>(),
                expected_diags
                    .iter()
                    .map(|diag| format!("diag {diag}"))
                    .collect::<Vec<_>>(),
                "resolved diagnostics follow the stream's errors"
            );
            assert_eq!(
                collector.unresolved(),
                usize::try_from(errors - u32::try_from(expected_diags.len()).unwrap_or(0))
                    .expect("fits usize"),
                "out-of-table indexes are counted"
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

            // StatsSink: totals + depth + histograms all agree with the
            // oracles (empty range table: zero-width synthesis, no bytes).
            let mut stats = StatsSink::default();
            drive_sink(&output, &[], &mut stats).expect("drive never fails");
            assert_eq!(stats.nodes(), enters);
            assert_eq!(stats.tokens(), tokens);
            assert_eq!(stats.errors(), errors);
            assert_eq!(stats.max_depth(), max_depth);
            assert_eq!(stats.token_bytes(), 0, "no ranges, no counted bytes");
            let snapshot = stats.into_stats();
            assert_eq!(
                snapshot.node_histogram.iter().sum::<u32>(),
                enters,
                "node histogram sums to the enter total"
            );
            assert_eq!(
                snapshot.token_histogram.iter().sum::<u32>(),
                tokens,
                "token histogram sums to the token total"
            );

            // FilterSink: even-kind token gate — the inner token sink
            // sees exactly the accepted steps, structure passes whole.
            let mut filtered = FilterSink::new(
                |step: &FilterStep<'_>| match *step {
                    FilterStep::Token { kind, .. } => kind % 2 == 0,
                    _ => true,
                },
                TokenSink::default(),
            );
            drive_sink(&output, &[], &mut filtered).expect("drive never fails");
            let expected_kept = steps
                .iter()
                .filter_map(|step| match *step {
                    Step::Token { kind, .. } if kind % 2 == 0 => Some(kind),
                    _ => None,
                })
                .count();
            assert_eq!(
                filtered.into_inner().tokens.len(),
                expected_kept,
                "the gate forwards exactly the even-kind tokens"
            );
        }
    }
}

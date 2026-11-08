//! Green/red trees with structural sharing (roadmap §7.6, rowan lineage,
//! issue 008).
//!
//! Green nodes are immutable, position-independent trees (`GreenNode`):
//! children + trivia, renderable byte-faithfully. [`TreeBuilder`] is the
//! tree-sink side of [`crate::sink::Sink`]: it consumes the packed event
//! stream plus a source and produces one root [`GreenNode`]. Trivia rides
//! [`crate::sink::TriviaRecord`]s (ADR-008) — never token events.

use std::ops::Range;
use std::sync::Arc;

use crate::sink::{Sink, TriviaRecord};

/// A green-tree child: either an interior node or a token leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreenChild {
    /// An interior node (shared via `Arc` — the structural-sharing layer).
    Node(Arc<GreenNode>),
    /// A token leaf covering `range` of the source.
    Token {
        /// Token kind.
        kind: crate::TokenKind,
        /// Byte range in the source (zero trivia leaves are allowed).
        range: Range<u32>,
    },
}

impl GreenChild {
    /// Text length contributed by this child.
    #[must_use]
    pub fn text_len(&self) -> u32 {
        match self {
            Self::Node(node) => node.text_len,
            Self::Token { range, .. } => range.end.saturating_sub(range.start),
        }
    }

    /// Whether this subtree/leaf carries an error flag.
    #[must_use]
    pub fn has_error(&self) -> bool {
        match self {
            Self::Node(node) => node.has_error,
            Self::Token { kind, .. } => *kind == crate::ERROR_KIND,
        }
    }
}

/// An immutable green node: kind, accumulated text length, children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreenNode {
    /// Node kind.
    pub kind: crate::NodeKind,
    /// Sum of child text lengths (cached for O(1) parent math).
    pub text_len: u32,
    /// Whether this subtree contains an ERROR node/token.
    pub has_error: bool,
    /// Children in DFS order (trivia leaves interleaved per ADR-008).
    pub children: Vec<GreenChild>,
}

impl GreenNode {
    /// Render byte-faithfully: concatenating all token leaf slices in DFS
    /// order reproduces the source exactly.
    ///
    /// # Panics
    /// Never for trees built by [`TreeBuilder`] (token ranges are in-bounds
    /// and on char boundaries); a hand-built tree with out-of-bounds ranges
    /// panics on slice.
    pub fn render(&self, source: &crate::input::Source<'_>) -> String {
        let mut out = String::with_capacity(self.text_len as usize);
        self.render_into(source, &mut out);
        out
    }

    fn render_into(&self, source: &crate::input::Source<'_>, out: &mut String) {
        for child in &self.children {
            match child {
                GreenChild::Node(node) => node.render_into(source, out),
                GreenChild::Token { range, .. } => {
                    out.push_str(source.slice(range.clone()));
                }
            }
        }
    }

    /// Total green nodes in this subtree (this node included).
    #[must_use]
    pub fn node_count(&self) -> usize {
        1 + self
            .children
            .iter()
            .filter_map(|c| match c {
                GreenChild::Node(n) => Some(n.node_count()),
                GreenChild::Token { .. } => None,
            })
            .sum::<usize>()
    }

    /// Total token leaves (trivia leaves included).
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.children
            .iter()
            .map(|c| match c {
                GreenChild::Node(n) => n.token_count(),
                GreenChild::Token { .. } => 1,
            })
            .sum()
    }

    /// All token leaves as `(kind, range)` pairs in DFS order.
    #[must_use]
    pub fn token_ranges(&self) -> Vec<(crate::TokenKind, Range<u32>)> {
        let mut out = Vec::new();
        self.collect_tokens(&mut out);
        out
    }

    fn collect_tokens(&self, out: &mut Vec<(crate::TokenKind, Range<u32>)>) {
        for child in &self.children {
            match child {
                GreenChild::Node(node) => node.collect_tokens(out),
                GreenChild::Token { kind, range } => out.push((*kind, range.clone())),
            }
        }
    }
}

/// Interning cache for token kinds (cstree lineage): v0 keeps token leaves
/// un-hashed (cheap) — the cache shape exists so the interning backend can
/// slot in without touching the builder API.
#[derive(Debug, Default)]
pub struct NodeCache {
    _private: (),
}

impl NodeCache {
    /// A fresh cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// The tree-building sink: consumes packed steps + trivia records and
/// produces one root [`GreenNode`].
#[derive(Debug)]
pub struct TreeBuilder<'s> {
    /// Stack of open nodes: `(kind, children, has_error)`.
    stack: Vec<(crate::NodeKind, Vec<GreenChild>, bool)>,
    /// Completed top-level nodes (sibling roots are an error at finish).
    roots: Vec<GreenChild>,
    /// Trivia records to interleave at token time (ADR-008).
    trivia: Vec<TriviaRecord>,
    /// Next trivia record not yet attached.
    trivia_idx: usize,
    /// The source (for bounds checks and EOF synthesis).
    source_len: u32,
    _marker: std::marker::PhantomData<&'s ()>,
}

impl<'s> TreeBuilder<'s> {
    /// A builder attached to a source (byte-faithfulness needs the source
    /// length for the EOF synthesis; the cache slot keeps the cstree shape).
    #[must_use]
    pub fn with_source(_cache: Arc<NodeCache>, source: &'s crate::input::Source<'s>) -> Self {
        Self {
            stack: Vec::new(),
            roots: Vec::new(),
            trivia: Vec::new(),
            trivia_idx: 0,
            source_len: u32::try_from(source.len()).unwrap_or(u32::MAX),
            _marker: std::marker::PhantomData,
        }
    }

    /// Attach trivia records (call once, before driving the stream).
    pub fn attach_trivia_records(&mut self, records: &[TriviaRecord]) {
        self.trivia = records.to_vec();
        self.trivia_idx = 0;
    }

    /// Attach trivia records without cloning (builder takes ownership).
    pub fn attach_trivia_owned(&mut self, records: Vec<TriviaRecord>) {
        self.trivia = records;
        self.trivia_idx = 0;
    }

    /// Emit every trivia record that ENDS at or before `token_start` as
    /// leaves, preserving order; returns whether any trailing record
    /// touched the token (for trailing classification on the next step).
    fn flush_trivia_before(&mut self, token_start: u32, children: &mut Vec<GreenChild>) {
        while self.trivia_idx < self.trivia.len() {
            let record = &self.trivia[self.trivia_idx];
            if record.range.end > token_start {
                break;
            }
            children.push(GreenChild::Token {
                kind: record.kind,
                range: record.range.clone(),
            });
            self.trivia_idx += 1;
        }
    }

    /// Finish: close all open nodes (a stream that left nodes open is an
    /// error at [`drive_sink`](crate::sink::drive_sink) level, so this
    /// only fires for a truly empty builder), synthesize the EOF token,
    /// and require exactly one root.
    ///
    /// # Errors
    /// When there is no root or more than one root (sibling roots are a
    /// parser bug — packs wrap documents in an implicit ROOT).
    ///
    /// # Panics
    /// Never in practice: the root `Arc` is freshly built and uniquely
    /// owned at this point; the `expect` documents that invariant.
    pub fn finish(mut self) -> Result<Arc<GreenNode>, String> {
        // Trivia after the last token.
        let mut tail: Vec<GreenChild> = Vec::new();
        self.flush_trivia_before(self.source_len, &mut tail);
        // EOF token (zero-width at the source end; excluded from render by
        // its empty range but visible in token tables).
        tail.push(GreenChild::Token {
            kind: crate::EOF_KIND,
            range: self.source_len..self.source_len,
        });

        match (self.roots.len(), self.stack.len()) {
            (0, 0) => Err("no root node: drive at least one Start before finish".to_owned()),
            (1, 0) => {
                let Some(GreenChild::Node(mut root)) = self.roots.pop() else {
                    return Err("root slot held a token".to_owned());
                };
                // The root Arc is freshly built and uniquely owned here.
                let node = Arc::get_mut(&mut root).expect("freshly built root is unique");
                node.children.extend(tail);
                node.text_len = node.children.iter().map(GreenChild::text_len).sum();
                node.has_error |= node.children.iter().any(GreenChild::has_error);
                Ok(root)
            }
            (n, 0) => Err(format!("{n} sibling roots: the parser must emit one root")),
            (_, open) => Err(format!("{open} node(s) still open at finish")),
        }
    }
}

impl Sink for TreeBuilder<'_> {
    fn enter(&mut self, kind: crate::NodeKind) {
        self.stack.push((kind, Vec::new(), false));
    }

    fn exit(&mut self) {
        let Some((kind, children, mut has_error)) = self.stack.pop() else {
            return; // drive_sink validates; never underflow a valid stream
        };
        // Single pass: text length and error flag in one scan (exit is the
        // hottest sink path — every node pays it once).
        let mut text_len = 0u32;
        for child in &children {
            text_len = text_len.saturating_add(child.text_len());
            has_error |= child.has_error();
        }
        let node = Arc::new(GreenNode {
            kind,
            text_len,
            has_error,
            children,
        });
        if let Some(parent) = self.stack.last_mut() {
            parent.1.push(GreenChild::Node(node));
        } else {
            // A top-level node completed: sibling roots are detected at
            // `finish` (the parser contract is exactly one root).
            self.roots.push(GreenChild::Node(node));
        }
    }

    fn token(&mut self, kind: crate::TokenKind, range: Range<u32>, _n_raw: u8) {
        // Field-disjoint borrows: `frame` holds `self.stack`, the trivia
        // scan touches `self.trivia`/`self.trivia_idx` — no `mem::take`
        // dance (the out-and-back Vec move per token showed up in phase
        // attribution).
        let Some(frame) = self.stack.last_mut() else {
            return; // token outside any node: drive_sink contract violation
        };
        // ADR-008 contract: the pack emits CLEAN token ranges (trivia
        // excluded) and TriviaRecords cover every gap — so each pending
        // record ends at or before the next token's start, and flushing
        // before the leaf preserves byte order exactly.
        let mut idx = self.trivia_idx;
        let trivia = &self.trivia;
        while idx < trivia.len() && trivia[idx].range.end <= range.start {
            let record = &trivia[idx];
            frame.1.push(GreenChild::Token {
                kind: record.kind,
                range: record.range.clone(),
            });
            idx += 1;
        }
        self.trivia_idx = idx;
        if kind == crate::ERROR_KIND {
            frame.2 = true;
        }
        frame.1.push(GreenChild::Token { kind, range });
    }

    fn error(&mut self, _diag: u32) {
        if let Some((_, _, has_error)) = self.stack.last_mut() {
            *has_error = true;
        }
    }
}

#[cfg(test)]
mod tree_tests {
    use std::sync::Arc;

    use crate::events::{Output, Step};
    use crate::input::Source;
    use crate::sink::{TriviaRecord, drive_sink};
    use crate::tree::{GreenNode, NodeCache, TreeBuilder};

    fn pack(steps: &[Step]) -> Output {
        let mut stream = Vec::new();
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

    /// Build the tree for `10 { tok(0..2) 11 { tok(3..5) } }` over a 5-byte
    /// source, with optional trivia records. The default call uses clean
    /// token ranges `0..2` and `3..5` (byte 2 is the inter-token gap).
    fn build(trivia: &[TriviaRecord]) -> Arc<GreenNode> {
        let output = pack(&[
            Step::Enter { kind: 10 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Enter { kind: 11 },
            Step::Token { kind: 2, n_raw: 2 },
            Step::Exit,
            Step::Exit,
        ]);
        let first: std::ops::Range<u32> = 0..2;
        let second: std::ops::Range<u32> = 3..5;
        let ranges = [first, second];
        let src = Source::from_str_unchecked("abcde");
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &src);
        builder.attach_trivia_records(trivia);
        drive_sink(&output, &ranges, &mut builder).expect("drives");
        builder.finish().expect("single root")
    }

    #[test]
    fn render_is_byte_faithful() {
        // The gap (byte 2) must be covered by a trivia record for the
        // render to reproduce the source byte-for-byte.
        let tree = build(&[TriviaRecord {
            kind: 14,
            range: 2..3,
            trailing: true,
        }]);
        assert_eq!(tree.render(&Source::from_str_unchecked("abcde")), "abcde");
        assert_eq!(tree.text_len, 5);
        assert_eq!(tree.node_count(), 2); // node10 + node11 (no external ROOT here)
        assert_eq!(tree.token_count(), 4); // 2 tokens + trivia + EOF
    }

    #[test]
    fn trivia_leaves_interleave_in_dfs_order() {
        // Records cover the GAP between the two clean tokens (ADR-008:
        // clean token ranges + gap-covering trivia records).
        let tree = build(&[TriviaRecord {
            kind: 14,
            range: 2..3,
            trailing: true,
        }]);
        let tokens = tree.token_ranges();
        let kinds: Vec<u16> = tokens.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![1, 14, 2, crate::EOF_KIND]);
        // Render is byte-faithful: token + trivia + token + EOF(zero-width)
        // reproduce every byte exactly once.
        assert_eq!(tree.render(&Source::from_str_unchecked("abcde")), "abcde");
    }

    #[test]
    fn unbalanced_streams_error_at_finish() {
        // Two roots (sibling tops) → error at finish.
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Token { kind: 1, n_raw: 1 },
            Step::Exit,
            Step::Enter { kind: 2 },
            Step::Exit,
        ]);
        let src = Source::from_str_unchecked("xy");
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &src);
        drive_sink(&output, &[], &mut builder).expect("drives");
        let err = builder.finish().expect_err("two roots must be an error");
        assert!(err.contains("sibling roots"), "{err}");
    }

    #[test]
    fn empty_drive_finishes_with_root_only() {
        let output = pack(&[Step::Enter { kind: 7 }, Step::Exit]);
        let src = Source::from_str_unchecked("");
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &src);
        drive_sink(&output, &[], &mut builder).expect("drives");
        let tree = builder.finish().expect("one root");
        assert_eq!(tree.kind, 7);
        assert_eq!(tree.token_count(), 1, "just the synthesized EOF");
        assert_eq!(tree.render(&Source::from_str_unchecked("")), "");
    }

    #[test]
    fn error_flags_propagate_bottom_up() {
        let output = pack(&[
            Step::Enter { kind: 1 },
            Step::Enter { kind: 2 },
            Step::Token {
                kind: crate::ERROR_KIND,
                n_raw: 1,
            },
            Step::Exit,
            Step::Exit,
        ]);
        let err_span: std::ops::Range<u32> = 0..1;
        let error_range = [err_span];
        let src = Source::from_str_unchecked("x");
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &src);
        drive_sink(&output, &error_range, &mut builder).expect("drives");
        let tree = builder.finish().expect("one root");
        assert!(tree.has_error, "error flag propagates to the root");
        // A clean sibling of the error token is not marked.
        let clean = build(&[]);
        assert!(!clean.has_error);
    }
}

//! Green-tree traversal: a forward-only cursor and a walk-event iterator
//! (roadmap §16.C.1 typed-views default — rowan `WalkEvent` lineage).
//!
//! The green tree ([`crate::tree::GreenNode`]) is an immutable, shared
//! structure that consumers previously had to recurse by hand. This module
//! gives them a single, allocation-light seam to iterate it without
//! re-implementing DFS traversal in every downstream crate (the query
//! matcher, the LSP outline, and the persistent-red sharing walker all
//! hand-roll variants of the same walk today).
//!
//! # The seam
//!
//! [`GreenCursor`] borrows a tree and yields [`WalkEvent`]s in DFS order:
//! [`WalkEvent::Enter`] on the way down (interior nodes and token leaves
//! alike), [`WalkEvent::Leave`] on the way back up (interior nodes only —
//! token leaves have no exit). A token leaf is a single `Enter` followed by
//! its sibling's `Enter` (no matching `Leave`). This is the rowan
//! `TreeCursor` / `WalkEvent` shape, trimmed to the green-tree's
//! `Node`/`Token` child enum.
//!
//! # Typed views
//!
//! [`TreeView`] is the trait typed-view crates (one per pack) implement on
//! top of a borrowed [`GreenNode`]: it exposes the kind, a typed child
//! cursor, and byte span recovery. Packs then write `Object`/`Array`/`
//! Value`-shaped wrappers that project into the tree without copying —
//! resolving the §16.C.1 "typed views" open decision without committing
//! to OXC-style full AST codegen.
//!
//! # Why here, not in a new crate
//!
//! The cursor is a pure function of `GreenNode` + `Source` — no
//! pack-specific knowledge — so it lives next to the tree it walks, in the
//! runtime kernel every pack already depends on. A separate crate would
//! add a workspace member for ~150 lines and force a dependency edge that
//! every pack must already pay (runtime-core) to re-point.

pub mod visit;

use std::ops::Range;
use std::sync::Arc;

use crate::input::Source;
use crate::tree::{GreenChild, GreenNode};

/// One event of a depth-first walk over a green tree.
///
/// Interior nodes emit an `Enter` on the way down and a `Leave` on the way
/// back up; token leaves emit a single `Enter` (the leaf *is* the entry —
/// there is no exit). This mirrors rowan's `WalkEvent` and keeps the walk
/// a flat iterator with no per-step heap allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkEvent<'t> {
    /// Entered an interior node (a borrow on the node).
    Enter(&'t GreenNode),
    /// Entered a token leaf (its kind and byte range).
    Token {
        /// Token kind.
        kind: crate::TokenKind,
        /// Byte range in the source.
        range: Range<u32>,
    },
    /// Left an interior node (a borrow on the node just exited).
    Leave(&'t GreenNode),
}

/// A forward-only cursor over a [`GreenNode`].
///
/// Constructed with [`GreenCursor::new`] over the root; [`GreenCursor::next`]
/// advances the walk. The cursor borrows the root `Arc<GreenNode>` for the
/// duration of the walk (`'t`) and stores `&'t GreenNode` references to each
/// node on its path stack — no `Arc::clone` per step, no heap allocation.
/// The yielded [`WalkEvent`] borrows borrow those `'t`-lifetime references,
/// so the returned borrows live as long as the root `Arc`.
///
/// The cursor is the engine behind [`walk`]; callers wanting the whole
/// traversal in one pass should prefer [`walk`].
pub struct GreenCursor<'t> {
    /// The work stack: a `&'t GreenNode` plus a child index per open frame.
    /// Borrowing the node (not cloning the `Arc`) keeps the walk allocation-
    /// free; the `'t` lifetime ties every yielded borrow to the root `Arc`.
    stack: Vec<Frame<'t>>,
}

struct Frame<'t> {
    /// Borrowed reference to the node being walked (lives as long as the
    /// root `Arc<GreenNode>` the cursor was constructed over).
    node: &'t GreenNode,
    /// Next child index to visit. `usize::MAX` sentinel means "emit Enter
    /// next, no child visited yet" (avoids a separate bool per frame).
    next: usize,
    /// True once the `Leave` for this node has been queued (the last child
    /// finished). Keeps `next` monotonic without a second scan.
    done: bool,
}

/// A pending action extracted from the top frame via a shared borrow, so
/// the returned [`WalkEvent`] borrow can be tied to `'t` (the cursor's root
/// borrow) instead of the `&mut self` call borrow.
enum Action<'t> {
    /// Emit a token leaf and advance.
    Token {
        kind: crate::TokenKind,
        range: Range<u32>,
    },
    /// A child interior node to descend into.
    Descend(&'t GreenNode),
    /// The top frame is out of children; queue its Leave for the next loop.
    FinishFrame,
}

impl<'t> GreenCursor<'t> {
    /// Start a cursor at the root. The first [`next`](Self::next) yields
    /// [`WalkEvent::Enter`] for `root`.
    #[must_use]
    pub fn new(root: &'t Arc<GreenNode>) -> Self {
        Self {
            stack: vec![Frame {
                node: root,
                next: usize::MAX,
                done: false,
            }],
        }
    }

    /// Advance to the next event, or `None` when the walk is complete.
    ///
    /// The walk is a flat DFS: `Enter(root)`, then each child in order
    /// (descending into interior nodes, emitting `Token` for leaves), then
    /// `Leave(root)`.
    #[must_use]
    pub fn next_event(&mut self) -> Option<WalkEvent<'t>> {
        loop {
            // Sentinel: emit Enter for the top frame if it has not been
            // entered yet (the root on the first call, or a child just
            // pushed by the previous iteration's Descend). The borrowed
            // node pointer is `&'t GreenNode` (independent of `&mut
            // self`), so the early return does not conflict with later
            // mutable stack access.
            let top = self.stack.last()?;
            if top.next == usize::MAX {
                let event = WalkEvent::Enter(top.node);
                self.stack.last_mut()?.next = 0;
                return Some(event);
            }
            if top.done {
                let node = self.stack.last()?.node;
                let event = WalkEvent::Leave(node);
                self.stack.pop();
                return Some(event);
            }
            // Inspect the top frame's next child with a shared borrow; the
            // child pointer (when it is a Node) is `&'t GreenNode` derived
            // from the frame's `'t`-lifetime `node` field, so it survives
            // dropping the shared borrow.
            let action = {
                let top = self.stack.last()?;
                match top.node.children.get(top.next) {
                    None => Action::FinishFrame,
                    Some(GreenChild::Token { kind, range }) => Action::Token {
                        kind: *kind,
                        range: range.clone(),
                    },
                    Some(GreenChild::Node(node)) => Action::Descend(node.as_ref()),
                }
            };
            match action {
                Action::Token { kind, range } => {
                    self.stack.last_mut()?.next += 1;
                    return Some(WalkEvent::Token { kind, range });
                }
                Action::FinishFrame => {
                    self.stack.last_mut()?.done = true;
                }
                Action::Descend(node) => {
                    self.stack.last_mut()?.next += 1;
                    self.stack.push(Frame {
                        node,
                        next: usize::MAX,
                        done: false,
                    });
                }
            }
        }
    }
}
/// Walk `root` depth-first, yielding [`WalkEvent`]s. A convenience over
/// [`GreenCursor::new`] for callers who want a one-shot iterator.
///
/// The returned iterator borrows the root `Arc` for its lifetime; each
/// step is O(1) amortized (one stack push/pop per interior node, one step
/// per token leaf) with no heap allocation.
pub fn walk(root: &Arc<GreenNode>) -> impl Iterator<Item = WalkEvent<'_>> + '_ {
    let mut cursor = GreenCursor::new(root);
    std::iter::from_fn(move || cursor.next_event())
}

/// The byte range a green node covers in `source`.
///
/// A green node's `text_len` is its *own* span width, but its position is
/// contextual (it depends on the parent). This helper recovers the absolute
/// span by walking from `root` to `target` accumulating offsets — the
/// shape every hand-rolled walker re-derives. Returns `None` when `target`
/// is not a descendant of `root` (different `Arc`, or `target == root`
/// maps to `0..root.text_len`).
///
/// # Examples
///
/// ```
/// use runtime_core::input::Source;
/// use runtime_core::tree::{GreenChild, GreenNode};
/// use runtime_core::view::node_span;
/// use std::sync::Arc;
///
/// let leaf = Arc::new(GreenNode { kind: 1, text_len: 3, has_error: false, children: Vec::new() });
/// let root = GreenNode {
///     kind: 0, text_len: 3, has_error: false,
///     children: vec![GreenChild::Node(leaf.clone())],
/// };
/// assert_eq!(node_span(&root, &leaf), Some(0..3));
/// ```
#[must_use]
pub fn node_span(root: &GreenNode, target: &GreenNode) -> Option<Range<u32>> {
    find_subtree(root, target).map(|(_, span)| span)
}

/// The byte range of the `n`-th child of `node`, or `None` when out of
/// bounds. A thin helper for typed views that want "child N's range" without
/// re-deriving the cursor's offset arithmetic.
#[must_use]
pub fn child_span(node: &GreenNode, n: usize) -> Option<Range<u32>> {
    let mut offset = 0u32;
    for (idx, child) in node.children.iter().enumerate() {
        let len = child.text_len();
        if idx == n {
            return Some(offset..offset + len);
        }
        offset = offset.saturating_add(len);
    }
    None
}

/// A trait typed-view crates implement on top of a borrowed `Arc<GreenNode>`:
/// exposes the kind and a cursor into children, plus byte-span recovery
/// against a source. Resolves the §16.C.1 "typed views" default without
/// committing to full AST codegen.
///
/// Implementors are zero-cost projections: they borrow the tree `Arc` for
/// `'t` and the source, exposing typed accessors (`Object::pairs`,
/// `Array::items`, `Value::as_str`) without copying tree data. A typed view
/// is invalidated when the underlying `Arc<GreenNode>` is dropped (a new
/// parse replaces it); the `'t` borrow lets the borrow checker enforce that.
pub trait TreeView<'t> {
    /// The node kind (pack-local dense numbering; sentinels live above).
    fn kind(&self) -> crate::NodeKind;

    /// The byte span this view's node covers in `source`.
    fn span(&self, source: &Source<'_>) -> Range<u32>;

    /// Whether this subtree carries an error flag (recovery landed inside).
    fn has_error(&self) -> bool;

    /// A cursor over this view's node.
    fn cursor(&self) -> GreenCursor<'t>;
}

/// Joint DFS for [`node_span`]: returns `(found, absolute_span)` where
/// `found` is true when `target` is `node` itself, and the span is the
/// accumulated offset at the match. Inner nodes accumulate child offsets;
/// the root matches at offset `0`.
fn find_subtree(node: &GreenNode, target: &GreenNode) -> Option<(bool, Range<u32>)> {
    if std::ptr::eq(node, target) {
        return Some((true, 0..node.text_len));
    }
    let mut offset = 0u32;
    for child in &node.children {
        let len = child.text_len();
        if let GreenChild::Node(n) = child {
            if let Some((_, span)) = find_subtree(n, target) {
                let start = offset.saturating_add(span.start);
                let end = offset.saturating_add(span.end);
                return Some((true, start..end));
            }
        }
        offset = offset.saturating_add(len);
    }
    None
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::input::Source;
    use crate::tree::{GreenChild, GreenNode};
    use std::sync::Arc;

    /// Build a small tree by hand for traversal assertions:
    /// `root(0) { node(1) { tok(10, 0..2) } tok(20, 2..3) }`.
    fn sample() -> Arc<GreenNode> {
        let inner = Arc::new(GreenNode {
            kind: 1,
            text_len: 2,
            has_error: false,
            children: vec![GreenChild::Token {
                kind: 10,
                range: 0..2,
            }],
        });
        Arc::new(GreenNode {
            kind: 0,
            text_len: 3,
            has_error: false,
            children: vec![
                GreenChild::Node(inner),
                GreenChild::Token {
                    kind: 20,
                    range: 2..3,
                },
            ],
        })
    }

    #[test]
    fn walk_emits_enter_token_leave_in_dfs_order() {
        let root = sample();
        let events: Vec<WalkEvent> = walk(&root).collect();
        // Enter(root) Enter(inner) Token(10) Leave(inner) Token(20) Leave(root)
        assert_eq!(events.len(), 6, "six events for a two-child tree");
        assert!(matches!(events[0], WalkEvent::Enter(n) if n.kind == 0));
        assert!(matches!(events[1], WalkEvent::Enter(n) if n.kind == 1));
        assert!(
            matches!(events[2], WalkEvent::Token { kind: 10, .. }),
            "token leaf emits a Token event, no Leave"
        );
        assert!(matches!(events[3], WalkEvent::Leave(n) if n.kind == 1));
        assert!(matches!(events[4], WalkEvent::Token { kind: 20, .. }));
        assert!(matches!(events[5], WalkEvent::Leave(n) if n.kind == 0));
    }

    #[test]
    fn cursor_visits_every_node_once() {
        let root = sample();
        let mut cursor = GreenCursor::new(&root);
        let mut enters = 0u32;
        let mut leaves = 0u32;
        let mut tokens = 0u32;
        while let Some(event) = cursor.next_event() {
            match event {
                WalkEvent::Enter(_) => enters += 1,
                WalkEvent::Leave(_) => leaves += 1,
                WalkEvent::Token { .. } => tokens += 1,
            }
        }
        assert_eq!(enters, 2, "two interior nodes");
        assert_eq!(leaves, 2, "each interior node leaves once");
        assert_eq!(tokens, 2, "two token leaves");
    }

    #[test]
    fn walk_handles_empty_root() {
        let root = Arc::new(GreenNode {
            kind: 7,
            text_len: 0,
            has_error: false,
            children: Vec::new(),
        });
        let events: Vec<WalkEvent> = walk(&root).collect();
        assert_eq!(events.len(), 2, "Enter then Leave for a childless node");
        assert!(matches!(events[0], WalkEvent::Enter(n) if n.kind == 7));
        assert!(matches!(events[1], WalkEvent::Leave(n) if n.kind == 7));
    }

    #[test]
    fn walk_handles_token_only_root() {
        // A root whose only child is a token leaf.
        let root = Arc::new(GreenNode {
            kind: 0,
            text_len: 4,
            has_error: false,
            children: vec![GreenChild::Token {
                kind: 5,
                range: 0..4,
            }],
        });
        let events: Vec<WalkEvent> = walk(&root).collect();
        // Enter(root) Token(5) Leave(root)
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], WalkEvent::Enter(n) if n.kind == 0));
        assert!(matches!(events[1], WalkEvent::Token { kind: 5, .. }));
        assert!(matches!(events[2], WalkEvent::Leave(n) if n.kind == 0));
    }

    #[test]
    fn node_span_recovers_absolute_range_for_descendant() {
        let root = sample();
        // The inner node (kind 1) is the first child at offset 0, width 2.
        let inner = match &root.children[0] {
            GreenChild::Node(n) => n.clone(),
            GreenChild::Token { .. } => unreachable!("first child is a node"),
        };
        let span = node_span(&root, &inner).expect("descendant found");
        assert_eq!(span, 0..2);
    }

    #[test]
    fn node_span_recovers_root() {
        let root = sample();
        let span = node_span(&root, &root).expect("root matches itself");
        assert_eq!(span, 0..3);
    }

    #[test]
    fn node_span_none_for_unrelated_tree() {
        let root = sample();
        let foreign = GreenNode {
            kind: 99,
            text_len: 1,
            has_error: false,
            children: Vec::new(),
        };
        assert!(node_span(&root, &foreign).is_none());
    }

    #[test]
    fn child_span_returns_nth_child_range() {
        let root = sample();
        assert_eq!(child_span(&root, 0), Some(0..2), "first child spans 0..2");
        assert_eq!(child_span(&root, 1), Some(2..3), "second child spans 2..3");
        assert_eq!(child_span(&root, 2), None, "out of bounds");
    }

    #[test]
    fn cursor_drives_a_deeper_tree_without_loss() {
        // root(0) { a(1) { b(2) { tok(9, 0..1) } tok(8, 1..2) } tok(7, 2..3) }
        let leaf_b = Arc::new(GreenNode {
            kind: 2,
            text_len: 1,
            has_error: false,
            children: vec![GreenChild::Token {
                kind: 9,
                range: 0..1,
            }],
        });
        let a = Arc::new(GreenNode {
            kind: 1,
            text_len: 2,
            has_error: false,
            children: vec![
                GreenChild::Node(leaf_b),
                GreenChild::Token {
                    kind: 8,
                    range: 1..2,
                },
            ],
        });
        let root = Arc::new(GreenNode {
            kind: 0,
            text_len: 3,
            has_error: false,
            children: vec![
                GreenChild::Node(a),
                GreenChild::Token {
                    kind: 7,
                    range: 2..3,
                },
            ],
        });
        let events: Vec<WalkEvent> = walk(&root).collect();
        // Enter(0) Enter(1) Enter(2) Token(9) Leave(2) Token(8) Leave(1) Token(7) Leave(0)
        assert_eq!(events.len(), 9);
        let kinds: Vec<Option<u16>> = events
            .iter()
            .map(|e| match e {
                WalkEvent::Enter(n) | WalkEvent::Leave(n) => Some(n.kind),
                WalkEvent::Token { kind, .. } => Some(*kind),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(9),
                Some(2),
                Some(8),
                Some(1),
                Some(7),
                Some(0)
            ]
        );
    }

    #[test]
    fn tree_view_trait_is_implementable() {
        // A minimal TreeView impl borrowing an Arc<GreenNode>, exercising
        // the trait surface (the typed-view crates build on this).
        struct NodeView<'a> {
            node: &'a Arc<GreenNode>,
            offset: u32,
        }
        impl<'a> TreeView<'a> for NodeView<'a> {
            fn kind(&self) -> crate::NodeKind {
                self.node.kind
            }
            fn span(&self, _source: &Source<'_>) -> Range<u32> {
                self.offset..self.offset + self.node.text_len
            }
            fn has_error(&self) -> bool {
                self.node.has_error
            }
            fn cursor(&self) -> GreenCursor<'a> {
                GreenCursor::new(self.node)
            }
        }
        let root = sample();
        let view = NodeView {
            node: &root,
            offset: 0,
        };
        assert_eq!(view.kind(), 0);
        assert_eq!(view.span(&Source::from_str_unchecked("abc")), 0..3);
        assert!(!view.has_error());
        let mut cursor = view.cursor();
        assert!(matches!(cursor.next_event(), Some(WalkEvent::Enter(_))));
    }
}

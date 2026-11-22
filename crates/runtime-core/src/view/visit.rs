//! Green-tree visitor: a push-based walk over a [`GreenNode`] (roadmap
//! §7.2 consumer-table gap — the cursor is pull, this is the push
//! counterpart).
//!
//! [`crate::view::GreenCursor`] is a pull model: the consumer calls
//! `next_event` in a loop. Some consumers want the inverse — a callback for
//! each event, driven by the tree. The query matcher, the LSP outline
//! builder, and the persistent-red sharing walker all hand-roll this shape
//! today; [`visit`] gives them one driver.
//!
//! # The seam
//!
//! [`Visitor`] is the trait: `enter`/`token`/`leave` callbacks (mirroring
//! [`crate::view::WalkEvent`]). [`visit`] drives a tree once, calling the
//! visitor for each event in DFS order. A visitor that wants early
//! termination returns [`ControlFlow::Break`].
//!
//! # Why a second seam, not a second pack
//!
//! M1 added the typed-view *consumer*; M2 adds the traversal *primitive*
//! that consumer (and the three hand-rolled walkers) build on. The cursor
//! remains the zero-allocation pull API; the visitor is the ergonomic push
//! API. Both share the same DFS contract — the visitor's event order is
//! byte-identical to [`crate::view::walk`].

use std::ops::ControlFlow;
use std::sync::Arc;

use crate::tree::{GreenChild, GreenNode};

/// A push-based visitor over a green tree.
///
/// Each method receives the event data and returns [`ControlFlow<()>`]:
/// `Continue` keeps walking, `Break(())` stops. The default impls are
/// no-ops, so a visitor implements only the events it cares about.
pub trait Visitor {
    /// Entered an interior node. The borrow lives for the duration of the
    /// callback (the driver holds the tree).
    fn enter(&mut self, _node: &GreenNode) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    /// Visited a token leaf (its kind and byte range).
    fn token(&mut self, _kind: crate::TokenKind, _range: std::ops::Range<u32>) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    /// Left an interior node (the borrow lives for the callback).
    fn leave(&mut self, _node: &GreenNode) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

/// Drive `visitor` over `root` in DFS order: `enter(root)`, then each child
/// in order (descending into interior nodes, calling `token` for leaves),
/// then `leave(root)`. Stops early if the visitor returns `Break`.
///
/// The event order is byte-identical to [`crate::view::walk`] (the pull
/// counterpart); the two are interchangeable for consumers that don't need
/// early termination.
///
/// # Examples
///
/// ```
/// use runtime_core::tree::{GreenChild, GreenNode};
/// use runtime_core::view::visit::{Visitor, visit};
/// use std::ops::ControlFlow;
/// use std::sync::Arc;
///
/// struct Counter { nodes: u32, tokens: u32 }
/// impl Visitor for Counter {
///     fn enter(&mut self, _n: &GreenNode) -> ControlFlow<()> { self.nodes += 1; ControlFlow::Continue(()) }
///     fn token(&mut self, _k: u16, _r: std::ops::Range<u32>) -> ControlFlow<()> { self.tokens += 1; ControlFlow::Continue(()) }
/// }
/// let leaf = Arc::new(GreenNode { kind: 1, text_len: 2, has_error: false, children: vec![GreenChild::Token { kind: 10, range: 0..2 }] });
/// let root = Arc::new(GreenNode { kind: 0, text_len: 3, has_error: false, children: vec![GreenChild::Node(leaf), GreenChild::Token { kind: 20, range: 2..3 }] });
/// let mut c = Counter { nodes: 0, tokens: 0 };
/// visit(&root, &mut c);
/// assert_eq!(c.nodes, 2); // root + leaf
/// assert_eq!(c.tokens, 2);
/// ```
pub fn visit<V: Visitor + ?Sized>(root: &Arc<GreenNode>, visitor: &mut V) -> ControlFlow<()> {
    visit_node(root, visitor)
}

/// Recursive driver over a borrowed node (the `Arc` entry point unwraps
/// once; recursion borrows children directly).
fn visit_node<V: Visitor + ?Sized>(node: &GreenNode, visitor: &mut V) -> ControlFlow<()> {
    visitor.enter(node)?;
    for child in &node.children {
        match child {
            GreenChild::Node(n) => visit_node(n, visitor)?,
            GreenChild::Token { kind, range } => visitor.token(*kind, range.clone())?,
        }
    }
    visitor.leave(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{GreenChild, GreenNode};
    use std::sync::Arc;

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

    /// Records every event as a string for order assertions.
    struct Recorder {
        events: Vec<String>,
    }
    impl Visitor for Recorder {
        fn enter(&mut self, n: &GreenNode) -> ControlFlow<()> {
            self.events.push(format!("enter({})", n.kind));
            ControlFlow::Continue(())
        }
        fn token(&mut self, k: crate::TokenKind, r: std::ops::Range<u32>) -> ControlFlow<()> {
            self.events
                .push(format!("token({}, {}..{})", k, r.start, r.end));
            ControlFlow::Continue(())
        }
        fn leave(&mut self, n: &GreenNode) -> ControlFlow<()> {
            self.events.push(format!("leave({})", n.kind));
            ControlFlow::Continue(())
        }
    }

    #[test]
    fn visit_emits_enter_token_leave_in_dfs_order() {
        let root = sample();
        let mut r = Recorder { events: Vec::new() };
        let _ = visit(&root, &mut r);
        assert_eq!(
            r.events,
            vec![
                "enter(0)",
                "enter(1)",
                "token(10, 0..2)",
                "leave(1)",
                "token(20, 2..3)",
                "leave(0)",
            ]
        );
    }

    #[test]
    fn visit_order_matches_the_pull_cursor() {
        use crate::view::walk;
        let root = sample();
        // Pull: collect kinds via walk().
        let pull: Vec<String> = walk(&root)
            .map(|e| match e {
                crate::view::WalkEvent::Enter(n) => format!("enter({})", n.kind),
                crate::view::WalkEvent::Leave(n) => format!("leave({})", n.kind),
                crate::view::WalkEvent::Token { kind, range } => {
                    format!("token({}, {}..{})", kind, range.start, range.end)
                }
            })
            .collect();
        // Push: collect via visit().
        let mut r = Recorder { events: Vec::new() };
        let _ = visit(&root, &mut r);
        assert_eq!(pull, r.events, "push and pull DFS orders are identical");
    }

    /// A visitor that breaks after the Nth enter (for the early-stop test).
    struct StopAfterOneNode {
        count: u32,
    }
    impl Visitor for StopAfterOneNode {
        fn enter(&mut self, _n: &GreenNode) -> ControlFlow<()> {
            self.count += 1;
            if self.count >= 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }

    #[test]
    fn visit_stops_early_on_break() {
        let root = sample();
        let mut v = StopAfterOneNode { count: 0 };
        let flow = visit(&root, &mut v);
        assert!(matches!(flow, ControlFlow::Break(())));
        assert_eq!(v.count, 2, "broke after the second enter");
    }

    #[test]
    fn visit_handles_empty_root() {
        let root = Arc::new(GreenNode {
            kind: 7,
            text_len: 0,
            has_error: false,
            children: Vec::new(),
        });
        let mut r = Recorder { events: Vec::new() };
        let _ = visit(&root, &mut r);
        assert_eq!(r.events, vec!["enter(7)", "leave(7)"]);
    }

    #[test]
    fn visit_drives_a_deeper_tree_without_loss() {
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
        let mut r = Recorder { events: Vec::new() };
        let _ = visit(&root, &mut r);
        assert_eq!(
            r.events,
            vec![
                "enter(0)",
                "enter(1)",
                "enter(2)",
                "token(9, 0..1)",
                "leave(2)",
                "token(8, 1..2)",
                "leave(1)",
                "token(7, 2..3)",
                "leave(0)",
            ]
        );
    }

    /// A visitor that implements no callbacks (exercises the default no-op impls).
    struct Noop;
    impl Visitor for Noop {}

    #[test]
    fn default_visitor_impls_are_no_ops() {
        let root = sample();
        let mut v = Noop;
        let flow = visit(&root, &mut v);
        assert!(matches!(flow, ControlFlow::Continue(())));
    }

    #[test]
    fn visit_dyn_dispatch_works() {
        // The ?Sized bound lets callers pass &mut dyn Visitor.
        let root = sample();
        let mut recorder = Recorder { events: Vec::new() };
        let v: &mut dyn Visitor = &mut recorder;
        let _ = visit(&root, v);
        assert!(!recorder.events.is_empty(), "dyn visitor received events");
    }
}

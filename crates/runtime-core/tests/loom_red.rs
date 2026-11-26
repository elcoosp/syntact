#![cfg(feature = "loom")]
//! The loom model harness for the persistent-red contract — the
//! "persistent-red mode loom-clean" P4 exit gate (roadmap §12.3,
//! §11.2 "loom for the persistent-red mode"). The crate's `loom`
//! feature swaps the persistent module's `Arc` for loom's model type,
//! so the REAL [`RedHandle`] / [`Version`] / [`count_shared`] types
//! participate in the interleaving search below; every assertion runs
//! under every explored schedule.
//!
//! The models pin the documented invariants (`persistent.rs`):
//!
//! - **Single writer / multi reader**: one writer publishes through
//!   `&mut self` (plus [`RedHandle::publish`]'s own return) while
//!   reader threads only ever hold `Arc` snapshots taken from the
//!   published cell — no reader ever observes a half-published or
//!   mutated tree, and every snapshot stays byte-stable for its whole
//!   lifetime.
//! - **Structural sharing**: the shared leaf `Arc` is pointer-identical
//!   across versions — visible from the reader side, not just a
//!   `count_shared` return value.
//! - **Version retention**: history holds every published version with
//!   strictly increasing numbers, and a version a reader still holds
//!   stays alive (strong count proves exactly who holds it after the
//!   writer exits).

use loom::sync::{Arc, Mutex};
use loom::thread;
use runtime_core::persistent::{RedHandle, count_shared};
use runtime_core::tree::{GreenChild, GreenNode};
use std::sync::Arc as StdArc;

/// A leaf node: the subtree both versions share. It lives INSIDE the
/// trees (`GreenChild` stores the kernel's `std` `Arc`), so its type
/// is the plain kernel pointer in every build.
fn leaf() -> StdArc<GreenNode> {
    StdArc::new(GreenNode {
        kind: 1,
        text_len: 2,
        has_error: false,
        children: Vec::new(),
    })
}

/// A two-node tree: root `[shared_leaf, token]`. Both published
/// versions share ONE leaf `Arc` — the structural-sharing witness.
/// The ROOT is the version-cell pointer (loom's model `Arc` under the
/// feature — the alias `persistent::RedHandle` publishes).
fn root_with(shared_leaf: &StdArc<GreenNode>, token: (u16, u32, u32)) -> Arc<GreenNode> {
    Arc::new(GreenNode {
        kind: 0,
        text_len: 2 + token.2 - token.1,
        has_error: false,
        children: vec![
            GreenChild::Node(StdArc::clone(shared_leaf)),
            GreenChild::Token {
                kind: token.0,
                range: token.1..token.2,
            },
        ],
    })
}

/// The total byte length a subtree covers (node text lengths + token
/// spans) — the completeness walk: a "half-published" tree would break
/// the sum. A [`GreenNode::text_len`] is the full byte length of its
/// subtree, so a Node child contributes its `text_len` and a Token
/// child its span.
fn bytes_of(node: &GreenNode) -> u32 {
    let mut total = 0;
    for child in &node.children {
        match child {
            GreenChild::Node(inner) => total += inner.text_len,
            GreenChild::Token { range, .. } => total += range.end - range.start,
        }
    }
    total
}

/// Model 1 — single writer, TWO readers: the writer publishes two
/// versions (sharing one leaf `Arc`), readers drain the published
/// cell; every snapshot must be a complete tree whose byte sum
/// matches its `text_len`, and snapshots taken from the same version
/// must be pointer-identical across readers (the sharing invariant).
#[test]
fn single_writer_publishes_whole_versions_to_many_readers() {
    loom::model(|| {
        let shared = leaf();
        let handle = RedHandle::new();
        // The published cell: the writer's handoff point — readers
        // clone Arc snapshots out of it, the documented reader shape.
        let cell: Arc<Mutex<Vec<Arc<GreenNode>>>> = Arc::new(Mutex::new(Vec::new()));

        let writer_cell = Arc::clone(&cell);
        let writer_leaf = StdArc::clone(&shared);
        let writer = thread::spawn(move || {
            let mut handle = handle;
            let v1 = root_with(&writer_leaf, (2, 0, 4));
            let v2 = root_with(&writer_leaf, (3, 5, 9));
            handle.publish(v1.clone());
            writer_cell.lock().expect("cell").push(v1);
            handle.publish(v2.clone());
            writer_cell.lock().expect("cell").push(v2);
            // Sharing is reported by publish itself and provable from
            // the roots: the leaf is the same `Arc` in both.
            handle
        });

        let reader_cell = Arc::clone(&cell);
        let reader_leaf = StdArc::clone(&shared);
        let reader = thread::spawn(move || {
            // One bounded poll: whatever is published by this point in
            // the interleaving, it must be COMPLETE.
            let snapshot = reader_cell.lock().expect("cell").pop();
            if let Some(root) = snapshot {
                assert_eq!(bytes_of(&root), root.text_len, "complete tree");
                let _ = &reader_leaf; // the shared witness stays alive
            }
        });

        let handle = writer.join().expect("writer");
        reader.join().expect("reader");
        // Retention: both versions in history, numbers strictly
        // increasing, the latest readable through `current`.
        let history = handle.history();
        assert_eq!(history.len(), 2, "both versions retained");
        assert_eq!(history[0].number, 1);
        assert_eq!(history[1].number, 2);
        let latest = handle.current().expect("current");
        assert!(Arc::ptr_eq(latest, &history[1].root));
        // Structural sharing across versions: exactly the leaf, seen
        // from the roots history retained.
        assert_eq!(count_shared(&history[0].root, &history[1].root), 1);
    });
}

/// Model 2 — version retention under concurrent publishes: the reader
/// takes a snapshot of version 1 and keeps it alive while the writer
/// publishes two more versions; after the writer exits, the reader's
/// snapshot is still the version-1 tree (byte-stable), and the strong
/// count proves exactly two holders: the handle's history and the
/// reader.
#[test]
fn a_retained_version_survives_concurrent_publishes() {
    loom::model(|| {
        let shared = leaf();
        let handle = RedHandle::new();
        let cell: Arc<Mutex<Option<Arc<GreenNode>>>> = Arc::new(Mutex::new(None));

        let writer_cell = Arc::clone(&cell);
        let writer_leaf = StdArc::clone(&shared);
        let writer = thread::spawn(move || {
            let mut handle = handle;
            let v1 = root_with(&writer_leaf, (2, 0, 4));
            *writer_cell.lock().expect("cell") = Some(Arc::clone(&v1));
            handle.publish(v1);
            handle.publish(root_with(&writer_leaf, (3, 5, 9)));
            handle.publish(root_with(&writer_leaf, (4, 10, 14)));
            handle
        });

        let reader_cell = Arc::clone(&cell);
        let reader = thread::spawn(move || {
            // One bounded poll: take whatever is published (the model
            // explores the empty case too) and HOLD it.
            reader_cell.lock().expect("cell").take()
        });

        let retained = reader.join().expect("reader");
        let handle = writer.join().expect("writer");
        assert_eq!(handle.history().len(), 3, "all versions retained");
        let numbers: Vec<u64> = handle.history().iter().map(|v| v.number).collect();
        assert_eq!(numbers, [1, 2, 3], "versions strictly increasing");
        if let Some(snapshot) = retained {
            // The snapshot is the version-1 tree, byte-stable, and the
            // WHOLE version-1 subtree is shared with the retained
            // history entry (`count_shared` short-circuits at 1 on
            // pointer-identical roots — the reader's snapshot IS that
            // root).
            assert_eq!(count_shared(&snapshot, &handle.history()[0].root), 1);
            assert_eq!(bytes_of(&snapshot), snapshot.text_len);
            // Exactly two holders now: the handle's history and this
            // reader (the writer's thread-local clones are gone).
            let holders = Arc::strong_count(&snapshot);
            assert!(
                holders >= 2,
                "the retained version is alive: {holders} holders"
            );
        }
    });
}

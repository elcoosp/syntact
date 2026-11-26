//! Example: the persistent-red seam ([`RedHandle`] + [`parallel_query`]).
//!
//! A single writer publishes two versions of a small tree through the
//! `&mut` gate — [`RedHandle::publish`] reports the structural-sharing
//! metric (how many old `Arc` subtrees the new root reuses) and the
//! version history retains every publish. The chunked query mode plans
//! the token stream into worker-shaped chunks and merges hits in
//! ascending start-offset order — the merged output is byte-for-byte
//! the sequential scan's, at every fan-out. The single-writer /
//! multi-reader invariants (whole-version publication, retention,
//! sharing) are additionally model-checked by `tests/loom_red.rs`
//! under the crate's test-only `loom` feature (which swaps the
//! version-cell `Arc` for loom's model type — the std-`Arc` demo
//! below is compiled out there, since its trees are plain kernel
//! pointers).
//!
//! Run: `cargo run -p runtime-core --example persistent_query`

#[cfg(not(feature = "loom"))]
use std::ops::Range;
#[cfg(not(feature = "loom"))]
use std::sync::Arc;

#[cfg(not(feature = "loom"))]
use runtime_core::persistent::{RedHandle, parallel_query, parallel_query_with, sequential_query};
#[cfg(not(feature = "loom"))]
use runtime_core::tree::{GreenChild, GreenNode};

/// Build a tiny root: `block { tok0 tok1 }` over 6 bytes, both tokens
/// `kind = 7`.
#[cfg(not(feature = "loom"))]
fn root(token_range: Range<u32>) -> Arc<GreenNode> {
    Arc::new(GreenNode {
        kind: 10,
        text_len: 6,
        has_error: false,
        children: vec![GreenChild::Token {
            kind: 7,
            range: token_range,
        }],
    })
}

#[cfg(feature = "loom")]
fn main() {
    println!(
        "persistent_query is a std-Arc demo; under the loom feature the \
         version cell is loom's model type — see tests/loom_red.rs"
    );
}

#[cfg(not(feature = "loom"))]
fn main() {
    // The single writer: publish version 1, then version 2. The
    // trees are distinct (no shared subtree) and each publish retains
    // its version in history.
    let mut handle = RedHandle::new();
    let v1 = root(0..2);
    let v2 = root(3..5);
    println!("publish v1: {} subtrees shared", handle.publish(v1));
    println!("publish v2: {} subtrees shared", handle.publish(v2));
    println!(
        "history: {:?} (latest = current)",
        handle
            .history()
            .iter()
            .map(|v| v.number)
            .collect::<Vec<_>>()
    );

    // The chunked query mode: the token stream as `(kind, range)`
    // pairs — four `kind = 7` tokens — planned into worker chunks,
    // scanned on scoped worker threads (issue 036's parallel query
    // mode), and merged in ascending start-offset order. Every
    // fan-out returns the sequential scan's output byte-for-byte.
    let tokens: Vec<(u16, Range<u32>)> = (0..4)
        .map(|i| {
            let start = u32::try_from(i * 3).unwrap_or(u32::MAX);
            (7, start..start + 2)
        })
        .collect();
    let sequential = sequential_query(&tokens, 7);
    println!("sequential: {sequential:?}");
    for workers in [1, 2, 4] {
        let merged = parallel_query(&tokens, 7, workers);
        assert_eq!(merged, sequential, "workers {workers}");
        println!(
            "workers {workers}: {} hits, merge order identical",
            merged.len()
        );
    }
    // The general form: a caller-provided scan predicate over the same
    // chunk plan (here: any kind — the kind-set query shape).
    let every_kind = parallel_query_with(&tokens, |_| true, 2);
    assert_eq!(every_kind.len(), tokens.len(), "every token is a hit");
    println!(
        "kind-set scan: {} hits over {} tokens, merge order identical",
        every_kind.len(),
        tokens.len()
    );
    println!("persistent query: chunked scan, deterministic ordered merge — parity holds");
}

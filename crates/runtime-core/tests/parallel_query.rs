//! Parallel-query suites (issue 036 exit criteria): the chunked query mode
//! runs its per-chunk scans on scoped worker threads over the `SoA` chunk
//! plan and merges the hits in ascending start-offset order — "parallel
//! merge order determinism on seeded corpora". The merged output MUST be
//! byte-for-byte the sequential scan's at every fan-out, whatever the
//! thread completion order.

use std::ops::Range;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use runtime_core::persistent::{Hit, parallel_query, parallel_query_with, sequential_query};

/// A deterministic seeded corpus: a 50k-token feed with pseudo-random
/// kinds (a small LCG — no rand dependency, stable across runs and
/// platforms) and tight, non-overlapping spans.
fn seeded_tokens(count: usize) -> Vec<(u16, Range<u32>)> {
    let mut state = 0x2545_f491_u32;
    let mut tokens = Vec::with_capacity(count);
    let mut cursor = 0_u32;
    for _ in 0..count {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let kind = u16::try_from((state >> 16) % 7).expect("kind fits u16");
        let len = (state >> 24) % 3 + 1;
        let end = cursor + len;
        tokens.push((kind, cursor..end));
        cursor = end;
    }
    tokens
}

/// The sequential reference for a predicate scan.
fn sequential_by(tokens: &[(u16, Range<u32>)], predicate: impl Fn(u16) -> bool) -> Vec<Hit> {
    tokens
        .iter()
        .filter(|(kind, _)| predicate(*kind))
        .map(|(kind, span)| Hit {
            kind: *kind,
            span: span.clone(),
        })
        .collect()
}

#[test]
fn parallel_kind_set_matches_sequential_at_every_fanout() {
    let tokens = seeded_tokens(50_000);
    let expected = sequential_by(&tokens, |kind| kind % 2 == 0);
    assert!(
        expected.len() > 1_000,
        "the seeded corpus exercises plenty of hits"
    );
    for workers in [0, 1, 2, 4, 8, 64] {
        let hits = parallel_query_with(&tokens, |kind| kind % 2 == 0, workers);
        assert_eq!(hits, expected, "workers {workers}: merged == sequential");
    }
}

#[test]
fn single_kind_api_matches_predicate_api() {
    let tokens = seeded_tokens(10_000);
    let wanted = 3_u16;
    let by_api = parallel_query(&tokens, wanted, 4);
    let by_predicate = parallel_query_with(&tokens, move |kind| kind == wanted, 4);
    assert_eq!(by_api, by_predicate);
    assert_eq!(by_api, sequential_query(&tokens, wanted));
}

#[test]
fn merge_order_deterministic_over_repetitions() {
    let tokens = seeded_tokens(20_000);
    let first = parallel_query(&tokens, 2, 4);
    // Real threads + repeated runs: every run merges to the same order.
    for _ in 0..25 {
        assert_eq!(parallel_query(&tokens, 2, 4), first, "run-to-run stable");
        assert_eq!(parallel_query(&tokens, 2, 8), first, "fan-out stable");
    }
    // The merged order itself is ascending by start offset.
    assert!(
        first
            .windows(2)
            .all(|pair| pair[0].span.start < pair[1].span.start)
    );
}

#[test]
fn scan_partition_never_loses_or_duplicates_tokens() {
    let tokens = seeded_tokens(30_000);
    let seen = AtomicUsize::new(0);
    let counter = &seen;
    let hits = parallel_query_with(
        &tokens,
        |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            false
        },
        3,
    );
    assert!(hits.is_empty(), "nothing matches the reject-all scan");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        tokens.len(),
        "every token was scanned exactly once across the workers"
    );
}

#[test]
fn empty_and_tiny_inputs_never_spawn_unbounded_work() {
    assert_eq!(parallel_query(&[], 1, 8), Vec::<Hit>::new());
    let tiny = vec![(1_u16, 0..1_u32), (2, 1..2), (1, 2..3)];
    let hits = parallel_query(&tiny, 1, 32);
    assert_eq!(hits.len(), 2, "the cap keeps more workers than work sane");
    assert_eq!(hits, sequential_query(&tiny, 1));
}

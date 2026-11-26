//! Criterion suites (issue 036 exit criteria, roadmap §12.3 P4 "persistent-
//! red + parallel query mode"): the chunked query mode's sequential scan
//! against the scoped-worker parallel drive on the same seeded corpus, at
//! several fan-outs. The merged output is conformance-gated to equal the
//! sequential scan byte for byte (`crates/runtime-core/tests/
//! parallel_query.rs`); these benches measure what the drive costs and
//! buys.

use criterion::{Criterion, criterion_group, criterion_main};
use runtime_core::persistent::{parallel_query, parallel_query_with, sequential_query};

/// A deterministic seeded corpus: a 50k-token feed with pseudo-random
/// kinds (the same LCG the parity suites use — no rand dependency).
fn seeded_tokens(count: usize) -> Vec<(u16, std::ops::Range<u32>)> {
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

fn persistent_query(c: &mut Criterion) {
    let tokens = seeded_tokens(50_000);
    c.bench_function("persistent_query/sequential", |b| {
        b.iter(|| sequential_query(&tokens, 2));
    });
    for workers in [1, 2, 4] {
        c.bench_function(&format!("persistent_query/parallel/{workers}"), |b| {
            b.iter(|| parallel_query(&tokens, 2, workers));
        });
    }
    c.bench_function("persistent_query/kind_set/4", |b| {
        b.iter(|| parallel_query_with(&tokens, |kind| kind % 2 == 0, 4));
    });
}

criterion_group!(benches, persistent_query);
criterion_main!(benches);

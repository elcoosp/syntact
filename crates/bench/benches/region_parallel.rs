//! Criterion suite for the region-parallel chunk drive: the sequential
//! `validate_chunks` baseline against the parallel drive at several
//! fan-outs (1 = same work with the thread plumbing, 2/4/8 = real
//! fan-outs, 0 = `available_parallelism`) plus the plan-and-drive
//! convenience. The document is built with top-level line comments
//! between objects on purpose: a comment closing at depth 0 is what
//! makes the Code region after it a chunk-boundary candidate, so the
//! 8-chunk target cuts a genuinely parallel plan (the single outer
//! object of `steady_state_document` would plan one chunk and the
//! fan-out would idle). The parity gate — parallel verdict ==
//! sequential verdict — lives in the tests; this group measures what
//! the §12.3 P4 gate ("region-parallel ≥1.5×") reads.

use criterion::{Criterion, criterion_group, criterion_main};
use structural_index::{
    plan_chunks, scan, validate_chunks, validate_chunks_parallel, validate_plan_parallel,
};

/// ~64 B per unit; `4_096` units ≈ 256 KiB with a depth-0 boundary
/// after every unit's comment — a real 8-chunk plan.
fn multi_chunk_document() -> String {
    let mut out = String::with_capacity(4096 * 72);
    for index in 0..4096 {
        out.push_str("{\"k0\": ");
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{index}"));
        out.push_str(", \"k1\": \"xxxxxxxx\"}\n// unit note\n");
    }
    out
}

fn region_parallel(c: &mut Criterion) {
    let source = multi_chunk_document();
    let index = scan(&source);
    let plan = plan_chunks(&source, &index, 8);
    assert_eq!(plan.len(), 8, "the fixture must plan a real 8-way split");
    c.bench_function("region_parallel/sequential_validate_chunks", |b| {
        b.iter(|| validate_chunks(&source, &plan));
    });
    for workers in [1, 2, 4, 8] {
        c.bench_function(
            &format!("region_parallel/parallel_validate_chunks_w{workers}"),
            |b| {
                b.iter(|| validate_chunks_parallel(&source, &plan, workers));
            },
        );
    }
    c.bench_function("region_parallel/parallel_validate_chunks_auto", |b| {
        b.iter(|| validate_chunks_parallel(&source, &plan, 0));
    });
    c.bench_function("region_parallel/validate_plan_parallel_target8", |b| {
        b.iter(|| validate_plan_parallel(&source, &index, 8, 4));
    });
}

criterion_group!(benches, region_parallel);
criterion_main!(benches);

//! Criterion suite for the region-sink seam: the streaming
//! `validate_fast` (a `FaultScanner` driven by `scan_regions`, no index
//! materialization) against the index-building `scan` over the same
//! steady-state document, plus the chunked validation seam
//! (`validate_plan` = planning + per-chunk validation, and
//! `validate_chunks` over a precomputed plan — the per-chunk cost the
//! region-parallel drive pays). Criterion defaults — the zero-allocation
//! gate lives in the dhat-counted tests.

use criterion::{Criterion, criterion_group, criterion_main};
use structural_index::{plan_chunks, scan, validate_chunks, validate_fast, validate_plan};

fn structural_validate(c: &mut Criterion) {
    let source = bench::steady_state_document();
    let index = scan(&source);
    let plan = plan_chunks(&source, &index, 4);
    c.bench_function("structural_validate/validate_fast_streaming", |b| {
        b.iter(|| validate_fast(&source));
    });
    c.bench_function("structural_validate/scan_build_index", |b| {
        b.iter(|| scan(&source));
    });
    c.bench_function("structural_validate/validate_plan_chunked", |b| {
        b.iter(|| validate_plan(&source, &index, 4));
    });
    c.bench_function("structural_validate/validate_chunks_only", |b| {
        b.iter(|| validate_chunks(&source, &plan));
    });
}

criterion_group!(benches, structural_validate);
criterion_main!(benches);

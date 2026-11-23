//! Criterion suite for the region-sink seam: the streaming
//! `validate_fast` (a `FaultScanner` driven by `scan_regions`, no index
//! materialization) against the index-building `scan` over the same
//! steady-state document. Run-fast config — the zero-allocation gate
//! lives in the dhat-counted tests.

use criterion::{Criterion, criterion_group, criterion_main};
use structural_index::{scan, validate_fast};

fn structural_validate(c: &mut Criterion) {
    let source = bench::steady_state_document();
    c.bench_function("structural_validate/validate_fast_streaming", |b| {
        b.iter(|| validate_fast(&source));
    });
    c.bench_function("structural_validate/scan_build_index", |b| {
        b.iter(|| scan(&source));
    });
}

criterion_group!(benches, structural_validate);
criterion_main!(benches);

//! Criterion suites (roadmap §8 `parse_partial`, the lezer `stopAt`
//! analogue): the viewport cut over the JSON pack's raw event stream —
//! what the truncation costs at early, middle, and late cut points
//! (the work is the kept prefix's), and the full-viewport cut that
//! must reproduce the fresh parse. The cut contract (well-formed
//! truncated streams, byte-faithful prefixes, forward-parent chains
//! never split) is conformance-gated by `crates/runtime-core/tests/
//! partial_viewport.rs`; these benches measure the transform.

use std::fmt::Write as _;

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference::parse_events;
use runtime_core::partial::cut_events;

/// The bench document (deterministic; no whitespace — trivia is a
/// caller-side concern the cut does not price).
fn bench_document(pairs: usize) -> String {
    let mut document = String::with_capacity(pairs * 24);
    document.push('{');
    for index in 0..pairs {
        if index > 0 {
            document.push(',');
        }
        let _ = write!(document, r#""key{index}":[1,2,{index}]"#);
    }
    document.push('}');
    document
}

fn partial(c: &mut Criterion) {
    let document = bench_document(500);
    let (events, tokens, _errors) = parse_events(&document);
    let len = u32::try_from(document.len()).expect("document fits u32");
    let quarter = len / 4;

    c.bench_function("partial/cut/quarter", |b| {
        b.iter(|| cut_events(&events, &tokens, quarter).resume_offset);
    });
    c.bench_function("partial/cut/half", |b| {
        b.iter(|| cut_events(&events, &tokens, len / 2).resume_offset);
    });
    c.bench_function("partial/cut/three_quarters", |b| {
        b.iter(|| cut_events(&events, &tokens, 3 * quarter).resume_offset);
    });
    c.bench_function("partial/cut/full", |b| {
        b.iter(|| cut_events(&events, &tokens, len).truncated);
    });
}

criterion_group!(benches, partial);
criterion_main!(benches);

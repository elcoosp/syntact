//! Criterion suites (roadmap §10.2, §12.2 "superinstruction mining"): the
//! descent engine's generic interpreter against the PGO-mined fused fast
//! path on the same fixtures, plus the mining cost itself (a one-time,
//! per-corpus profile step, not per document). The fused path's output is
//! conformance-gated to equal the generic run (`crates/descent/tests/
//! superinstr.rs`); these benches measure what the fusion buys.

use criterion::{Criterion, criterion_group, criterion_main};
use descent::superinstr::mine;
use descent::{parse_events, parse_events_with};
use descent_lower::lower;
use gir_syntax::load_grammar;

fn lowered_pipeline() -> (descent_lower::Lowered, lexgen::Scanner) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    (lowered, scanner)
}

fn superinstr(c: &mut Criterion) {
    for (source, name) in [
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ] {
        let (lowered, scanner) = lowered_pipeline();
        let feed = lowered.feed(&scanner, &source);
        // The profile here is the document's own feed (the corpus the
        // hot pairs are mined from); mining is a one-time cost.
        let set = mine(&[&feed], 4, 64);
        c.bench_function(&format!("superinstr/generic/{name}"), |b| {
            b.iter(|| parse_events(&lowered.desc, &feed, &source));
        });
        c.bench_function(&format!("superinstr/fused/{name}"), |b| {
            b.iter(|| parse_events_with(&lowered.desc, &feed, &source, Some(&set)));
        });
        c.bench_function(&format!("superinstr/mine/{name}"), |b| {
            b.iter(|| mine(&[&feed], 4, 64));
        });
    }
}

criterion_group!(benches, superinstr);
criterion_main!(benches);

//! Criterion head-to-head (issue 043): the generated JSON pipeline
//! (grammar → `descent-lower` → lexgen scan → descent engine → green
//! tree) against the hand-written reference pack on the same fixtures.
//! The generated path is expected to land in the same order of
//! magnitude — generation chases the reference (roadmap §12.4), it does
//! not have to beat it.

use criterion::{Criterion, criterion_group, criterion_main};
use descent_lower::Lowered;
use descent_lower::lower;
use gir_syntax::load_grammar;

/// Compile the fixture once per bench setup: the lowered descriptor
/// program plus its scanner.
fn compile() -> (Lowered, lexgen::Scanner) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    (lowered, scanner)
}

fn lowered_json(c: &mut Criterion) {
    for (source, name) in [
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ] {
        let (lowered, scanner) = compile();
        let trivia = lowered.trivia_kind.expect("fixture has trivia");
        c.bench_function(&format!("lowered/scan/{name}"), |b| {
            b.iter(|| lowered.feed(&scanner, &source));
        });
        let tokens = lowered.feed(&scanner, &source);
        c.bench_function(&format!("lowered/parse_events/{name}"), |b| {
            b.iter(|| descent::parse_events(&lowered.desc, &tokens, &source));
        });
        c.bench_function(&format!("lowered/green_tree/{name}"), |b| {
            b.iter(|| {
                let tokens = lowered.feed(&scanner, &source);
                let run = descent::parse_events(&lowered.desc, &tokens, &source);
                descent::green_tree(&run, &source, packs_json::reference::ROOT_KIND, trivia)
                    .expect("tree integration")
            });
        });
        // The hand-written reference head-to-head on the same fixture.
        c.bench_function(&format!("lowered/reference_parse/{name}"), |b| {
            b.iter(|| packs_json::reference::parse_events(&source));
        });
        c.bench_function(&format!("lowered/reference_tree/{name}"), |b| {
            b.iter(|| packs_json::parse(&source).expect("reference parses"));
        });
    }
    // The compile step itself: grammar → descriptors (a one-time,
    // millisecond-class cost per document set, not per document).
    c.bench_function("lowered/compile/json", |b| {
        b.iter(|| {
            let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("loads");
            lower(&grammar).expect("lowers")
        });
    });
}

criterion_group!(benches, lowered_json);
criterion_main!(benches);

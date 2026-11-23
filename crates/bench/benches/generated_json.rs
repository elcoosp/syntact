//! Criterion head-to-head (issue 045): the **generated** parser crate
//! (the codegen-rust descent backend's emitted output, regenerated on
//! every build via `generated-json`'s build.rs) against the interpreted
//! `descent` engine and the hand-written reference pack on the same
//! fixtures. The generated path is expected to beat the engine (no
//! per-entry descriptor clones, no interpreter dispatch) while staying
//! within a constant factor of the reference (generation chases the
//! reference, roadmap §12.4).

use criterion::{Criterion, criterion_group, criterion_main};
use descent_lower::lower;
use gir_syntax::load_grammar;

/// Compile the fixture once per bench setup: the lowered descriptor
/// program plus its scanner (the engine's inputs).
fn engine_pipeline() -> (descent_lower::Lowered, lexgen::Scanner) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    (lowered, scanner)
}

fn generated_json(c: &mut Criterion) {
    for (source, name) in [
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ] {
        // The generated parser (emitted code, compiled into the workspace
        // via generated-json's build.rs).
        c.bench_function(&format!("generated/parse/{name}"), |b| {
            b.iter(|| generated_json::parse_events(&source));
        });
        c.bench_function(&format!("generated/tree/{name}"), |b| {
            b.iter(|| generated_json::parse(&source).expect("generated parses"));
        });
        // The interpreted engine head-to-head on the same fixture.
        // Apples-to-apples: the generated path scans internally
        // (`parse_events` → `scanner::feed`), so the engine bench measures
        // scan + parse together (not parse alone).
        let (lowered, scanner) = engine_pipeline();
        let trivia = lowered.trivia_kind.expect("fixture has trivia");
        c.bench_function(&format!("generated/engine/parse/{name}"), |b| {
            b.iter(|| {
                let tokens = lowered.feed(&scanner, &source);
                descent::parse_events(&lowered.desc, &tokens, &source)
            });
        });
        c.bench_function(&format!("generated/engine/tree/{name}"), |b| {
            b.iter(|| {
                let tokens = lowered.feed(&scanner, &source);
                let run = descent::parse_events(&lowered.desc, &tokens, &source);
                descent::green_tree(&run, &source, packs_json::reference::ROOT_KIND, trivia)
                    .expect("engine tree")
            });
        });
        // The hand-written reference pack on the same fixture (the
        // ceiling the generated parser chases).
        c.bench_function(&format!("generated/reference/tree/{name}"), |b| {
            b.iter(|| packs_json::parse(&source).expect("reference parses"));
        });
    }
}

criterion_group!(benches, generated_json);
criterion_main!(benches);

//! Criterion suites (roadmap §6.2, ADR-007): the ambiguity-mark pass over
//! an ambiguous fixture — conflict attribution through production origins
//! plus the annotated reclassification. Run-fast config — budgets are
//! enforced by tests, benches exist to watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use gir_syntax::load_grammar;
use lr_shadow::Automaton;

/// An ambiguous fixture: several dispatch rules whose alternatives
/// contend, one annotated as an island candidate.
const AMBIGUOUS_GIR: &str = r#"
language "amb" version="1.0.0"

tokens {
    a kind=lit "a"
    b kind=lit "b"
}

grammar {
    s0 kind=alt p0 q0 allow-ambiguous="true"
    s1 kind=alt p1 q1
    s2 kind=alt p2 q2
    p0 kind=seq a
    q0 kind=seq a
    p1 kind=seq b
    q1 kind=seq b
    p2 kind=seq a b
    q2 kind=seq a b
}
"#;

/// The pipeline: the fixture grammar, its automaton, and the annotated
/// report the CLI prints.
fn pipeline() -> (gir_ir::Grammar, Automaton) {
    let grammar = load_grammar(AMBIGUOUS_GIR).expect("fixture loads");
    let automaton = Automaton::build(&grammar);
    (grammar, automaton)
}

fn attribution_and_classification(bench: &mut Criterion) {
    let (grammar, automaton) = pipeline();
    bench.bench_function("ambiguity/marks", |b| {
        b.iter(|| lr_shadow::ambiguity_marks(&automaton));
    });
    bench.bench_function("ambiguity/report_classification", |b| {
        b.iter(|| gir_analysis::DeterminismReport::compute_with_ambiguities(&grammar, &automaton));
    });
    let report = gir_analysis::DeterminismReport::compute_with_ambiguities(&grammar, &automaton);
    println!(
        "fixture: {} rules, {} marks, {} islands",
        report.verdicts.len(),
        report.ambiguities.len(),
        report
            .ambiguities
            .iter()
            .filter(|ambiguity| ambiguity.allowed)
            .count(),
    );
}

criterion_group!(ambiguity, attribution_and_classification);
criterion_main!(ambiguity);

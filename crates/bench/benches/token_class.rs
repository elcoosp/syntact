//! Criterion suites (roadmap §6.1 pass 7): the token-class inference over
//! the JSON fixture automaton — the action-column equivalence pass whose
//! grouping the driver-substitution gate checks. Run-fast config —
//! budgets are enforced by tests, benches exist to watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use gir_syntax::load_grammar;
use lr_shadow::{Automaton, TokenClasses};

/// The JSON fixture pipeline: the compiled automaton and its grammar.
fn pipeline() -> (Automaton, gir_ir::Grammar) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let automaton = Automaton::build(&grammar);
    (automaton, grammar)
}

/// The pass-7 grouping over the fixture automaton (states × tokens).
fn compute_classes(bench: &mut Criterion) {
    let (automaton, grammar) = pipeline();
    bench.bench_function("token_class/json_fixture", |b| {
        b.iter(|| TokenClasses::compute(&automaton, &grammar));
    });
    let classes = TokenClasses::compute(&automaton, &grammar);
    println!(
        "json fixture: {} tokens -> {} classes ({} merged)",
        classes.token_count(),
        classes.len(),
        classes.merged(),
    );
}

criterion_group!(token_class, compute_classes);
criterion_main!(token_class);

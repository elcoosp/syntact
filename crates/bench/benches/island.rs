//! Criterion suites (roadmap §7.4): the GLR island executor over the
//! JSON fixture automaton — the deterministic grammar runs one linear
//! GSS (no forks, the merge machinery idle), and the ambiguous pair
//! fixture exercises the fork/merge/re-queue paths. Run-fast config —
//! budgets are enforced by tests, benches exist to watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use gir_syntax::load_grammar;
use lr_shadow::island::IslandCaps;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, driver, island};

/// The JSON fixture pipeline: the compiled automaton and the feed for
/// one object document, resolved by token name (the automaton's own
/// alphabet — the same feed shape the descent integration bridges).
fn json_pipeline() -> (Automaton, Vec<Look>) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let automaton = Automaton::build(&grammar);
    let token = |name: &str| {
        let index = grammar
            .tokens
            .iter()
            .position(|def| grammar.interner.lookup(def.name) == name)
            .expect("fixture token");
        Look::token(gir_ir::TokenId(
            u16::try_from(index).expect("token count fits u16"),
        ))
    };
    let feed: Vec<Look> = vec![
        token("lbrace"),
        token("string"),
        token("colon"),
        token("number"),
        token("rbrace"),
    ];
    (automaton, feed)
}

/// The ambiguous nested-pair fixture: the executor's fork machinery.
fn nested_pipeline() -> (Automaton, Vec<Look>) {
    let mut b = gir_ir::GrammarBuilder::new("nested-bench");
    let a = b.token_lit("a", "a").expect("token");
    let s = b.declare_rule("s").expect("rule");
    b.set_body(
        s,
        gir_ir::GrammarBuilder::alt(vec![
            gir_ir::GrammarBuilder::seq(vec![
                gir_ir::GrammarBuilder::token_ref(a),
                gir_ir::GrammarBuilder::rule_ref(s),
                gir_ir::GrammarBuilder::token_ref(a),
            ]),
            gir_ir::GrammarBuilder::seq(vec![]),
        ]),
    );
    b.set_allow_ambiguous(s, true);
    let automaton = Automaton::build(&b.finish().expect("valid grammar"));
    let feed = vec![
        Look::token(a),
        Look::token(a),
        Look::token(a),
        Look::token(a),
    ];
    (automaton, feed)
}

/// The executor over the deterministic JSON feed (one linear GSS).
fn island_linear(bench: &mut Criterion) {
    let (automaton, feed) = json_pipeline();
    bench.bench_function("island/json_linear", |b| {
        b.iter(|| island::parse(&automaton, &feed, &IslandCaps::default()));
    });
    let table = driver::parse(&automaton, &feed);
    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    println!(
        "json fixture: {} tokens, island accepted {} (table {}), forks {}",
        feed.len(),
        run.accepted,
        table.accepted,
        run.forks,
    );
}

/// The executor over the ambiguous nested-pair feed (fork + unwind).
fn island_ambiguous(bench: &mut Criterion) {
    let (automaton, feed) = nested_pipeline();
    bench.bench_function("island/nested_ambiguous", |b| {
        b.iter(|| island::parse(&automaton, &feed, &IslandCaps::default()));
    });
    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    println!(
        "nested fixture: {} tokens, accepted {}, forks {}, merges {}",
        feed.len(),
        run.accepted,
        run.forks,
        run.merges,
    );
}

criterion_group!(island_benches, island_linear, island_ambiguous);
criterion_main!(island_benches);

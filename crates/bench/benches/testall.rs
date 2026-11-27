//! Criterion suites (roadmap §7.4 "Cross-backend oracle"): the descent
//! engine's generic interpreter against the table-driven LR(1) driver on
//! the same scanner feed — the two backends the `syntact test-all` oracle
//! differential-tests. Their outputs are agreement-gated per document
//! (`crates/cli/tests/testall.rs`); these benches measure what each
//! backend costs per document. Run-fast config — budgets are enforced by
//! dhat-counted tests, benches exist to watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use descent::parse_events;
use descent_lower::lower;
use gir_ir::TokenId;
use gir_syntax::load_grammar;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, driver};

/// Deterministic JSON corpus: small / medium / steady-state.
fn corpus() -> Vec<(String, &'static str)> {
    vec![
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ]
}

/// The full oracle pipeline for the JSON fixture: lowered descriptors,
/// the compiled scanner, and the shadow automaton with the feed-kind →
/// terminal map the oracle builds (bad tokens ride a terminal outside
/// the token table).
struct Pipeline {
    lowered: descent_lower::Lowered,
    scanner: lexgen::Scanner,
    automaton: Automaton,
    look_of_kind: Vec<Look>,
}

fn pipeline() -> Pipeline {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let automaton = Automaton::build(&grammar);
    let stranger = Look(u32::MAX - 1);
    let mut look_of_kind = vec![stranger; usize::from(lowered.bad_kind) + 1];
    for (index, &kind) in lowered.kind_of_token.iter().enumerate() {
        if !grammar.tokens[index].trivia {
            look_of_kind[usize::from(kind)] =
                Look::token(TokenId(u16::try_from(index).expect("token count fits u16")));
        }
    }
    look_of_kind[usize::from(lowered.bad_kind)] = stranger;
    Pipeline {
        lowered,
        scanner,
        automaton,
        look_of_kind,
    }
}

fn backends(c: &mut Criterion) {
    let built = pipeline();
    for (source, name) in corpus() {
        let feed = built.lowered.feed(&built.scanner, &source);
        let looks: Vec<Look> = feed
            .iter()
            .map(|(kind, _)| built.look_of_kind[usize::from(*kind)])
            .collect();
        c.bench_function(&format!("testall/descent/{name}"), |b| {
            b.iter(|| parse_events(&built.lowered.desc, &feed, &source));
        });
        c.bench_function(&format!("testall/table/{name}"), |b| {
            b.iter(|| driver::parse(&built.automaton, &looks));
        });
    }
}

criterion_group!(benches, backends);
criterion_main!(benches);

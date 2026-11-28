//! Consumer example: the GLR island executor (roadmap §7.4, ADR-007)
//! over the classic ambiguous nested-pair grammar `s → a s a | ε`. The
//! shift-wins-resolved table dead-ends on `aa`; the island executor forks
//! at the recorded conflict and accepts — the runtime for the regions the
//! analyzer marks `@allow_ambiguous`.
//!
//! Run: `cargo run -p lr-shadow --example island_exec`

use gir_syntax::load_grammar;
use lr_shadow::Automaton;
use lr_shadow::driver;
use lr_shadow::island;
use lr_shadow::island::IslandCaps;
use lr_shadow::lower::Look;

const GRAMMAR: &str = r#"
language "nested" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s     kind=alt inner empty allow-ambiguous="true" node="true"
    inner kind=seq a s a
    empty kind=seq
}
"#;

fn main() {
    let grammar = load_grammar(GRAMMAR).expect("grammar loads");
    let automaton = Automaton::build(&grammar);
    // The feed is the automaton's own alphabet here: one `a` token.
    let feed = [
        Look::token(gir_ir::TokenId(0)),
        Look::token(gir_ir::TokenId(0)),
    ];

    let table = driver::parse(&automaton, &feed);
    println!(
        "table driver: accepted={} (the shift-wins resolution dead-ends)",
        table.accepted,
    );

    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    println!(
        "island: accepted={}, consumed={}/{} — forks {} at the recorded \
         conflict, merges {}, {} arena nodes (shared tails, no per-version \
         stacks)",
        run.accepted,
        run.consumed,
        feed.len(),
        run.forks,
        run.merges,
        run.versions_spawned,
    );
    let replay: Vec<String> = run
        .ops
        .iter()
        .map(|op| match op {
            lr_shadow::IslandOp::Shift => "shift".to_string(),
            lr_shadow::IslandOp::Reduce(prod) => format!("reduce {prod}"),
        })
        .collect();
    println!("winning derivation: {}", replay.join(" -> "));
    assert!(
        run.accepted && !table.accepted,
        "the island accepts what the deterministic resolution cannot"
    );
}

//! Consumer example: the analyzer-side ambiguity marking (roadmap §6.2,
//! ADR-007) over a genuinely ambiguous grammar — the shadow automaton's
//! reduce/reduce contention attributes back to the grammar rules and the
//! determinism report renders the two counter-example derivations; the
//! `allow-ambiguous="true"` annotation accepts the region as a GLR
//! island candidate so the build continues.
//!
//! Run: `cargo run -p gir-analysis --example ambiguous_report`

use gir_syntax::load_grammar;
use lr_shadow::Automaton;

/// Two alternatives derive the same string: the dispatch rule `s` cannot
/// choose between `p` and `q`, and the automaton records the contention
/// on the shared lookahead.
const AMBIGUOUS_GIR: &str = r#"
language "amb" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s kind=alt p q allow-ambiguous="true"
    p kind=seq a
    q kind=seq a
}
"#;

fn main() {
    let grammar = load_grammar(AMBIGUOUS_GIR).expect("grammar loads");
    let automaton = Automaton::build(&grammar);
    let report = gir_analysis::DeterminismReport::compute_with_ambiguities(&grammar, &automaton);

    println!("{report}");
    for verdict in &report.verdicts {
        if verdict.backend == gir_analysis::Backend::GlrIsland {
            println!(
                "-> rule {} accepted as a GLR island candidate (@allow_ambiguous)",
                verdict.rule.0
            );
        }
    }
}

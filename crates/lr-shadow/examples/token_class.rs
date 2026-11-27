//! Consumer example: token-class inference (roadmap §6.1 pass 7) over a
//! small grammar — the declared-but-unused reserved keywords collapse
//! into one never-distinguished class, and the used token stays alone.
//!
//! Run: `cargo run -p lr-shadow --example token_class`

use gir_syntax::load_grammar;
use lr_shadow::{Automaton, TokenClasses};

const GRAMMAR: &str = r#"
language "tc" version="1.0.0" extension="tc"

tokens {
    num    kind=re "[0-9]+"
    dead_a kind=lit "alpha"
    dead_b kind=lit "beta"
}

grammar {
    document kind=seq num
}
"#;

fn main() {
    let grammar = load_grammar(GRAMMAR).expect("grammar loads");
    let automaton = Automaton::build(&grammar);
    let classes = TokenClasses::compute(&automaton, &grammar);

    println!(
        "{} tokens -> {} classes ({} merged)",
        classes.token_count(),
        classes.len(),
        classes.merged(),
    );
    for class in classes.classes() {
        let names: Vec<String> = class
            .iter()
            .map(|token| {
                grammar
                    .interner
                    .lookup(grammar.tokens[usize::from(token.0)].name)
                    .to_string()
            })
            .collect();
        println!("  class [{}]: {}", class[0], names.join(", "));
    }

    // The checked property: same-class tokens are machine-
    // indistinguishable — every class member beyond the representative
    // never changes the table's verdicts.
    println!(
        "representative of `dead_b`: {}",
        classes
            .representative(gir_ir::TokenId(2))
            .map_or("?".to_string(), |token| token.0.to_string()),
    );
}

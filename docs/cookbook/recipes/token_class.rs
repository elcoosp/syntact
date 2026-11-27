// Recipe: group the tokens the parser machinery never distinguishes.
//
// Roadmap §6.1 pass 7 ("Token-class inference") groups tokens that are
// never distinguished in any parser state — the tree-sitter
// `ts_symbol_map` idea, extended: the grouping is computed over the
// resolved LR(1) action table AND checked, because mapping a feed to
// class representatives must never change the table-driven driver's
// verdict. Declared-but-unused tokens (reserved keywords nobody's rule
// references) collapse into one never-distinguished class.

use gir_syntax::load_grammar;
use gir_ir::TokenId;
use lr_shadow::driver;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, TokenClasses};

#[test]
fn group_never_distinguished_tokens() {
    // `dead_a`/`dead_b` are declared but no rule references them: no
    // reachable parser state acts on either, so they are machine-
    // indistinguishable and share a class.
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
    let grammar = load_grammar(GRAMMAR).expect("grammar loads");
    let automaton = Automaton::build(&grammar);
    let classes = TokenClasses::compute(&automaton, &grammar);

    // Three declared tokens -> two classes; exactly one token merged.
    assert_eq!(classes.len(), 2);
    assert_eq!(classes.merged(), 1);
    let members: Vec<Vec<u16>> = classes
        .classes()
        .iter()
        .map(|class| class.iter().map(|token| token.0).collect())
        .collect();
    assert_eq!(members, vec![vec![0], vec![1, 2]]);

    // The representative is the class's smallest member; `dead_b`
    // resolves to `dead_a`.
    assert_eq!(
        classes.representative(TokenId(2)),
        Some(TokenId(1)),
        "dead_b's representative is dead_a"
    );

    // The checked property, end-to-end: substituting representatives
    // preserves the table-driven driver's verdict on every feed.
    let num = Look::token(TokenId(0));
    let dead_a = Look::token(TokenId(1));
    let dead_b = Look::token(TokenId(2));
    for feed in [
        vec![num],
        vec![dead_a],
        vec![dead_b],
        vec![num, dead_a, num],
        vec![num, dead_b, num],
    ] {
        let mapped: Vec<Look> = feed
            .iter()
            .map(|look| match look.as_token() {
                Some(token) => Look::token(classes.representative(token).unwrap_or(token)),
                None => *look,
            })
            .collect();
        assert_eq!(
            driver::parse(&automaton, &feed).accepted,
            driver::parse(&automaton, &mapped).accepted,
            "representative substitution must not change the verdict"
        );
    }
}

//! The token-class inference gate (roadmap §6.1 pass 7).
//!
//! Plan line pinned (§6.1 pass 7 "Token-class inference"): *"group tokens
//! never distinguished in any parser state"* — the tree-sitter `ts_symbol_map`
//! idea, extended. The concrete predicate: two terminals share a class iff
//! their action columns over the resolved LR(1) table are identical (every
//! state shifts them to the same target, reduces the same production, or
//! acts on neither) — and the extension: the map is *checked*, not just
//! computed, because substituting a feed's tokens by their representatives
//! must never change the table-driven driver's verdict (machine-level
//! indistinguishability, verified end-to-end with `lr_shadow::driver::parse`).

use gir_ir::TokenId;
use gir_syntax::load_grammar;
use lr_shadow::driver;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, TokenClasses};

/// A grammar with two declared-but-unused tokens (`dead_a`, `dead_b`) —
/// the reserved-keyword case: no production references them, so no
/// reachable parser state acts on either. They must land in one
/// never-distinguished class; the used token stays its own class.
const DEAD_TOKEN_GIR: &str = r#"
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

/// A grammar where every token is used in a distinguishable position:
/// `a` opens the document, `b` closes it — both shiftable, always in
/// distinct states, so each is its own class.
const USED_TOKENS_GIR: &str = r#"
language "ut" version="1.0.0" extension="ut"

tokens {
    a kind=lit "a"
    b kind=lit "b"
}

grammar {
    document kind=seq a b
}
"#;

fn classes_for(grammar_text: &str) -> (Automaton, TokenClasses, usize) {
    let grammar = load_grammar(grammar_text).expect("fixture grammar loads");
    let token_count = grammar.tokens.len();
    let automaton = Automaton::build(&grammar);
    let classes = TokenClasses::compute(&automaton, &grammar);
    (automaton, classes, token_count)
}

#[test]
fn dead_tokens_share_one_never_distinguished_class() {
    let (_, classes, token_count) = classes_for(DEAD_TOKEN_GIR);

    // Three declared tokens; the two dead ones merge: one class holds
    // {dead_a, dead_b}, one holds {num}.
    assert_eq!(classes.len(), 2, "two classes: the dead pair and num");
    assert_eq!(classes.merged(), 1, "exactly one token merged away");
    assert_eq!(classes.token_count(), token_count);

    let members: Vec<Vec<u16>> = classes
        .classes()
        .iter()
        .map(|class| class.iter().map(|token| token.0).collect())
        .collect();
    assert!(
        members.contains(&vec![0]),
        "num (id 0) is its own class, got {members:?}"
    );
    assert!(
        members.contains(&vec![1, 2]),
        "the dead pair merges, got {members:?}"
    );
}

#[test]
fn distinguished_tokens_stay_apart() {
    let (_, classes, _) = classes_for(USED_TOKENS_GIR);

    // Both tokens shift (in distinct states, to distinct targets), so
    // the table distinguishes them and each class is a singleton.
    assert_eq!(classes.len(), 2, "each used token is its own class");
    assert_eq!(classes.merged(), 0, "nothing merged");
    for class in classes.classes() {
        assert_eq!(class.len(), 1, "singleton class {class:?}");
    }
}

#[test]
fn classes_are_ordered_by_representative_and_resolvable() {
    let (_, classes, _) = classes_for(DEAD_TOKEN_GIR);

    // Classes are ordered by their smallest member; the representative
    // of a member is that smallest member; `class_of` round-trips.
    let reps: Vec<u16> = classes.classes().iter().map(|class| class[0].0).collect();
    let mut sorted = reps.clone();
    sorted.sort_unstable();
    assert_eq!(reps, sorted, "classes ordered by representative");

    let dead_rep = classes.representative(TokenId(2)).expect("token in table");
    assert_eq!(dead_rep.0, 1, "dead_b's representative is dead_a");
    let class_index = classes.class_of(TokenId(1)).expect("token in table");
    assert_eq!(classes.class_of(TokenId(2)), Some(class_index));
    assert_ne!(
        classes.class_of(TokenId(0)),
        Some(class_index),
        "num is not in the dead class"
    );
}

#[test]
fn eof_never_enters_a_class() {
    let (_, classes, _) = classes_for(DEAD_TOKEN_GIR);

    // The pseudo-EOF sentinel is not a token; every class member must
    // convert to a TokenId.
    for class in classes.classes() {
        for token in class {
            let _ = token.0;
        }
    }
    assert_eq!(
        classes.classes().iter().map(Vec::len).sum::<usize>(),
        classes.token_count(),
        "classes partition exactly the token table"
    );
}

#[test]
fn substitution_by_representative_never_changes_the_driver() {
    // The extension, end-to-end: same-class tokens are machine-
    // indistinguishable, so mapping every feed token to its class
    // representative must preserve the driver's verdict on every feed.
    let (automaton, classes, _) = classes_for(DEAD_TOKEN_GIR);

    let num = Look::token(TokenId(0));
    let dead_a = Look::token(TokenId(1));
    let dead_b = Look::token(TokenId(2));
    let feeds: Vec<Vec<Look>> = vec![
        vec![num],
        vec![num, num],
        vec![dead_a],
        vec![dead_b],
        vec![num, dead_a, num],
        vec![num, dead_b, num],
        vec![dead_a, dead_b],
        Vec::new(),
    ];

    let verdict = |tokens: &[Look]| -> bool { driver::parse(&automaton, tokens).accepted };
    for feed in &feeds {
        let mapped: Vec<Look> = feed
            .iter()
            .map(|look| match look.as_token() {
                Some(token) => Look::token(classes.representative(token).unwrap_or(token)),
                None => *look,
            })
            .collect();
        assert_eq!(
            verdict(feed),
            verdict(&mapped),
            "substituting representatives changed the verdict on {feed:?}"
        );
    }
}

//! The ambiguity-mark attribution gate (roadmap §6.2 / ADR-007).
//!
//! Plan line pinned (§6.2): *"otherwise **ambiguous** — the region becomes
//! a GLR island candidate (requires `@allow_ambiguous` or gets an error
//! with two counter-example derivations)"* — the automaton's recorded
//! conflicts attribute back to grammar rules through production origins,
//! carrying the two contending productions (the derivations) and the
//! states where the contention arose. Attribution is a deterministic
//! function of the automaton.

use gir_syntax::load_grammar;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, ConflictKind};

/// Two alternatives reduce on the same lookahead: the reduce/reduce
/// contention between `p -> a` and `q -> a` on the pseudo-EOF.
const RR_GIR: &str = r#"
language "amb" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s kind=alt p q
    p kind=seq a
    q kind=seq a
}
"#;

/// A conflict-free grammar: nothing to attribute.
const CLEAN_GIR: &str = r#"
language "clean" version="1.0.0"

tokens {
    a kind=lit "a"
    b kind=lit "b"
}

grammar {
    document kind=seq a b
}
"#;

#[test]
fn reduce_reduce_contentions_attribute_to_the_first_declaring_rule() {
    let grammar = load_grammar(RR_GIR).expect("fixture loads");
    let automaton = Automaton::build(&grammar);
    let marks = lr_shadow::ambiguity_marks(&automaton);

    assert_eq!(marks.len(), 1, "one contention group: the p/q reduce pair");
    let mark = &marks[0];
    let rule_p = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "p")
        .expect("rule p")
        .id;
    assert_eq!(mark.rule, rule_p, "attribution: first declaring origin");
    assert_eq!(
        mark.term,
        Look::EOF,
        "the contention is on the pseudo-EOF lookahead"
    );
    assert_eq!(mark.kind, ConflictKind::ReduceReduce);
    assert_eq!(
        mark.sides.len(),
        2,
        "the two contending productions ride the mark"
    );
    let origins: Vec<_> = mark.sides.iter().filter_map(|side| side.origin).collect();
    assert!(
        origins.contains(&rule_p),
        "the reduce side origins include `p`"
    );
    assert!(
        !mark.states.is_empty(),
        "the state where the contention arose is recorded"
    );
}

#[test]
fn marks_are_deterministic_for_a_given_automaton() {
    let grammar = load_grammar(RR_GIR).expect("fixture loads");
    let first = lr_shadow::ambiguity_marks(&Automaton::build(&grammar));
    let second = lr_shadow::ambiguity_marks(&Automaton::build(&grammar));

    let render = |marks: &[lr_shadow::AmbiguityMark]| -> Vec<String> {
        marks
            .iter()
            .map(|mark| {
                format!(
                    "{} {:?} {} {} {:?}",
                    mark.rule,
                    mark.term,
                    match mark.kind {
                        ConflictKind::ShiftReduce => "sr",
                        ConflictKind::ReduceReduce => "rr",
                    },
                    mark.sides
                        .iter()
                        .map(|side| (side.origin, side.lhs, side.symbols.len()))
                        .collect::<Vec<_>>()
                        .iter()
                        .map(|(o, l, n)| format!("{o:?}/{l}/{n}"))
                        .collect::<Vec<_>>()
                        .join(","),
                    mark.states
                )
            })
            .collect()
    };
    assert_eq!(render(&first), render(&second));
}

#[test]
fn conflict_free_automatons_produce_no_marks() {
    let grammar = load_grammar(CLEAN_GIR).expect("fixture loads");
    let automaton = Automaton::build(&grammar);
    assert_eq!(automaton.conflicts, [] as [lr_shadow::Conflict; 0]);
    assert_eq!(
        lr_shadow::ambiguity_marks(&automaton),
        [] as [lr_shadow::AmbiguityMark; 0]
    );
}

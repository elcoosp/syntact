//! The analyzer-side ambiguity marking gate (roadmap §6.1 pass 4, §6.2,
//! ADR-007).
//!
//! Plan line pinned (§6.2): *"otherwise **ambiguous** — the region becomes
//! a GLR island candidate (requires `@allow_ambiguous` or gets an error
//! with two counter-example derivations)"* — the determinism report gains
//! the ambiguous verdict: automaton conflicts attribute back to grammar
//! rules, propagate to their enclosing regions, render the two contending
//! derivations, and the `@allow_ambiguous` annotation (issue: `allow-ambiguous="true"`
//! on the surface) reclassifies an annotated table-fallback region as a
//! GLR island candidate so the build continues.

use gir_analysis::{Backend, DeterminismReport};
use gir_syntax::load_grammar;
use lr_shadow::Automaton;

/// A genuinely ambiguous grammar: `s` dispatches `p` and `q` on the same
/// token (LL(1) conflict), and the shadow automaton records the reduce/
/// reduce contention between `p -> a` and `q -> a` on the shared lookahead.
const AMBIGUOUS_GIR: &str = r#"
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

/// The same grammar with the annotation on the dispatch rule: the
/// ambiguous region is accepted as an island candidate.
const ANNOTATED_GIR: &str = r#"
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

/// A deterministic grammar: no conflicts anywhere, no ambiguity marks.
const DETERMINISTIC_GIR: &str = r#"
language "det" version="1.0.0"

tokens {
    a kind=lit "a"
    b kind=lit "b"
}

grammar {
    document kind=seq a b
}
"#;

fn report_for(text: &str) -> DeterminismReport {
    let grammar = load_grammar(text).expect("fixture grammar loads");
    let automaton = Automaton::build(&grammar);
    DeterminismReport::compute_with_ambiguities(&grammar, &automaton)
}

#[test]
fn unannotated_ambiguity_keeps_the_table_fallback() {
    let report = report_for(AMBIGUOUS_GIR);

    let grammar = load_grammar(AMBIGUOUS_GIR).expect("loads");
    let dispatch = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "s")
        .expect("dispatch rule")
        .id;

    let verdict = report
        .verdicts
        .iter()
        .find(|verdict| verdict.rule == dispatch)
        .expect("verdict for the dispatch rule");
    assert_eq!(
        verdict.backend,
        Backend::Table,
        "unannotated ambiguous regions keep the table fallback"
    );
    assert!(!report.all_deterministic());
}

#[test]
fn the_annotation_reclassifies_the_region_as_an_island_candidate() {
    let report = report_for(ANNOTATED_GIR);

    let grammar = load_grammar(ANNOTATED_GIR).expect("loads");
    let dispatch = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "s")
        .expect("dispatch rule")
        .id;

    let verdict = report
        .verdicts
        .iter()
        .find(|verdict| verdict.rule == dispatch)
        .expect("verdict for the dispatch rule");
    assert_eq!(
        verdict.backend,
        Backend::GlrIsland,
        "the annotated ambiguous region is a GLR island candidate (ADR-007)"
    );
    assert!(
        report.all_deterministic(),
        "an island candidate is not table fallback: the build continues"
    );
}

#[test]
fn ambiguous_regions_render_two_counter_example_derivations() {
    let report = report_for(AMBIGUOUS_GIR);

    assert!(
        !report.ambiguities.is_empty(),
        "the reduce/reduce contention surfaces as an ambiguity mark"
    );
    let ambiguity = &report.ambiguities[0];
    assert!(!ambiguity.allowed, "the fixture carries no annotation");
    assert_eq!(
        ambiguity.sides.len(),
        2,
        "the two contending derivations are rendered"
    );
    assert!(
        ambiguity.sides[0] != ambiguity.sides[1],
        "the two derivations are distinct productions"
    );
    // EOF-contention render (the shared lookahead is the pseudo-EOF).
    let rendered = format!("{report}");
    assert!(
        rendered.contains("ambiguity rule"),
        "the report carries an ambiguity section:\n{rendered}"
    );
    assert!(
        rendered.contains("interp #1:") && rendered.contains("interp #2:"),
        "both derivations render:\n{rendered}"
    );
}

#[test]
fn ambiguity_marks_are_deterministic_across_runs() {
    let first = report_for(AMBIGUOUS_GIR);
    let second = report_for(AMBIGUOUS_GIR);
    assert_eq!(format!("{first}"), format!("{second}"));
}

#[test]
fn deterministic_grammars_carry_no_ambiguity_marks() {
    let report = report_for(DETERMINISTIC_GIR);
    assert!(
        report.ambiguities.is_empty(),
        "a conflict-free automaton records nothing"
    );
    assert!(report.all_deterministic());
}

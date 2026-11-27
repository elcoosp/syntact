//! The `@allow_ambiguous` surface gate (roadmap §5.4 annotation table,
//! §6.2, ADR-007).
//!
//! Plan line pinned (§6.1 pass 4): *"genuine ambiguity → ambiguity report
//! with two derivations (§9.2), build continues with GLR island iff
//! `@allow_ambiguous`"* — the surface spelling is `allow-ambiguous="true"`
//! on a grammar rule; it loads, lowers into the IR flag, and round-trips
//! through the canonical renderer. Documents without the flag are
//! unchanged (new surface syntax loads old documents unchanged — the
//! semver-policy surface freeze).

use gir_syntax::{load, render};

/// The ambiguous fixture with the annotation on the dispatch rule.
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

#[test]
fn the_flag_loads_into_the_surface_document() {
    let doc = load(ANNOTATED_GIR).expect("loads");
    let s = doc
        .rules
        .iter()
        .find(|rule| rule.name == "s")
        .expect("dispatch rule");
    assert!(s.allow_ambiguous, "the annotation lands on the rule");
    let p = doc.rules.iter().find(|rule| rule.name == "p").expect("p");
    assert!(!p.allow_ambiguous, "absent flags default false");
}

#[test]
fn the_flag_lowers_into_the_ir() {
    let doc = load(ANNOTATED_GIR).expect("loads");
    let grammar = doc.to_grammar().expect("lowers");
    let s = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "s")
        .expect("dispatch rule");
    assert!(s.allow_ambiguous, "the IR rule carries the annotation");
}

#[test]
fn the_flag_round_trips_through_the_canonical_renderer() {
    let doc = load(ANNOTATED_GIR).expect("loads");
    let text = render(&doc);
    assert!(
        text.contains(r#"allow-ambiguous="true""#),
        "the canonical form emits the annotation:\n{text}"
    );
    let reloaded = load(&text).expect("render is valid KDL again");
    assert_eq!(
        doc.to_grammar().expect("ir"),
        reloaded.to_grammar().expect("ir"),
        "render -> load preserves the IR, annotation included"
    );
    // Render is idempotent (canonical form).
    assert_eq!(render(&reloaded), text);
}

#[test]
fn documents_without_the_flag_render_unchanged() {
    const PLAIN_GIR: &str = r#"
language "plain" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    document kind=seq a
}
"#;
    let doc = load(PLAIN_GIR).expect("loads");
    let text = render(&doc);
    assert!(
        !text.contains("allow-ambiguous"),
        "old documents stay byte-identical:\n{text}"
    );
}

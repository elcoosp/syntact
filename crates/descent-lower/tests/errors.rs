//! Lowering-error suites (issue 043 TDD): every canonical form the v0
//! compiler cannot express is a deterministic, named, testable error —
//! never a silent mis-lowering — and the bounded repetition shapes the
//! surface does express lower to their engine forms.

use descent::parse_events;
use descent_lower::{LowerError, lower};
use gir_ir::{Element, GrammarBuilder};
use gir_syntax::load_grammar;
use pretty_assertions::assert_eq;

/// The lowering error for a grammar, rendered (panics on success).
fn error_of(text: &str) -> String {
    let grammar = load_grammar(text).expect("fixture loads");
    match lower(&grammar) {
        Err(error) => error.to_string(),
        Ok(lowered) => panic!("expected a refusal, lowered anyway: {lowered:?}"),
    }
}

/// A non-node rule cycle (`loop-a → loop-b → loop-a`) cannot inline —
/// the lowering names the cycle's entry rule.
#[test]
fn transparent_cycle_is_a_named_error() {
    let text = r#"
language "cycle" version="1.0.0"

grammar {
    start kind=alt loop-a
    loop-a kind=seq loop-b "x"
    loop-b kind=seq loop-a "y"
}
"#;
    let message = error_of(text);
    assert!(
        message.contains("transparent rule cycle through `loop-a`"),
        "{message}"
    );
}

/// A pratt rule must carry `node="true"`: the descriptor table rides
/// inside the rule's node; a transparent pratt rule has nowhere to hang.
#[test]
fn transparent_pratt_rule_is_rejected() {
    let text = r#"
language "bare-expr" version="1.0.0"

tokens {
    int kind=re "[0-9]+"
    plus kind=lit "+"
}

grammar {
    expr lbp=1 kind=alt add int
    add kind=seq expr plus expr
}
"#;
    let message = error_of(text);
    assert!(
        message.contains("pratt rule `expr` needs node=\"true\""),
        "{message}"
    );
}

/// Lookahead predicates (`and`/`not` markers) have no descriptor form
/// in v0 — rejected up front, never silently dropped.
#[test]
fn lookahead_predicates_are_rejected() {
    let text = r#"
language "look" version="1.0.0"

grammar {
    start kind=seq "a" not
}
"#;
    let message = error_of(text);
    assert!(
        message.contains("rule `start`: and/not predicates"),
        "{message}"
    );
}

/// Two alternatives sharing a FIRST token is an LL(1) conflict: the
/// generated descriptor would dispatch by luck, so the compiler
/// refuses.
#[test]
fn overlapping_alternatives_are_a_conflict_error() {
    let text = r#"
language "clash" version="1.0.0"

grammar {
    start kind=alt x y
    x kind=seq "k" "a"
    y kind=seq "k" "b"
}
"#;
    let message = error_of(text);
    assert!(
        message.contains("rule `start`: alternatives share FIRST token"),
        "{message}"
    );
}

/// Bounded repetitions with an upper bound above one have no
/// descriptor form in v0 (the interpreter's `Rep` is unbounded).
#[test]
fn bounded_repetition_is_rejected() {
    let mut builder = GrammarBuilder::new("bounded");
    builder.version("1.0.0");
    let a = builder.token_lit("a", "a").expect("token declared");
    let rule = builder.declare_rule("start").expect("rule declared");
    builder.set_body(
        rule,
        Element::Rep {
            inner: Box::new(Element::Token(a)),
            min: 0,
            max: Some(2),
        },
    );
    let grammar = builder.finish().expect("valid grammar");
    match lower(&grammar) {
        Err(LowerError::BoundedRepetition { rule }) => assert_eq!(rule, "start"),
        Err(other) => panic!("expected BoundedRepetition, got {other}"),
        Ok(lowered) => panic!("expected a refusal, lowered anyway: {lowered:?}"),
    }
}

/// `rep1` lowers to `body ~ rep(body)` — first mandatory, then the
/// unbounded tail the engine's `Rep` provides.
#[test]
fn rep1_lowers_to_mandatory_plus_rep() {
    let text = r#"
language "one-plus" version="1.0.0"

grammar {
    start kind=seq "a" rep1
}
"#;
    let grammar = load_grammar(text).expect("loads");
    let lowered = lower(&grammar).expect("lowers");
    // `a a a` parses clean; `` produces no tokens and no errors (the
    // document sweep is empty), while the start pattern itself fails.
    let scanner = lexgen::compile(&grammar).expect("lexes");
    let source = "aaa";
    let tokens = lowered.feed(&scanner, source);
    let run = parse_events(&lowered.desc, &tokens, source);
    assert_eq!(run.errors, 0);
    assert_eq!(run.tokens.len(), 3, "all three `a`s consumed");
}

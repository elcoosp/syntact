//! gir-transform integration tests (issue 013 TDD): pratt lowering,
//! left-factoring fixpoint, cycle-guarded inlining, pipeline
//! determinism — plus language checks through the LR(1) shadow automaton.

use gir_ir::{Associativity, Element, Grammar, GrammarBuilder, PrattHint};
use gir_transform::{inline_single_use, left_factor, lower_pratt, optimize};
use lr_shadow::{Action, Automaton, Look};

/// Left-recursive binop grammar: expr → expr + n | n with a pratt hint.
fn binop_grammar() -> Grammar {
    let mut b = GrammarBuilder::new("binop");
    let n = b.token_re("n", "[0-9]+").expect("t");
    let plus = b.token_lit("plus", "+").expect("t");
    let expr = b.declare_rule("expr").expect("r");
    b.set_body(
        expr,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(expr),
                GrammarBuilder::token_ref(plus),
                GrammarBuilder::token_ref(n),
            ]),
            GrammarBuilder::token_ref(n),
        ]),
    );
    b.set_pratt(
        expr,
        PrattHint {
            left_bp: 10,
            right_bp: 9,
            assoc: Associativity::Left,
        },
    );
    b.finish().expect("valid")
}

/// Rule id by name.
fn rid(grammar: &Grammar, name: &str) -> gir_ir::RuleId {
    grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == name)
        .expect("fixture rule")
        .id
}

/// Token id by name.
fn tok(grammar: &Grammar, name: &str) -> gir_ir::TokenId {
    grammar
        .tokens
        .iter()
        .find(|token| grammar.interner.lookup(token.name) == name)
        .expect("fixture token")
        .id
}

/// Drive the LR(1) tables over a token stream (same interpreter shape as
/// the lr-shadow tests).
fn accepts(auto: &Automaton, stream: &[Look]) -> bool {
    let mut stack: Vec<lr_shadow::StateId> = vec![auto.start];
    let mut pos = 0;
    loop {
        let state = *stack.last().expect("stack never empty");
        let Some(action) = stream
            .get(pos)
            .copied()
            .and_then(|term| auto.action(state, term))
            .copied()
        else {
            return false;
        };
        match action {
            Action::Shift(target) => {
                stack.push(target);
                pos += 1;
            }
            Action::Reduce(prod) => {
                let production = &auto.lowered.productions[prod as usize];
                let symbols = production.symbols.len();
                stack.truncate(stack.len() - symbols);
                let top = *stack.last().expect("stack never empty");
                let target = auto
                    .goto(top, production.lhs)
                    .expect("goto exists for every reachable lhs");
                stack.push(target);
            }
            Action::Accept => return true,
        }
    }
}

// ---------------------------------------------------------------------------
// Pratt lowering
// ---------------------------------------------------------------------------

#[test]
fn pratt_removes_left_recursion_preserving_language() {
    let grammar = binop_grammar();
    let lowered = lower_pratt(&grammar);
    let expr = rid(&lowered, "expr");
    let rule = lowered.rule(expr);
    // Body is now Seq[atom, Rep0(tail)] — nothing starts with expr.
    let Element::Seq(elements) = &rule.body else {
        panic!("repetition shape expected, got {:?}", rule.body);
    };
    assert_eq!(elements.len(), 2);
    assert!(matches!(elements[0], Element::Token(_)));
    assert!(matches!(elements[1], Element::Rep { min: 0, .. }));
    // Hint rides along.
    assert!(rule.pratt.is_some());
    // Language check through the shadow automaton: n + n + n parses.
    let auto = Automaton::build(&lowered);
    let n = Look::token(tok(&lowered, "n"));
    let plus = Look::token(tok(&lowered, "plus"));
    assert!(accepts(&auto, &[n, plus, n, plus, n, Look::EOF]));
    assert!(!accepts(&auto, &[n, plus, Look::EOF]));
}

#[test]
fn pratt_multiple_tails_and_multi_atom() {
    let mut b = GrammarBuilder::new("twoops");
    let n = b.token_re("n", "[0-9]+").expect("t");
    let plus = b.token_lit("plus", "+").expect("t");
    let star = b.token_lit("star", "*").expect("t");
    let par_l = b.token_lit("lpar", "(").expect("t");
    let par_r = b.token_lit("rpar", ")").expect("t");
    let expr = b.declare_rule("expr").expect("r");
    b.set_body(
        expr,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(expr),
                GrammarBuilder::token_ref(plus),
                GrammarBuilder::token_ref(n),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(expr),
                GrammarBuilder::token_ref(star),
                GrammarBuilder::token_ref(n),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(par_l),
                GrammarBuilder::rule_ref(expr),
                GrammarBuilder::token_ref(par_r),
            ]),
            GrammarBuilder::token_ref(n),
        ]),
    );
    b.set_pratt(
        expr,
        PrattHint {
            left_bp: 10,
            right_bp: 9,
            assoc: Associativity::Left,
        },
    );
    let grammar = b.finish().expect("valid");
    let lowered = lower_pratt(&grammar);
    let auto = Automaton::build(&lowered);
    let n = Look::token(tok(&lowered, "n"));
    let plus = Look::token(tok(&lowered, "plus"));
    let star = Look::token(tok(&lowered, "star"));
    let par_l = Look::token(tok(&lowered, "lpar"));
    let par_r = Look::token(tok(&lowered, "rpar"));
    assert!(accepts(&auto, &[n, plus, n, star, n, Look::EOF]));
    assert!(accepts(
        &auto,
        &[par_l, n, plus, n, par_r, star, n, Look::EOF]
    ));
    assert!(!accepts(&auto, &[plus, n, Look::EOF]));
}

#[test]
fn pratt_rule_without_left_recursion_unchanged() {
    let mut b = GrammarBuilder::new("postfix");
    let n = b.token_re("n", "[0-9]+").expect("t");
    let bang = b.token_lit("bang", "!").expect("t");
    let expr = b.declare_rule("expr").expect("r");
    b.set_body(
        expr,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(n),
                GrammarBuilder::rep0(GrammarBuilder::token_ref(bang)),
            ]),
            GrammarBuilder::token_ref(n),
        ]),
    );
    b.set_pratt(
        expr,
        PrattHint {
            left_bp: 5,
            right_bp: 4,
            assoc: Associativity::Left,
        },
    );
    let grammar = b.finish().expect("valid");
    let lowered = lower_pratt(&grammar);
    assert_eq!(lowered, grammar, "no left recursion → no rewrite");
}

// ---------------------------------------------------------------------------
// Left factoring
// ---------------------------------------------------------------------------

#[test]
fn common_prefix_is_hoisted() {
    let mut b = GrammarBuilder::new("factor");
    let a = b.token_lit("a", "a").expect("t");
    let bb = b.token_lit("b", "b").expect("t");
    let c = b.token_lit("c", "c").expect("t");
    let s = b.declare_rule("s").expect("r");
    b.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(a),
                GrammarBuilder::token_ref(bb),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(a),
                GrammarBuilder::token_ref(c),
            ]),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let factored = left_factor(&grammar);
    // s now has one branch: a s_f0; the helper holds b | c.
    let s = rid(&factored, "s");
    let Element::Seq(elements) = &factored.rule(s).body else {
        panic!("factored shape");
    };
    assert!(matches!(elements[0], Element::Token(_)));
    let Element::Rule(helper) = elements[1] else {
        panic!("helper reference expected");
    };
    let Element::Alt(rem) = &factored.rule(helper).body else {
        panic!("helper alternatives expected");
    };
    assert_eq!(rem.len(), 2);
}

#[test]
fn epsilon_remainder_is_explicit() {
    // s → a b | a  →  s → a s_f0 ; s_f0 → b | ε
    let mut b = GrammarBuilder::new("epsrem");
    let a = b.token_lit("a", "a").expect("t");
    let bb = b.token_lit("b", "b").expect("t");
    let s = b.declare_rule("s").expect("r");
    b.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(a),
                GrammarBuilder::token_ref(bb),
            ]),
            GrammarBuilder::token_ref(a),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let factored = left_factor(&grammar);
    let s = rid(&factored, "s");
    let Element::Seq(elements) = &factored.rule(s).body else {
        panic!("factored shape");
    };
    let Element::Rule(helper) = elements[1] else {
        panic!("helper reference expected");
    };
    let body = &factored.rule(helper).body;
    let has_epsilon = matches!(body, Element::Alt(rem) if rem.iter().any(|r| matches!(r, Element::Seq(v) if v.is_empty())));
    assert!(has_epsilon, "ε remainder must be explicit: {body:?}");
}

#[test]
fn factoring_fixpoint_terminates_and_is_idempotent() {
    // Nested + chained prefixes: s → a b x | a b y | a c.
    let mut builder = GrammarBuilder::new("chain");
    let tok_a = builder.token_lit("a", "a").expect("t");
    let tok_b = builder.token_lit("b", "b").expect("t");
    let tok_c = builder.token_lit("c", "c").expect("t");
    let tok_x = builder.token_lit("x", "x").expect("t");
    let tok_y = builder.token_lit("y", "y").expect("t");
    let s = builder.declare_rule("s").expect("r");
    builder.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(tok_a),
                GrammarBuilder::token_ref(tok_b),
                GrammarBuilder::token_ref(tok_x),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(tok_a),
                GrammarBuilder::token_ref(tok_b),
                GrammarBuilder::token_ref(tok_y),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(tok_a),
                GrammarBuilder::token_ref(tok_c),
            ]),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let once = left_factor(&grammar);
    let twice = left_factor(&once);
    assert_eq!(once, twice, "fixpoint reached: re-running is identity");
    assert!(grammar.validate().is_ok());
    assert!(once.validate().is_ok());
}

#[test]
fn no_common_prefix_unchanged() {
    let mut b = GrammarBuilder::new("noprefix");
    let a = b.token_lit("a", "a").expect("t");
    let bb = b.token_lit("b", "b").expect("t");
    let s = b.declare_rule("s").expect("r");
    b.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::token_ref(a),
            GrammarBuilder::token_ref(bb),
        ]),
    );
    let grammar = b.finish().expect("valid");
    assert_eq!(left_factor(&grammar), grammar);
}

// ---------------------------------------------------------------------------
// Cycle-guarded inlining
// ---------------------------------------------------------------------------

#[test]
fn transparent_single_use_rule_is_inlined() {
    let mut b = GrammarBuilder::new("inline");
    let a = b.token_lit("a", "a").expect("t");
    let s = b.declare_rule("s").expect("r");
    let helper = b.declare_rule("helper").expect("r");
    b.set_body(helper, GrammarBuilder::token_ref(a));
    b.set_body(
        s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(helper),
            GrammarBuilder::token_ref(a),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let inlined = inline_single_use(&grammar);
    let s_id = rid(&inlined, "s");
    // s's body folded the helper: two token references, no rule refs.
    assert_eq!(inlined.rule(s_id).body.rule_refs().len(), 0);
    // The helper is now unreachable.
    let unreachable = inlined.unreachable_rules();
    assert!(unreachable.contains(&rid(&inlined, "helper")));
}

#[test]
fn cycle_guard_blocks_recursive_candidate() {
    // s → h a ; h → b | h c  (h references itself → must not inline).
    let mut builder = GrammarBuilder::new("guarded");
    let tok_a = builder.token_lit("a", "a").expect("t");
    let tok_b = builder.token_lit("b", "b").expect("t");
    let tok_c = builder.token_lit("c", "c").expect("t");
    let s = builder.declare_rule("s").expect("r");
    let h = builder.declare_rule("h").expect("r");
    builder.set_body(
        h,
        GrammarBuilder::alt(vec![
            GrammarBuilder::token_ref(tok_b),
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(h),
                GrammarBuilder::token_ref(tok_c),
            ]),
        ]),
    );
    builder.set_body(
        s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(h),
            GrammarBuilder::token_ref(tok_a),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let inlined = inline_single_use(&grammar);
    // h still referenced once from s — untouched.
    let s_id = rid(&inlined, "s");
    assert_eq!(inlined.rule(s_id).body.rule_refs().len(), 1);
}

#[test]
fn cycle_guard_blocks_mutual_recursion() {
    // s → h a ; h → s? c — inlining h into s would give s → (s? c) a,
    // a new self-reference the grammar did not have at that site.
    let mut builder = GrammarBuilder::new("mutual");
    let tok_a = builder.token_lit("a", "a").expect("t");
    let tok_c = builder.token_lit("c", "c").expect("t");
    let s = builder.declare_rule("s").expect("r");
    let h = builder.declare_rule("h").expect("r");
    builder.set_body(
        h,
        GrammarBuilder::seq(vec![
            GrammarBuilder::opt(GrammarBuilder::rule_ref(s)),
            GrammarBuilder::token_ref(tok_c),
        ]),
    );
    builder.set_body(
        s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(h),
            GrammarBuilder::token_ref(tok_a),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let inlined = inline_single_use(&grammar);
    let s_id = rid(&inlined, "s");
    assert_eq!(inlined.rule(s_id).body.rule_refs().len(), 1, "guarded");
}

#[test]
fn annotated_rules_are_never_inlined() {
    // node-annotated single-use rule stays a rule (it emits a node).
    let mut b = GrammarBuilder::new("annotated");
    let a = b.token_lit("a", "a").expect("t");
    let s = b.declare_rule("s").expect("r");
    let node_rule = b.declare_rule("nodery").expect("r");
    b.set_body(node_rule, GrammarBuilder::token_ref(a));
    b.set_node_rule(node_rule, true);
    b.set_body(
        s,
        GrammarBuilder::seq(vec![GrammarBuilder::rule_ref(node_rule)]),
    );
    let grammar = b.finish().expect("valid");
    let inlined = inline_single_use(&grammar);
    let s_id = rid(&inlined, "s");
    assert_eq!(inlined.rule(s_id).body.rule_refs().len(), 1);
}

#[test]
fn multi_use_rule_is_not_inlined() {
    // s → h a | h  — h used twice → not inlined.
    let mut b = GrammarBuilder::new("multiuse");
    let a = b.token_lit("a", "a").expect("t");
    let s = b.declare_rule("s").expect("r");
    let h = b.declare_rule("h").expect("r");
    b.set_body(h, GrammarBuilder::token_ref(a));
    b.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(h),
                GrammarBuilder::token_ref(a),
            ]),
            GrammarBuilder::rule_ref(h),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let inlined = inline_single_use(&grammar);
    // h is still referenced from s (both uses) — not unreachable.
    assert!(
        !inlined.unreachable_rules().contains(&rid(&inlined, "h")),
        "multi-use rule must not be folded"
    );
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

#[test]
fn optimize_pipeline_is_deterministic_and_valid() {
    let grammar = binop_grammar();
    let a = optimize(&grammar);
    let b = optimize(&grammar);
    assert_eq!(a, b, "pipeline is a pure function");
    assert!(a.validate().is_ok());
    // The optimized binop grammar still parses sums through the shadow.
    let auto = Automaton::build(&a);
    let n = Look::token(tok(&a, "n"));
    let plus = Look::token(tok(&a, "plus"));
    assert!(accepts(&auto, &[n, plus, n, plus, n, Look::EOF]));
}

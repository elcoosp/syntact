//! gir-analysis integration tests (issues 010 + 012 TDD): hand-computed
//! FIRST/FOLLOW fixtures, LL(1) verdicts, fixpoint termination, report
//! snapshots, left-recursion detection.

use gir_analysis::{Backend, DeterminismReport, FirstSets, FollowSets};
use gir_ir::{Grammar, GrammarBuilder, RuleId, TokenId};

/// JSON-shaped LL(1) fixture (same shape as the gir-ir tests).
fn json_grammar() -> Grammar {
    let mut b = GrammarBuilder::new("json");
    let lbrace = b.token_lit("lbrace", "{").expect("t");
    let rbrace = b.token_lit("rbrace", "}").expect("t");
    let left_bracket = b.token_lit("lbrack", "[").expect("t");
    let right_bracket = b.token_lit("rbrack", "]").expect("t");
    let colon = b.token_lit("colon", ":").expect("t");
    let comma = b.token_lit("comma", ",").expect("t");
    let string = b.token_re("string", "\"[^\"]*\"").expect("t");
    let number = b.token_re("number", "[0-9]+").expect("t");

    let value = b.declare_rule("value").expect("r");
    let object = b.declare_rule("object").expect("r");
    let array = b.declare_rule("array").expect("r");
    let pair = b.declare_rule("pair").expect("r");
    b.set_body(
        value,
        GrammarBuilder::alt(vec![
            GrammarBuilder::rule_ref(object),
            GrammarBuilder::rule_ref(array),
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(number),
        ]),
    );
    b.set_body(
        object,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(lbrace),
            GrammarBuilder::rule_ref(pair),
            GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(comma),
                GrammarBuilder::rule_ref(pair),
            ])),
            GrammarBuilder::token_ref(rbrace),
        ]),
    );
    b.set_body(
        pair,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(colon),
            GrammarBuilder::rule_ref(value),
        ]),
    );
    b.set_body(
        array,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(left_bracket),
            GrammarBuilder::opt(GrammarBuilder::rule_ref(value)),
            GrammarBuilder::token_ref(right_bracket),
        ]),
    );
    b.finish().expect("valid")
}

/// Token id by name in a fixture grammar.
fn tok(grammar: &Grammar, name: &str) -> TokenId {
    grammar
        .tokens
        .iter()
        .find(|token| grammar.interner.lookup(token.name) == name)
        .expect("fixture token")
        .id
}

#[test]
fn first_sets_match_hand_computed_json() {
    let grammar = json_grammar();
    let first = FirstSets::compute(&grammar);
    let value = first.first(RuleId(0));
    // FIRST(value) = { object→lbrace, array→lbrack, string, number }
    assert_eq!(
        value.tokens,
        [
            tok(&grammar, "lbrace"),
            tok(&grammar, "lbrack"),
            tok(&grammar, "string"),
            tok(&grammar, "number"),
        ]
        .into_iter()
        .collect()
    );
    assert!(!value.nullable);
    // object = "{" pair ("," pair)* "}": starts with lbrace only.
    let object = first.first(RuleId(1));
    assert_eq!(
        object.tokens,
        [tok(&grammar, "lbrace")].into_iter().collect()
    );
}

#[test]
fn follow_sets_match_hand_computed_json() {
    let grammar = json_grammar();
    let first = FirstSets::compute(&grammar);
    let follow = FollowSets::compute(&grammar, &first);
    // FOLLOW(pair) = { comma, rbrace } (pair is followed by "," or "}"
    // through object's repetition).
    let pair = follow.follow(RuleId(3));
    assert_eq!(
        pair.tokens,
        [tok(&grammar, "comma"), tok(&grammar, "rbrace")]
            .into_iter()
            .collect()
    );
    // FOLLOW(value) = { comma, rbrace, rbrack, EOF }.
    let value = follow.follow(RuleId(0));
    assert!(value.eof, "value is the start rule: EOF in FOLLOW");
    assert_eq!(
        value.tokens,
        [
            tok(&grammar, "comma"),
            tok(&grammar, "rbrace"),
            tok(&grammar, "rbrack"),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn json_grammar_is_ll1_and_deterministic() {
    let grammar = json_grammar();
    let report = DeterminismReport::compute(&grammar);
    assert!(
        report.all_deterministic(),
        "json fixture classifies fully deterministic:\n{report}"
    );
    assert_eq!(report.conflicts, [] as [gir_analysis::ConflictSite; 0]);
    for verdict in &report.verdicts {
        assert_eq!(verdict.backend, Backend::Descent, "{verdict:?}");
        assert!(!verdict.left_recursive);
    }
}

#[test]
fn report_snapshot_is_stable_text() {
    let grammar = json_grammar();
    let report = DeterminismReport::compute(&grammar);
    let text = report.to_string();
    // 4 rules, no conflicts; exact snapshot (issue 012).
    assert_eq!(
        text,
        "rule 0 backend=descent ll1=true left_recursive=false reachable=true\n\
         rule 1 backend=descent ll1=true left_recursive=false reachable=true\n\
         rule 2 backend=descent ll1=true left_recursive=false reachable=true\n\
         rule 3 backend=descent ll1=true left_recursive=false reachable=true\n"
    );
    // Determinism of the report itself.
    assert_eq!(DeterminismReport::compute(&grammar).to_string(), text);
}

#[test]
fn common_prefix_branches_conflict() {
    // s = "a" "b" | "a" "c" → conflict on "a".
    let mut b = GrammarBuilder::new("prefix");
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
    let report = DeterminismReport::compute(&grammar);
    assert!(!report.all_deterministic());
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].token, a);
    assert_eq!(report.conflicts[0].rule, s);
    assert_eq!(report.backend_for(s), Backend::Table);
}

#[test]
fn pratt_hint_overrides_conflicts() {
    // Same conflicting grammar but flagged pratt: expression regions are
    // expected to be non-LL(1); the backend stays pratt.
    let mut b = GrammarBuilder::new("pratt");
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
    b.set_pratt(
        s,
        gir_ir::PrattHint {
            left_bp: 10,
            right_bp: 9,
            assoc: gir_ir::Associativity::Left,
        },
    );
    let grammar = b.finish().expect("valid");
    let report = DeterminismReport::compute(&grammar);
    assert_eq!(report.backend_for(s), Backend::Pratt);
    assert!(!report.conflicts.is_empty(), "conflicts are still reported");
}

#[test]
fn nullable_branch_follow_conflict() {
    // s = x | y where x = "a"?, y = "a"? — both alternatives can be
    // entered on "a" and on FOLLOW(s) → conflicts.
    let mut builder = GrammarBuilder::new("nullable");
    let a = builder.token_lit("a", "a").expect("t");
    let x = builder.declare_rule("x").expect("r");
    let y = builder.declare_rule("y").expect("r");
    let s = builder.declare_rule("s").expect("r");
    builder.set_body(x, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
    builder.set_body(y, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
    builder.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::rule_ref(x),
            GrammarBuilder::rule_ref(y),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let report = DeterminismReport::compute(&grammar);
    assert_eq!(report.backend_for(s), Backend::Table);
    assert_ne!(report.conflicts, [] as [gir_analysis::ConflictSite; 0]);
}

#[test]
fn negative_lookahead_is_opaque() {
    let mut b = GrammarBuilder::new("opaque");
    let a = b.token_lit("a", "a").expect("t");
    let bb = b.token_lit("b", "b").expect("t");
    let s = b.declare_rule("s").expect("r");
    b.set_body(
        s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::not(GrammarBuilder::token_ref(bb)),
            GrammarBuilder::token_ref(a),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let report = DeterminismReport::compute(&grammar);
    assert_eq!(report.backend_for(s), Backend::Table);
}

#[test]
fn direct_left_recursion_detected() {
    let mut b = GrammarBuilder::new("leftrec");
    let n = b.token_lit("n", "n").expect("t");
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
    let grammar = b.finish().expect("valid");
    let report = DeterminismReport::compute(&grammar);
    assert!(report.verdicts[expr.0 as usize].left_recursive);
}

#[test]
fn indirect_left_recursion_detected_through_nullable_prefix() {
    // a = b "x" | "z";  b = a? "y"  → a is indirectly left-recursive
    // through b's nullable prefix.
    let mut builder = GrammarBuilder::new("indirect");
    let x = builder.token_lit("x", "x").expect("t");
    let y = builder.token_lit("y", "y").expect("t");
    let z = builder.token_lit("z", "z").expect("t");
    let a = builder.declare_rule("a").expect("r");
    let rule_b = builder.declare_rule("b").expect("r");
    builder.set_body(
        a,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(rule_b),
                GrammarBuilder::token_ref(x),
            ]),
            GrammarBuilder::token_ref(z),
        ]),
    );
    builder.set_body(
        rule_b,
        GrammarBuilder::seq(vec![
            GrammarBuilder::opt(GrammarBuilder::rule_ref(a)),
            GrammarBuilder::token_ref(y),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let flags = gir_analysis::left_recursion(&grammar);
    assert!(
        flags[a.0 as usize],
        "indirect left recursion via nullable prefix"
    );
    assert!(flags[rule_b.0 as usize], "b begins with a through the opt");
}

#[test]
fn fixpoint_terminates_on_recursive_grammar() {
    // A grammar with cycles in every direction: the fixpoint must
    // terminate and stay stable (two runs agree).
    let mut b = GrammarBuilder::new("cycles");
    let t = b.token_lit("t", "t").expect("t");
    let a = b.declare_rule("a").expect("r");
    let rule_b = b.declare_rule("b").expect("r");
    let c = b.declare_rule("c").expect("r");
    b.set_body(
        a,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(rule_b),
                GrammarBuilder::token_ref(t),
            ]),
            GrammarBuilder::opt(GrammarBuilder::rule_ref(c)),
        ]),
    );
    b.set_body(
        rule_b,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(a),
            GrammarBuilder::opt(GrammarBuilder::rule_ref(rule_b)),
        ]),
    );
    b.set_body(
        c,
        GrammarBuilder::rep0(GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(t),
            GrammarBuilder::rule_ref(a),
        ])),
    );
    let grammar = b.finish().expect("valid");
    let first_one = FirstSets::compute(&grammar);
    let first_two = FirstSets::compute(&grammar);
    assert_eq!(format!("{first_one:?}"), format!("{first_two:?}"));
    // FIRST(a) grew to include t (through b → a cycles).
    assert!(first_one.first(a).tokens.contains(&t));
    let follow = FollowSets::compute(&grammar, &first_one);
    assert!(follow.follow(a).eof, "a is start; EOF flows around cycles");
}

#[test]
fn unreachable_rules_flag_reachable_false() {
    let mut b = GrammarBuilder::new("unreach");
    let t = b.token_lit("t", "t").expect("t");
    let s = b.declare_rule("s").expect("r");
    let orphan = b.declare_rule("orphan").expect("r");
    b.set_body(s, GrammarBuilder::token_ref(t));
    b.set_body(
        orphan,
        GrammarBuilder::alt(vec![
            GrammarBuilder::token_ref(t),
            GrammarBuilder::token_ref(t),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let report = DeterminismReport::compute(&grammar);
    assert!(report.verdicts[s.0 as usize].reachable);
    assert!(!report.verdicts[orphan.0 as usize].reachable);
}

#[test]
fn positive_lookahead_contributes_no_consumption() {
    // s = &"a" "a": FIRST(s) = {a}, and the lookahead itself is nullable.
    let mut b = GrammarBuilder::new("pred");
    let a = b.token_lit("a", "a").expect("t");
    let s = b.declare_rule("s").expect("r");
    b.set_body(
        s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::and(GrammarBuilder::token_ref(a)),
            GrammarBuilder::token_ref(a),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let first = FirstSets::compute(&grammar);
    let set = first.first(s);
    assert!(set.tokens.contains(&a));
    // The Seq continues past the (non-consuming) predicate to "a".
    assert_eq!(set.tokens.len(), 1);
}

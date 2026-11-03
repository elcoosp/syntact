//! gir-ir integration tests: IR construction and validation (issue 005 TDD).

use gir_ir::{
    Associativity, Element, Grammar, GrammarBuilder, IrError, PrattHint, RuleId, TokenId,
};

/// Build a JSON-shaped grammar (the canonical fixture): forward-declared
/// `value` referenced before its body exists.
fn json_grammar() -> Grammar {
    let mut b = GrammarBuilder::new("json");
    b.version("1.0.0").extension("json");
    let open_brace = b.token_lit("lbrace", "{").expect("fresh token");
    let close_brace = b.token_lit("rbrace", "}").expect("fresh token");
    let open_bracket = b.token_lit("lbrack", "[").expect("fresh token");
    let close_bracket = b.token_lit("rbrack", "]").expect("fresh token");
    let colon = b.token_lit("colon", ":").expect("fresh token");
    let comma = b.token_lit("comma", ",").expect("fresh token");
    let string = b.token_re("string", "\"[^\"]*\"").expect("fresh token");
    let number = b.token_re("number", "[0-9]+").expect("fresh token");
    b.set_node(string, true);
    b.set_node(number, true);

    let value = b.declare_rule("value").expect("fresh rule");
    let object = b.declare_rule("object").expect("fresh rule");
    let array = b.declare_rule("array").expect("fresh rule");
    let pair = b.declare_rule("pair").expect("fresh rule");

    // value = | object | array | string | number
    b.set_body(
        value,
        GrammarBuilder::alt(vec![
            GrammarBuilder::rule_ref(object),
            GrammarBuilder::rule_ref(array),
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(number),
        ]),
    );
    // object = "{" pair ("," pair)* "}"
    b.set_body(
        object,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(open_brace),
            GrammarBuilder::rule_ref(pair),
            GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(comma),
                GrammarBuilder::rule_ref(pair),
            ])),
            GrammarBuilder::token_ref(close_brace),
        ]),
    );
    // pair = string ":" value
    b.set_body(
        pair,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(colon),
            GrammarBuilder::rule_ref(value),
        ]),
    );
    // array = "[" (value ("," value)*)? "]"
    b.set_body(
        array,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(open_bracket),
            GrammarBuilder::opt(GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(value),
                GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                    GrammarBuilder::token_ref(comma),
                    GrammarBuilder::rule_ref(value),
                ])),
            ])),
            GrammarBuilder::token_ref(close_bracket),
        ]),
    );
    b.finish().expect("json grammar is valid")
}

#[test]
fn ids_are_dense_and_declaration_ordered() {
    let grammar = json_grammar();
    let expected_tokens: Vec<u16> = (0..grammar.tokens.len())
        .map(|i| u16::try_from(i).expect("fits"))
        .collect();
    assert_eq!(
        grammar.tokens.iter().map(|t| t.id.0).collect::<Vec<_>>(),
        expected_tokens,
        "token ids dense"
    );
    let expected_rules: Vec<u32> = (0..grammar.rules.len())
        .map(|i| u32::try_from(i).expect("fits"))
        .collect();
    assert_eq!(
        grammar.rules.iter().map(|r| r.id.0).collect::<Vec<_>>(),
        expected_rules,
        "rule ids dense"
    );
    // Declaration order: value(0), object(1), array(2), pair(3).
    assert_eq!(grammar.rule_name(RuleId(0)), "value");
    assert_eq!(grammar.rule_name(RuleId(1)), "object");
    assert_eq!(grammar.start, RuleId(0), "first rule defaults to start");
}

#[test]
fn forward_references_resolve_by_lookup() {
    let mut b = GrammarBuilder::new("fwd");
    let t = b.token_lit("t", "t").expect("fresh token");
    let a = b.declare_rule("a").expect("fresh");
    let target = b.declare_rule("target").expect("fresh");
    // `a` references `target` before target has a body.
    b.set_body(a, GrammarBuilder::rule_ref(target));
    b.set_body(target, GrammarBuilder::token_ref(t));
    let grammar = b.finish().expect("valid");
    assert_eq!(
        grammar.rule(a).body,
        Element::Rule(target),
        "the id captured at declaration survives to the body"
    );
}

#[test]
fn duplicate_rule_names_are_rejected() {
    let mut b = GrammarBuilder::new("dup");
    b.declare_rule("x").expect("first");
    let err = b.declare_rule("x").expect_err("duplicate rejected");
    assert_eq!(
        err,
        IrError::DuplicateRule {
            name: "x".to_owned()
        }
    );
    assert!(err.to_string().contains("duplicate rule"), "{err}");
}

#[test]
fn duplicate_token_names_are_rejected() {
    let mut b = GrammarBuilder::new("dup");
    b.token_lit("a", "a").expect("first");
    let err = b.token_lit("a", "b").expect_err("duplicate rejected");
    assert_eq!(
        err,
        IrError::DuplicateToken {
            name: "a".to_owned()
        }
    );
}

#[test]
fn finish_flags_out_of_bounds_rule_reference() {
    let mut b = GrammarBuilder::new("bad");
    let a = b.declare_rule("a").expect("fresh");
    b.set_body(a, Element::Rule(RuleId(9)));
    assert_eq!(
        b.finish(),
        Err(IrError::RuleOutOfBounds {
            rule: a,
            target: RuleId(9)
        })
    );
}

#[test]
fn validate_catches_stale_ids_in_hand_built_grammar() {
    let mut b = GrammarBuilder::new("bad2");
    let a = b.declare_rule("a").expect("fresh");
    b.set_body(a, Element::Rule(RuleId(0)));
    let mut grammar = b.finish().expect("self-reference is valid");
    grammar.rules.push(grammar.rules[0].clone());
    grammar.rules[1].id = RuleId(1);
    grammar.rules[1].body = Element::Rule(RuleId(42));
    let err = grammar.validate().expect_err("stale id caught");
    assert_eq!(
        err,
        IrError::RuleOutOfBounds {
            rule: RuleId(1),
            target: RuleId(42)
        }
    );
}

#[test]
fn validate_flags_out_of_bounds_token_and_start() {
    let mut b = GrammarBuilder::new("badtok");
    let a = b.declare_rule("a").expect("fresh");
    b.set_body(a, Element::Token(TokenId(7)));
    assert!(b.finish().is_err(), "token out of bounds");

    let mut b = GrammarBuilder::new("badstart");
    let a = b.declare_rule("a").expect("fresh");
    b.set_body(a, Element::Seq(vec![]));
    let mut grammar = b.finish().expect("valid");
    grammar.start = RuleId(9);
    assert_eq!(
        grammar.validate(),
        Err(IrError::StartOutOfBounds {
            start: RuleId(9),
            rules: 1
        })
    );
}

#[test]
fn validate_flags_empty_alternatives() {
    let mut b = GrammarBuilder::new("empty");
    let a = b.declare_rule("a").expect("fresh");
    // No body set: the placeholder is Alt(vec![]).
    assert_eq!(b.finish(), Err(IrError::EmptyAlternatives { rule: a }));
}

#[test]
fn validate_flags_empty_grammar() {
    let b = GrammarBuilder::new("void");
    assert_eq!(b.finish(), Err(IrError::EmptyGrammar));
}

#[test]
fn validate_flags_repetition_max_below_min() {
    let mut b = GrammarBuilder::new("rep");
    let a = b.declare_rule("a").expect("fresh");
    let t = b.token_lit("t", "t").expect("fresh");
    b.set_body(
        a,
        GrammarBuilder::rep(GrammarBuilder::token_ref(t), 3, Some(2)),
    );
    assert!(matches!(
        b.finish(),
        Err(IrError::RepetitionBounds { min: 3, max: 2, .. })
    ));
}

#[test]
fn unreachable_rules_detected_from_start() {
    let mut b = GrammarBuilder::new("reach");
    let start = b.declare_rule("start").expect("fresh");
    let used = b.declare_rule("used").expect("fresh");
    let orphan = b.declare_rule("orphan").expect("fresh");
    let t = b.token_lit("t", "t").expect("fresh");
    b.set_body(
        start,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(used),
            GrammarBuilder::token_ref(t),
        ]),
    );
    b.set_body(used, GrammarBuilder::token_ref(t));
    b.set_body(orphan, GrammarBuilder::token_ref(t));
    let grammar = b.finish().expect("valid");
    assert_eq!(grammar.unreachable_rules(), vec![orphan]);
}

#[test]
fn rule_refs_and_token_refs_are_collected() {
    let grammar = json_grammar();
    let object = grammar
        .rules
        .iter()
        .find(|r| grammar.interner.lookup(r.name) == "object")
        .expect("object rule");
    let refs = object.body.rule_refs();
    assert_eq!(refs.len(), 1, "object references only `pair`");
    let tokens = object.body.token_refs();
    assert_eq!(tokens.len(), 3, "lbrace, comma, rbrace");
}

#[test]
fn pratt_hints_and_annotations_round_trip() {
    let mut b = GrammarBuilder::new("pratt");
    let expr = b.declare_rule("expr").expect("fresh");
    let plus = b.token_lit("plus", "+").expect("fresh");
    b.set_body(
        expr,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(plus),
            GrammarBuilder::and(GrammarBuilder::token_ref(plus)),
            GrammarBuilder::not(GrammarBuilder::token_ref(plus)),
            GrammarBuilder::opt(GrammarBuilder::token_ref(plus)),
            GrammarBuilder::rep1(GrammarBuilder::token_ref(plus)),
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
    b.set_node_rule(expr, true);
    b.set_reuse(expr, true);
    b.set_list(expr, true);
    b.set_wrap_error(expr, true);
    b.add_sync(expr, &[plus]);
    b.set_insert(expr, vec![plus]);
    let grammar = b.finish().expect("valid");
    let rule = grammar.rule(expr);
    assert_eq!(
        rule.pratt,
        Some(PrattHint {
            left_bp: 10,
            right_bp: 9,
            assoc: Associativity::Left
        })
    );
    assert!(rule.node && rule.reuse && rule.list && rule.wrap_error);
    assert_eq!(rule.sync, vec![plus]);
    assert_eq!(rule.insert, vec![plus]);
}

#[test]
fn element_predicates_do_not_leak_into_refs() {
    // And/Not/Opt wrap inner elements: refs must still be found.
    let mut b = GrammarBuilder::new("pred");
    let a = b.declare_rule("a").expect("fresh");
    let b_rule = b.declare_rule("b").expect("fresh");
    let t = b.token_lit("t", "t").expect("fresh");
    b.set_body(
        a,
        GrammarBuilder::seq(vec![
            GrammarBuilder::not(GrammarBuilder::rule_ref(b_rule)),
            GrammarBuilder::and(GrammarBuilder::token_ref(t)),
        ]),
    );
    b.set_body(b_rule, GrammarBuilder::token_ref(t));
    let grammar = b.finish().expect("valid");
    assert_eq!(
        grammar.rule(a).body.rule_refs(),
        [b_rule].into_iter().collect(),
        "negative-lookahead rule refs are visible"
    );
    assert_eq!(grammar.rule(a).body.token_refs(), [t].into_iter().collect());
}

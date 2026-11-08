//! gir-syntax integration tests (issue 004 TDD): round-trip render,
//! span fidelity, error reporting with spans, separated sugar lowering.

use gir_ir::{Element, RuleId};
use gir_syntax::{
    ByteSpan, GirDoc, ItemSurf, Marker, RuleKindSurf, SurfaceError, TokenKindSurf, load,
    load_grammar, render,
};

/// The canonical JSON surface fixture (valid KDL, full feature use).
const JSON: &str = r#"
language "json" version="1.0.0" extension="json"

tokens {
    number  kind=re "[0-9-]+(\\.[0-9]+)?([eE][+-]?[0-9]+)?" node="true"
    string  kind=re "\"[^\"\\\\]*\"" node="true" index-bound="\""
    lbrace  kind=lit "{"
    rbrace  kind=lit "}"
    colon   kind=lit ":"
    comma   kind=lit ","
    ws      kind=re "[ \\t\\r\\n]+" trivia="true"
}

grammar {
    value     kind=alt object array string number
    object    kind=seq "{" pair-list opt "}" node="true" reuse="true"
    pair-list kind=sep element=pair separator="," allow-trailing="true"
    pair      kind=seq string ":" value
    array     kind=seq "[" value-list opt "]"
    value-list kind=sep element=value separator=","
}
"#;

#[test]
fn loads_language_meta() {
    let doc = load(JSON).expect("canonical fixture loads");
    assert_eq!(doc.language.name, "json");
    assert_eq!(doc.language.version, "1.0.0");
    assert_eq!(doc.language.extensions, vec!["json".to_owned()]);
    assert!(!doc.language.unicode);
    assert_eq!(doc.tokens.len(), 7);
    assert_eq!(doc.rules.len(), 6);
}

#[test]
fn token_decl_carries_flags_and_pattern() {
    let doc = load(JSON).expect("loads");
    let number = &doc.tokens[0];
    assert_eq!(number.name, "number");
    assert_eq!(
        number.kind,
        TokenKindSurf::Re {
            regex: "[0-9-]+(\\.[0-9]+)?([eE][+-]?[0-9]+)?".to_owned()
        }
    );
    assert!(number.node);
    assert!(!number.trivia);
    let ws = doc.tokens.last().expect("ws token");
    assert!(ws.trivia);
    let string = &doc.tokens[1];
    assert_eq!(string.index_bound.as_deref(), Some("\""));
}

#[test]
fn seq_markers_bind_backward() {
    let doc = load(JSON).expect("loads");
    let object = doc
        .rules
        .iter()
        .find(|rule| rule.name == "object")
        .expect("object rule");
    let RuleKindSurf::Seq { items } = &object.kind else {
        panic!("object is a seq");
    };
    // Markers attach to the previous item: {, pair-list+opt, }.
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0].item,
        ItemSurf::Lit {
            text: "{".to_owned()
        }
    );
    assert_eq!(
        items[1].item,
        ItemSurf::Ref {
            name: "pair-list".to_owned()
        }
    );
    assert_eq!(items[1].marker, Some(Marker::Opt));
    assert_eq!(
        items[2].item,
        ItemSurf::Lit {
            text: "}".to_owned()
        }
    );
    assert_eq!(items[2].marker, None, "the marker binds backward only once");
}

#[test]
fn seq_markers_lower_to_the_marked_element() {
    // `X opt` must wrap X in Opt — the marker binds backward to the
    // element it follows, never to whatever preceded X, and X itself
    // must not be dropped (regression: the lowering used to pop the
    // previous output element and discard the marked one).
    let grammar = load_grammar(JSON).expect("ir");
    let rule_named = |name: &str| {
        grammar
            .rules
            .iter()
            .find(|rule| grammar.interner.lookup(rule.name) == name)
            .map_or_else(|| panic!("rule `{name}` exists"), |rule| rule.id)
    };
    let token_named = |name: &str| {
        grammar
            .tokens
            .iter()
            .find(|token| grammar.interner.lookup(token.name) == name)
            .map_or_else(|| panic!("token `{name}` exists"), |token| token.id)
    };
    let token_named_lit = |text: &str| {
        grammar
            .tokens
            .iter()
            .find(|token| {
                matches!(&token.pattern, gir_ir::TokenPattern::Lit { text: lit } if lit == text)
            })
            .map_or_else(|| panic!("literal token `{text}` exists"), |token| token.id)
    };

    // object = "{" pair-list opt "}"
    let object = grammar.rule(rule_named("object"));
    let Element::Seq(parts) = &object.body else {
        panic!("object is a seq");
    };
    assert_eq!(
        parts.len(),
        3,
        "three elements: {{, pair-list?, }} — nothing dropped"
    );
    assert_eq!(parts[0], Element::Token(token_named("lbrace")));
    let Element::Opt(inner) = &parts[1] else {
        panic!("`pair-list opt` wraps pair-list, got {:?}", parts[1]);
    };
    assert_eq!(**inner, Element::Rule(rule_named("pair-list")));
    assert_eq!(parts[2], Element::Token(token_named("rbrace")));

    // array = "[" value-list opt "]" — `opt` binds backward to
    // value-list (the element it follows): "[" value-list? "]".
    let array = grammar.rule(rule_named("array"));
    let Element::Seq(parts) = &array.body else {
        panic!("array is a seq");
    };
    assert_eq!(parts.len(), 3, "[, value-list?, ] — nothing dropped");
    assert!(matches!(&parts[0], Element::Token(_)), "anonymous `[`");
    let Element::Opt(inner) = &parts[1] else {
        panic!("`value-list opt` wraps value-list, got {:?}", parts[1]);
    };
    assert_eq!(**inner, Element::Rule(rule_named("value-list")));
    assert_eq!(parts[2], Element::Token(token_named_lit("]")));
}

#[test]
fn span_fidelity_name_slices() {
    // The name spans must slice exactly the identifiers in the source.
    let doc = load(JSON).expect("loads");
    let object = doc
        .rules
        .iter()
        .find(|rule| rule.name == "object")
        .expect("object rule");
    assert_eq!(object.name_span.slice(JSON), "object");
    assert!(JSON[object.span.start..].starts_with("object"));
    let number = &doc.tokens[0];
    assert_eq!(number.name_span.slice(JSON), "number");
}

#[test]
fn round_trip_render_reloads_to_equal_ir() {
    let doc = load(JSON).expect("loads");
    let rendered = render(&doc);
    let reloaded = load(&rendered).expect("render is valid KDL again");
    let grammar_a = doc.to_grammar().expect("ir");
    let grammar_b = reloaded.to_grammar().expect("ir");
    assert_eq!(grammar_a, grammar_b, "round-trip preserves the IR");
    // Render is idempotent (canonical form).
    assert_eq!(render(&reloaded), rendered);
}

#[test]
fn separated_sugar_lowers_to_rep_and_opt() {
    let grammar = load_grammar(JSON).expect("ir");
    let pair_list = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "pair-list")
        .expect("pair-list rule");
    assert!(pair_list.list, "sep rules are flagged list");
    // pair-list = pair ("," pair)* ","?
    let Element::Seq(parts) = &pair_list.body else {
        panic!("sep lowers to a sequence");
    };
    assert_eq!(parts.len(), 3, "element, repetition, optional trailing");
    let Element::Rep { min, max, .. } = &parts[1] else {
        panic!("middle part is a repetition");
    };
    assert_eq!(*min, 0);
    assert_eq!(*max, None);
    let Element::Opt(_) = &parts[2] else {
        panic!("allow-trailing adds the optional separator");
    };

    // value-list has no allow-trailing: only two parts.
    let value_list = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "value-list")
        .expect("value-list rule");
    let Element::Seq(parts) = &value_list.body else {
        panic!("sep lowers to a sequence");
    };
    assert_eq!(parts.len(), 2, "no trailing separator part");
}

#[test]
fn anonymous_literals_become_tokens() {
    let text = r#"
language "anon"
grammar {
    start kind=seq "a" "+" "a"
}
"#;
    let grammar = load_grammar(text).expect("ir");
    // Two distinct anonymous literals ("a" shared, "+" separate).
    assert_eq!(grammar.tokens.len(), 2, "lit_a and lit_+ deduped by text");
    let names: Vec<&str> = grammar
        .tokens
        .iter()
        .map(|token| grammar.interner.lookup(token.name))
        .collect();
    assert!(names.contains(&"lit_a"));
    assert!(names.iter().any(|name| name.starts_with("lit_")));
    let start = grammar.rule(RuleId(0));
    let Element::Seq(parts) = &start.body else {
        panic!("seq body");
    };
    assert_eq!(parts.len(), 3);
    assert!(
        parts
            .iter()
            .all(|element| matches!(element, Element::Token(_))),
        "all three items are token refs"
    );
}

#[test]
fn pratt_props_lower_to_hint() {
    let text = r#"
language "pratt"
tokens {
    number kind=re "[0-9]+"
    plus kind=lit "+"
    star kind=lit "*"
}
grammar {
    sum kind=seq number plus sum lbp=10 rbp=9 assoc=left
    mul kind=seq number star mul lbp=20
}
"#;
    let grammar = load_grammar(text).expect("ir");
    let sum = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "sum")
        .expect("sum rule");
    let hint = sum.pratt.expect("explicit hint");
    assert_eq!(hint.left_bp, 10);
    assert_eq!(hint.right_bp, 9);
    // mul: lbp=20, default left assoc → rbp = 21 (the descent engine's
    // left-assoc encoding: rbp = lbp + 1).
    let mul = grammar
        .rules
        .iter()
        .find(|rule| grammar.interner.lookup(rule.name) == "mul")
        .expect("mul rule");
    assert_eq!(mul.pratt.map(|hint| hint.right_bp), Some(21));
}

#[test]
fn errors_carry_spans() {
    // Unknown reference: span points at the offending identifier.
    let text = "language \"x\"\ngrammar {\n    start kind=seq nothing\n}\n";
    let err = load_grammar(text).expect_err("unknown ref");
    let SurfaceError::UnknownRef { name, span } = err else {
        panic!("expected UnknownRef, got {err:?}");
    };
    assert_eq!(name, "nothing");
    assert_eq!(span.slice(text), "nothing");

    // Unknown section: span points at the node.
    let text = "language \"x\"\nfoo \"bar\"\n";
    let err = load(text).expect_err("unknown section");
    let SurfaceError::UnknownSection { name, span } = err else {
        panic!("expected UnknownSection, got {err:?}");
    };
    assert_eq!(name, "foo");
    assert_eq!(span.slice(text), "foo \"bar\"");

    // Duplicate name: span points at the second declaration.
    let text = "language \"x\"\ntokens { a kind=lit \"a\" }\ngrammar { a kind=seq a }\n";
    let err = load(text).expect_err("cross-namespace duplicate");
    assert!(matches!(err, SurfaceError::DuplicateName { ref name, .. } if name == "a"));

    // Reserved marker as a rule name.
    let text = "language \"x\"\ngrammar { opt kind=seq }\n";
    let err = load(text).expect_err("reserved name");
    assert!(matches!(err, SurfaceError::ReservedName { ref name, .. } if name == "opt"));

    // Dangling marker.
    let text =
        "language \"x\"\ntokens { a kind=lit \"a\" }\ngrammar { start kind=seq a opt opt }\n";
    let err = load(text).expect_err("double marker");
    assert!(matches!(err, SurfaceError::DanglingMarker { marker, .. } if marker == "opt"));
}

#[test]
fn kdl_syntax_errors_map_to_kdl_variant() {
    let err = load("language \"x\" {").expect_err("unterminated node");
    assert!(matches!(err, SurfaceError::Kdl { .. }), "{err:?}");
}

#[test]
fn doc_loading_is_deterministic() {
    // Loading the same text twice yields byte-identical docs (span
    // determinism).
    let a = load(JSON).expect("loads");
    let b = load(JSON).expect("loads");
    assert_eq!(a, b);
}

#[test]
fn byte_span_slice_and_default() {
    let span = ByteSpan::new(3, 2);
    assert_eq!(span.slice("abcdef"), "de");
    assert_eq!(ByteSpan::empty(7).slice("abcdef"), "");
    assert_eq!(ByteSpan::default(), ByteSpan::new(0, 0));
}

#[test]
fn empty_grammar_lowering_reports_ir_error() {
    let text = "language \"x\"\n";
    let err = load_grammar(text).expect_err("no rules");
    assert!(matches!(err, SurfaceError::Ir { .. }), "{err:?}");
}

#[test]
fn hand_built_doc_renders_and_reloads() {
    // The surface types are plain data: a minimal doc builds by hand and
    // renders to valid KDL.
    let doc = GirDoc {
        language: gir_syntax::LanguageMeta {
            name: "hand".to_owned(),
            version: String::new(),
            extensions: vec![],
            unicode: false,
            span: ByteSpan::default(),
        },
        tokens: vec![],
        rules: vec![],
    };
    let text = render(&doc);
    assert_eq!(text, "language \"hand\"\n");
    assert_eq!(load(&text).expect("reloads").language.name, "hand");
}

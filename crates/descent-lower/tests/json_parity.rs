//! End-to-end parity suites for the descriptor compiler (issue 043 TDD):
//! a `.gir` grammar lowered by [`descent_lower::lower`] and executed by
//! the descent engine over a lexgen scanner feed must agree with the
//! hand-written reference parser (`packs_json::reference`) on every
//! valid document — same tree shape, same token ranges, byte-for-byte
//! rendering — and stay lossless with error nodes on malformed input.

use std::sync::Arc;

use descent::parse_events;
use descent_lower::{Lowered, lower};
use gir_ir::Grammar;
use gir_syntax::load_grammar;
use packs_json::kinds as pk;
use packs_json::reference::{self, token_kinds as ptk};
use pretty_assertions::assert_eq;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};
use runtime_core::{EOF_KIND, ERROR_KIND};

/// The JSON grammar fixture: the reference pack's language expressed on
/// the KDL surface. Shape contract mirrors `packs_json` (issue 019):
/// value leaves are bare tokens; `object`/`pair`/`array` are the only
/// nodes; lists are FIRST-guarded separations.
const JSON_GRAMMAR: &str = descent_lower::JSON_GRAMMAR;

/// Lower the fixture once per call: the pipeline entry under test.
fn lowered() -> (Grammar, Lowered) {
    let grammar = load_grammar(JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    (grammar, lowered)
}

/// The generated kind of a rule (panics on transparent rules — the
/// fixture only queries node rules).
fn rule_kind(grammar: &Grammar, lowered: &Lowered, name: &str) -> u16 {
    for (idx, rule) in grammar.rules.iter().enumerate() {
        if grammar.interner.lookup(rule.name) == name {
            return lowered.kind_of_rule[idx].expect("node rule has a kind");
        }
    }
    panic!("no rule named {name}")
}

/// The generated kind of a token.
fn token_kind(grammar: &Grammar, lowered: &Lowered, name: &str) -> u16 {
    for (idx, token) in grammar.tokens.iter().enumerate() {
        if grammar.interner.lookup(token.name) == name {
            return lowered.kind_of_token[idx];
        }
    }
    panic!("no token named {name}")
}

/// Everything one document's generated parse produces.
struct Generated {
    /// The wrapped green tree.
    tree: Arc<GreenNode>,
    /// The loaded grammar (name lookups).
    grammar: Grammar,
    /// The lowered descriptors + kind maps.
    lowered: Lowered,
    /// The clean feed the engine consumed.
    feed: Vec<(u16, std::ops::Range<u32>)>,
}

/// Parse a document through the full generated path: load → lower →
/// lexgen scan → clean feed → descent engine → wrapped green tree.
fn generated_parts(source: &str) -> Generated {
    let (grammar, lowered) = lowered();
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let tokens = lowered.feed(&scanner, source);
    let run = parse_events(&lowered.desc, &tokens, source);
    let tree = descent::green_tree(
        &run,
        source,
        reference::ROOT_KIND,
        lowered.trivia_kind.expect("fixture has trivia"),
    )
    .expect("tree integration never fails structurally");
    Generated {
        tree,
        grammar,
        lowered,
        feed: tokens,
    }
}

fn generated_tree(source: &str) -> Arc<GreenNode> {
    generated_parts(source).tree
}

// ---------------------------------------------------------------------------
// Kind numbering (the documented, deterministic mapping)
// ---------------------------------------------------------------------------

/// The dense kind space: `node="true"` rules in declaration order, then
/// `node="true"` tokens, then plain tokens, then trivia; the unmatched
/// sentinel lands last.
#[test]
fn kind_numbering_is_dense_and_declaration_ordered() {
    let (grammar, lowered) = lowered();
    assert_eq!(rule_kind(&grammar, &lowered, "object"), 0);
    assert_eq!(rule_kind(&grammar, &lowered, "pair"), 1);
    assert_eq!(rule_kind(&grammar, &lowered, "array"), 2);
    assert_eq!(token_kind(&grammar, &lowered, "number"), 3);
    assert_eq!(token_kind(&grammar, &lowered, "string"), 4);
    assert_eq!(token_kind(&grammar, &lowered, "lbrace"), 5);
    assert_eq!(token_kind(&grammar, &lowered, "rbrace"), 6);
    assert_eq!(token_kind(&grammar, &lowered, "lbrack"), 7);
    assert_eq!(token_kind(&grammar, &lowered, "rbrack"), 8);
    assert_eq!(token_kind(&grammar, &lowered, "colon"), 9);
    assert_eq!(token_kind(&grammar, &lowered, "comma"), 10);
    assert_eq!(token_kind(&grammar, &lowered, "true-lit"), 11);
    assert_eq!(token_kind(&grammar, &lowered, "false-lit"), 12);
    assert_eq!(token_kind(&grammar, &lowered, "null-lit"), 13);
    assert_eq!(token_kind(&grammar, &lowered, "ws"), 14);
    assert_eq!(lowered.bad_kind, 15);
    // Transparent rules carry no kind.
    let value_idx = grammar
        .rules
        .iter()
        .position(|rule| grammar.interner.lookup(rule.name) == "value")
        .expect("value rule");
    assert!(lowered.kind_of_rule[value_idx].is_none());
}

// ---------------------------------------------------------------------------
// Tree-shape parity against the hand-written reference (valid corpus)
// ---------------------------------------------------------------------------

/// One DFS step of a name-resolved tree shape: node enter/exit or a
/// non-trivia token. Kinds map through the grammar interner so the
/// comparison is structural, never numeric.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Enter(String),
    Exit,
    Token(String, std::ops::Range<u32>),
}

fn named_shape(tree: &GreenNode, grammar: &Grammar, lowered: &Lowered, trivia: &str) -> Vec<Step> {
    fn name_of_rule_kind(kind: u16, grammar: &Grammar, lowered: &Lowered) -> String {
        if kind == ERROR_KIND {
            return "<error>".to_owned();
        }
        if kind == reference::ROOT_KIND {
            return "<root>".to_owned();
        }
        for (idx, mapped) in lowered.kind_of_rule.iter().enumerate() {
            if *mapped == Some(kind) {
                return grammar
                    .rule_name(gir_ir::RuleId(u32::try_from(idx).expect("small")))
                    .to_owned();
            }
        }
        format!("kind{kind}")
    }

    fn name_of_token_kind(kind: u16, grammar: &Grammar, lowered: &Lowered) -> String {
        if kind == ERROR_KIND {
            return "<error>".to_owned();
        }
        if kind == EOF_KIND {
            return "<eof>".to_owned();
        }
        for (idx, mapped) in lowered.kind_of_token.iter().enumerate() {
            if *mapped == kind {
                return grammar
                    .token_name(gir_ir::TokenId(u16::try_from(idx).expect("small")))
                    .to_owned();
            }
        }
        format!("kind{kind}")
    }

    fn walk(
        node: &GreenNode,
        out: &mut Vec<Step>,
        grammar: &Grammar,
        lowered: &Lowered,
        trivia: &str,
    ) {
        out.push(Step::Enter(name_of_rule_kind(node.kind, grammar, lowered)));
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => walk(sub, out, grammar, lowered, trivia),
                GreenChild::Token { kind, range } => {
                    let name = name_of_token_kind(*kind, grammar, lowered);
                    if name != trivia {
                        out.push(Step::Token(name, range.clone()));
                    }
                }
            }
        }
        out.push(Step::Exit);
    }
    let mut out = Vec::new();
    walk(tree, &mut out, grammar, lowered, trivia);
    out
}

/// The reference pack's kind → name table (issue 019 numbering); the
/// error sentinel covers every unmapped kind.
fn reference_name(kind: u16) -> &'static str {
    match kind {
        pk::OBJECT => "object",
        pk::ARRAY => "array",
        pk::PAIR => "pair",
        pk::STRING => "string",
        pk::NUMBER => "number",
        pk::TRUE => "true-lit",
        pk::FALSE => "false-lit",
        pk::NULL => "null-lit",
        ptk::LBRACE => "lbrace",
        ptk::RBRACE => "rbrace",
        ptk::LBRACK => "lbrack",
        ptk::RBRACK => "rbrack",
        ptk::COLON => "colon",
        ptk::COMMA => "comma",
        ptk::WHITESPACE => "ws",
        reference::ROOT_KIND => "<root>",
        EOF_KIND => "<eof>",
        _ => "<error>",
    }
}

fn reference_shape(tree: &GreenNode) -> Vec<Step> {
    fn walk(node: &GreenNode, out: &mut Vec<Step>) {
        out.push(Step::Enter(reference_name(node.kind).to_owned()));
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => walk(sub, out),
                GreenChild::Token { kind, range } => {
                    if reference_name(*kind) != "ws" {
                        out.push(Step::Token(reference_name(*kind).to_owned(), range.clone()));
                    }
                }
            }
        }
        out.push(Step::Exit);
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

const VALID_CORPUS: &[&str] = &[
    "{}",
    "[]",
    r#"{"a": 1}"#,
    "[1, 2, 3]",
    r#"{"name": "syntact", "version": 1, "ok": true, "bits": [true, false, null]}"#,
    "3.14e-2",
    "-17",
    r#""str with \u00e9 escape""#,
    "  [ 1 , 2 ]  ",
    r#"{"a": [1, 2, {"b": null}], "c": true}"#,
    "1 2",
    r#""""#,
    "",
];

#[test]
fn generated_parser_matches_reference_shape_on_valid_corpus() {
    for source in VALID_CORPUS {
        let generated = generated_parts(source);
        let theirs: Arc<GreenNode> = packs_json::parse(source).expect("reference parses");
        assert_eq!(
            named_shape(
                &generated.tree,
                &generated.grammar,
                &generated.lowered,
                "ws"
            ),
            reference_shape(&theirs),
            "shape parity on {source:?}"
        );
    }
}

#[test]
fn generated_parser_is_byte_faithful_on_valid_corpus() {
    for source in VALID_CORPUS {
        let tree = generated_tree(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Malformed corpus: recovery keeps the stream lossless and progressing
// ---------------------------------------------------------------------------

const MALFORMED_CORPUS: &[&str] = &["{\"a\": }", "[1, 2", "tru", "@#$", "{\"a\": 1", "[", "}"];

#[test]
fn malformed_inputs_error_and_stay_lossless() {
    for source in MALFORMED_CORPUS {
        let generated = generated_parts(source);
        let lowered = &generated.lowered;
        let run = parse_events(&lowered.desc, &generated.feed, source);
        assert!(run.errors >= 1, "at least one error on {source:?}");
        assert!(
            descent::stream_is_balanced(&run, source, reference::ROOT_KIND),
            "balanced + partitioning on {source:?}"
        );
        let tree = generated_tree(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "lossless on {source:?}"
        );
    }
}

#[test]
fn recovery_always_makes_progress_on_garbage() {
    let source = "@!#?@! #garbage !!!!";
    let (grammar, lowered) = lowered();
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let feed = lowered.feed(&scanner, source);
    let run = parse_events(&lowered.desc, &feed, source);
    assert_eq!(run.errors, feed.len(), "one error per garbage token");
    assert!(descent::stream_is_balanced(
        &run,
        source,
        reference::ROOT_KIND
    ));
}

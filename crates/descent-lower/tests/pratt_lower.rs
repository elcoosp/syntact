//! Pratt-lowering suites (issue 043 TDD): a pratt-hinted grammar rule
//! lowers into a [`descent::PrattDesc`] table that reproduces the
//! golden precedence shapes the hand-built backend fixtures pin —
//! left-associative chains nest left, parenthesized groups stay
//! transparent, prefix unaries wrap their operand, and operator nodes
//! reuse the operator token's kind (tree-sitter's anonymous-node
//! convention) inside the rule's own node.

use std::sync::Arc;

use descent::parse_events;
use descent_lower::{Lowered, lower};
use gir_ir::{Grammar, RuleId, TokenId};
use gir_syntax::load_grammar;
use pretty_assertions::assert_eq;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

/// One operator precedence level, left-associative (the IR hint is
/// per-rule — multi-level hints are a documented follow-up seam);
/// prefix `-`; transparent `( … )` groups; whitespace trivia.
const CALC_GRAMMAR: &str = r#"
language "calc" version="1.0.0" extension="calc"

tokens {
    int    kind=re "[0-9]+" node="true"
    plus   kind=lit "+"
    dash   kind=lit "-"
    star   kind=lit "*"
    lparen kind=lit "("
    rparen kind=lit ")"
    ws     kind=re "[ \t\r\n]+" trivia="true"
}

grammar {
    expr  node="true" lbp=1 kind=alt add sub mul group neg int
    add   kind=seq expr plus expr
    sub   kind=seq expr dash expr
    mul   kind=seq expr star expr
    group kind=seq lparen expr rparen
    neg   kind=seq dash expr
}
"#;

fn lowered() -> (Grammar, Lowered) {
    let grammar = load_grammar(CALC_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    (grammar, lowered)
}

/// Parse through the full generated path and return the wrapped tree.
fn parse_tree(source: &str) -> (Arc<GreenNode>, Grammar, Lowered) {
    let (grammar, lowered) = lowered();
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let tokens = lowered.feed(&scanner, source);
    let run = parse_events(&lowered.desc, &tokens, source);
    assert_eq!(run.errors, 0, "clean parse for {source:?}");
    let trivia = lowered
        .trivia_kind
        .expect("fixture declares a trivia token");
    let tree = descent::green_tree(&run, source, u16::MAX - 3, trivia)
        .expect("tree integration never fails structurally");
    (tree, grammar, lowered)
}

/// The readable spine of a tree: `(rule …)` for rule nodes, `(token
/// "text")` for node-wrapped leaves, operator text for plain tokens;
/// trivia skipped throughout.
fn spine(tree: &GreenNode, source: &str, grammar: &Grammar, lowered: &Lowered) -> String {
    fn rule_name(kind: u16, grammar: &Grammar, lowered: &Lowered) -> Option<String> {
        for (idx, mapped) in lowered.kind_of_rule.iter().enumerate() {
            if *mapped == Some(kind) {
                return Some(
                    grammar
                        .rule_name(RuleId(u32::try_from(idx).expect("small")))
                        .to_owned(),
                );
            }
        }
        None
    }
    fn token_name(kind: u16, grammar: &Grammar, lowered: &Lowered) -> Option<String> {
        for (idx, mapped) in lowered.kind_of_token.iter().enumerate() {
            if *mapped == kind {
                return Some(
                    grammar
                        .token_name(TokenId(u16::try_from(idx).expect("small")))
                        .to_owned(),
                );
            }
        }
        None
    }
    fn text_of(range: &std::ops::Range<u32>, source: &str) -> String {
        source[range.start as usize..range.end as usize].to_owned()
    }
    fn walk(
        node: &GreenNode,
        source: &str,
        out: &mut String,
        grammar: &Grammar,
        lowered: &Lowered,
    ) {
        // Rule nodes print by rule name; token-kind nodes (operator
        // applications, node-wrapped leaves) print by token name.
        let label = rule_name(node.kind, grammar, lowered)
            .or_else(|| token_name(node.kind, grammar, lowered));
        let Some(label) = label else {
            // Unnamed wrapper (the implicit root): descend without
            // printing.
            for child in &node.children {
                if let GreenChild::Node(sub) = child {
                    walk(sub, source, out, grammar, lowered);
                }
            }
            return;
        };
        if label == "ws" {
            return;
        }
        out.push('(');
        out.push_str(&label);
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => {
                    if !out.ends_with('(') {
                        out.push(' ');
                    }
                    walk(sub, source, out, grammar, lowered);
                }
                GreenChild::Token { kind, range } => {
                    let name = token_name(*kind, grammar, lowered).expect("mapped token");
                    if name == "ws" {
                        continue;
                    }
                    if !out.ends_with('(') {
                        out.push(' ');
                    }
                    out.push_str(&text_of(range, source));
                }
            }
        }
        out.push(')');
    }
    let mut out = String::new();
    walk(tree, source, &mut out, grammar, lowered);
    out
}

#[test]
fn left_associative_chain_nests_left() {
    let source = "1 + 2 + 3";
    let (tree, grammar, lowered) = parse_tree(source);
    assert_eq!(
        spine(&tree, source, &grammar, &lowered),
        "(expr (plus (plus (int 1) + (int 2)) + (int 3)))"
    );
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

#[test]
fn single_precedence_chain_groups_left_to_right() {
    let source = "1 + 2 - 3 * 4";
    let (tree, grammar, lowered) = parse_tree(source);
    // One precedence level: every operator shares lbp=1, so grouping
    // is strictly left-to-right — `*` does NOT bind tighter (the
    // multi-level hint is the documented follow-up seam).
    assert_eq!(
        spine(&tree, source, &grammar, &lowered),
        "(expr (star (dash (plus (int 1) + (int 2)) - (int 3)) * (int 4)))"
    );
}

#[test]
fn parenthesized_group_stays_transparent() {
    let source = "(1 + 2) + 3";
    let (tree, grammar, lowered) = parse_tree(source);
    // The group contributes no node of its own: the inner plus stands
    // for it — only the bare paren tokens remain as leaves (the
    // engine's transparent-group contract).
    assert_eq!(
        spine(&tree, source, &grammar, &lowered),
        "(expr (plus ((plus (int 1) + (int 2)) ) + (int 3)))"
    );
}

#[test]
fn prefix_unary_wraps_its_operand_not_the_infix_tail() {
    let source = "-1 + 2";
    let (tree, grammar, lowered) = parse_tree(source);
    assert_eq!(
        spine(&tree, source, &grammar, &lowered),
        "(expr (plus (dash - (int 1)) + (int 2)))"
    );
}

#[test]
fn byte_faithful_on_expressions() {
    for source in ["-1 + 2", "(1 + 2) + 3", "1+2*3", "- 5", "42"] {
        let (tree, _, _) = parse_tree(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            source,
            "lossless on {source:?}"
        );
    }
}

#[test]
fn pratt_table_lands_in_the_descriptor() {
    let (_, lowered) = lowered();
    assert_eq!(lowered.desc.exprs.len(), 1, "one pratt table");
    let table = &lowered.desc.exprs[0];
    assert_eq!(table.infix.len(), 3, "plus, dash, star");
    assert_eq!(table.prefix.len(), 1, "prefix dash");
    assert_eq!(table.postfix.len(), 0);
    assert_eq!(table.group, Some((5, 6)), "lparen..rparen by the kind map");
    // Kind map: node rule expr=0, node token int=1, plain tokens
    // plus=2 dash=3 star=4 lparen=5 rparen=6, trivia ws=7, bad=8.
    assert_eq!(lowered.bad_kind, 8);
    assert_eq!(lowered.kind_of_rule[0], Some(0), "expr");
    // Left-assoc binding powers straight from the hint.
    assert_eq!(table.infix[0].lbp, 1);
    assert_eq!(table.infix[0].rbp, 2, "left-assoc: rbp = lbp + 1");
    // Operator nodes ride the operator token's kind.
    assert_eq!(table.infix[0].node, 2, "plus node kind == plus token kind");
    assert_eq!(table.prefix[0].node, 3, "dash node kind == dash token kind");
    // The rule delegates to the table and carries the rule kind.
    assert_eq!(lowered.desc.rules[0].node_kind, 0);
    assert!(matches!(lowered.desc.rules[0].body, descent::Pat::Expr(0)));
    // Prefix unaries bind tighter than every infix level.
    assert_eq!(table.prefix[0].bp, 2);
}

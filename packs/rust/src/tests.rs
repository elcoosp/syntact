//! packs-rust test suite (issue 033 TDD): lifetime disambiguation,
//! macro token-tree balance, generics/where shapes, per-family tests,
//! losslessness, determinism, recovery — mirroring the packs-python
//! discipline.

use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::ROOT_KIND;
use crate::kinds as nk;
use crate::reference::parse_events;
use crate::token_kinds as tk;
use crate::{lexer, parse};

/// The corpus: `(source, tolerated errors)` across the documented subset.
const CORPUS: &[(&str, usize)] = &[
    // functions + params
    ("fn main() {}\n", 0),
    ("fn add(a: i32, b: i32) -> i32 { a + b }\n", 0),
    ("fn f(x: &str, y: &mut u8) {}\n", 0),
    ("fn method(&self, v: Vec<u8>) -> u64 { 0 }\n", 0),
    // generics + where
    ("fn g<T: Clone>(t: T) {}\n", 0),
    ("struct S<T> { v: Vec<T> }\n", 0),
    ("fn w<T>(t: T)\nwhere\n    T: Clone + Send,\n{\n}\n", 0),
    // structs / enums
    ("struct Unit;\n", 0),
    ("struct Tup(i32, String);\n", 0),
    ("struct P { x: i32, y: i32 }\n", 0),
    ("enum E { A, B(i32), C { k: u8 } }\n", 0),
    // impl / trait
    ("impl P { fn x(&self) -> i32 { self.x } }\n", 0),
    (
        "impl Clone for P { fn clone(&self) -> Self { P { x: 0, y: 0 } } }\n",
        0,
    ),
    ("trait T { fn m(&self); fn d(&self) {} }\n", 0),
    // use trees
    ("use std::collections::HashMap;\n", 0),
    ("use a::b::{c, d as e, *};\n", 0),
    ("use std::{fmt, io};\n", 0),
    // mod / const / type
    ("mod m;\n", 0),
    ("mod inner { fn f() {} }\n", 0),
    ("const N: usize = 3;\n", 0),
    ("static S: u8 = 0;\n", 0),
    ("type Pair<T> = (T, T);\n", 0),
    // let + patterns
    ("fn f() { let x = 1; let (a, b) = (1, 2); let _ = x; }\n", 0),
    ("fn f() { let mut v: Vec<u8> = Vec::new(); }\n", 0),
    // Pratt expressions
    ("fn f() { let r = 1 + 2 * 3 - 4 / 2; }\n", 0),
    ("fn f() { let b = a && c || !d == e; }\n", 0),
    ("fn f() { let s = v[0] + p.x; }\n", 0),
    ("fn f() { let c = f(a, b).g().h[1]; }\n", 0),
    ("fn f() { let t = (1, 2); let u = (); }\n", 0),
    ("fn f() { let arr = [1, 2, 3]; let z = [0; 8]; }\n", 0),
    // closures + ranges
    ("fn f() { let g = |x: i32| x + 1; }\n", 0),
    ("fn f() { let g = move || 42; }\n", 0),
    ("fn f() { let r = 1..10; }\n", 0),
    ("fn f() { let r = 1..=10; }\n", 0),
    ("fn f() { let r = ..end; }\n", 0),
    // casts / unary
    ("fn f() { let c = x as u64; }\n", 0),
    ("fn f() { let n = -x * !y; }\n", 0),
    ("fn f() { let r = &mut v; }\n", 0),
    // control flow
    (
        "fn f(c: bool) { if c { 1 } else if !c { 2 } else { 3 } }\n",
        0,
    ),
    ("fn f() { let x = if c { 1 } else { 2 }; }\n", 0),
    (
        "fn f(v: Option<u8>) { match v { Some(x) => x, None => 0 } }\n",
        0,
    ),
    (
        "fn f() { match p { P { x, y: 0 } if x > 1 => x, _ => 0 } }\n",
        0,
    ),
    ("fn f() { loop { break; } }\n", 0),
    ("fn f() { while c { continue; } }\n", 0),
    ("fn f(it: Vec<u8>) { for x in it { f(x); } }\n", 0),
    ("fn f() { return 1 + 2; }\n", 0),
    ("fn f() { unsafe { 1 } }\n", 0),
    // paths + turbofish
    ("fn f() { std::io::write(); }\n", 0),
    ("fn f() { let x = Vec::<u8>::new(); }\n", 0),
    // lifetimes + fn ptrs in types
    ("fn f<'a>(x: &'a str) -> &'a str { x }\n", 0),
    ("fn f(cb: fn(i32) -> bool) {}\n", 0),
    ("type R<'a> = &'a mut [u8];\n", 0),
    // macros
    ("fn f() { println!(\"hi\"); }\n", 0),
    ("fn f() { m!(a, (b, [c]), {d}); }\n", 0),
    ("fn f() { v![1; 2]; }\n", 0),
    // attributes + doc comments
    ("#[derive(Debug, Clone)]\nstruct P { x: i32 }\n", 0),
    ("/// docs\nfn f() {}\n", 0),
    ("#![allow(dead_code)]\nfn f() {}\n", 0),
    // comments / block comments trivia
    ("// line\n/* block /* nested */ */\nfn f() {}\n", 0),
    // malformed with budgets
    ("fn f( {}\n", 2),
    ("let = 1;\n", 1),
    ("struct { x: i32 }\n", 2),
    ("fn f() { let x = ; }\n", 1),
    ("use std::;\n", 1),
    ("fn f() { match x { => 0 } }\n", 1),
];

#[test]
fn corpus_is_lossless_and_within_budgets() {
    for (source, tolerated) in CORPUS {
        let tree = parse(source).expect("tree integration");
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert!(errors <= *tolerated, "{source:?}: {errors} > {tolerated}");
    }
}

#[test]
fn valid_fixtures_are_error_free_and_deterministic() {
    for (source, tolerated) in CORPUS {
        if *tolerated != 0 {
            continue;
        }
        let first = parse_events(source);
        let second = parse_events(source);
        assert_eq!(first, second, "determinism on {source:?}");
        assert_eq!(first.2, 0, "error-free on {source:?}");
        let mut wrapped = Vec::with_capacity(first.0.len() + 2);
        wrapped.push(Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(first.0.iter().copied());
        wrapped.push(Event::Finish);
        assert!(process(&wrapped).is_ok(), "balanced on {source:?}");
    }
}

#[test]
fn lifetime_vs_char_literal_disambiguation() {
    let scan = lexer::scan("fn f<'a>(c: &'a str) { let ch = 'x'; let nl = '\\n'; }");
    let kinds: Vec<u16> = scan.tokens.iter().map(|t| t.kind).collect();
    assert!(kinds.contains(&tk::LIFETIME), "lifetimes present");
    assert_eq!(
        kinds.iter().filter(|k| **k == tk::LIFETIME).count(),
        2,
        "'a and 'a are lifetimes"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == tk::CHAR_LIT).count(),
        2,
        "'x' and '\\n' are char literals"
    );
    assert_eq!(scan.errors, 0);
    // 'static lifetime.
    let scan = lexer::scan("static S: &'static str = \"s\";");
    assert_eq!(scan.tokens[4].kind, tk::LIFETIME, "'static is a lifetime");
}

#[test]
fn doc_comments_are_trivia_never_tokens() {
    let source = "/// item docs\n// plain\nfn f() {}\n/*! inner */\n";
    let scan = lexer::scan(source);
    assert!(scan.tokens.iter().all(|t| t.kind != tk::DOC_COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::DOC_COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::COMMENT));
    assert_eq!(scan.errors, 0);
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

#[test]
fn nested_block_comments_are_trivia() {
    let source = "/* a /* b */ c */ fn f() {}\n";
    let scan = lexer::scan(source);
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::COMMENT));
    assert_eq!(scan.tokens[0].kind, tk::KW_FN, "fn follows the comment");
    assert_eq!(scan.errors, 0);
}

#[test]
fn macro_token_trees_are_balanced() {
    let source = "fn f() { m!(a, (b, [c]), {d}); }";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::MACRO_CALL), "macro call present");
    assert_eq!(
        shape.iter().filter(|k| **k == nk::TOKEN_TREE).count(),
        4,
        "outer + (b,[c]) + [c] + brace-d — every balanced group its own node"
    );
    // All three delimiters.
    let tree = parse("fn f() { a!(); b![]; c!{}; }").expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::MACRO_CALL).count(), 3);
}

#[test]
fn attributes_are_nodes_with_token_tree_payload() {
    let source = "#[derive(Debug, Clone)]\n#[cfg(test)]\nstruct P;\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::ATTR).count(), 2);
    assert!(
        shape.contains(&nk::STRUCT_DEF),
        "the attributed struct still parses"
    );
}

#[test]
fn expression_precedence_golden_shapes() {
    // `1 + 2 * 3` — the `*` BIN nests inside the `+` BIN.
    let tree = parse("fn f() { let r = 1 + 2 * 3; }").expect("tree");
    let bin = first_bin(&tree);
    assert!(has_bin_child(bin), "* nests under +");
    // `-x * !y` — unary operands nest under the binary op.
    let tree = parse("fn f() { let r = -x * !y; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::UNARY_EXPR));
    assert!(shape.contains(&nk::BIN_EXPR));
    // Right-assoc assignment: `a = b = c` — one BIN under another.
    let tree = parse("fn f() { a = b = c; }").expect("tree");
    let bin = first_bin(&tree);
    assert!(has_bin_child(bin), "assignment is right-associative");
    // Range below `+`: `1..x + 2` → RANGE(1, BIN(+)).
    let tree = parse("fn f() { let r = 1..x + 2; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::RANGE_EXPR));
    assert!(shape.contains(&nk::BIN_EXPR));
}

#[test]
fn closures_and_ranges_shapes() {
    let tree = parse("fn f() { let g = move |x: i32, y| x + y; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CLOSURE_EXPR));
    let tree = parse("fn f() { let r = ..=end; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::RANGE_EXPR), "prefix range");
}

#[test]
fn generics_where_and_turbofish_shapes() {
    let source = "fn w<T: Clone>(t: T) -> T\nwhere\n    T: Send,\n{ t }\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::TYPE_PARAMS));
    assert!(shape.contains(&nk::WHERE_CLAUSE));
    assert!(shape.contains(&nk::PARAM_LIST));

    let tree = parse("fn f() { Vec::<u8>::new(); }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::GENERIC_ARGS));
    assert!(shape.contains(&nk::PATH_EXPR));
    assert!(shape.contains(&nk::CALL));
}

#[test]
fn match_arms_patterns_and_guards() {
    let source = "fn f(v: Option<u8>) {\n    match v {\n        Some(x) | Some(y) if x > 0 => x,\n        None => 0,\n    }\n}\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::MATCH_EXPR));
    assert!(shape.contains(&nk::MATCH_ARM));
    assert!(shape.contains(&nk::GUARD));
    assert!(shape.contains(&nk::PATH_PAT));
    let arms = shape.iter().filter(|k| **k == nk::MATCH_ARM).count();
    assert_eq!(arms, 2);
}

#[test]
fn struct_enum_field_shapes() {
    let source = "struct P { x: i32, y: i32 }\nenum E { A, B(i32), C { k: u8 } }\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::FIELD).count(), 3);
    assert!(shape.contains(&nk::TUPLE_STRUCT_BODY));
    assert_eq!(shape.iter().filter(|k| **k == nk::ENUM_VARIANT).count(), 3);
    // Tuple struct.
    let tree = parse("struct Tup(i32, String);").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::TUPLE_STRUCT_BODY));
}

#[test]
fn use_tree_nesting_shapes() {
    let source = "use a::b::{c::{d, e}, f as g, *};";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::USE_DECL));
    // Shape: outer tree, `c::{d, e}` tree, `d`, `e`, `f as g`, `*` —
    // brace groups ride their parent (documented).
    assert_eq!(
        shape.iter().filter(|k| **k == nk::USE_TREE).count(),
        6,
        "each inner path is its own USE_TREE"
    );
}

#[test]
fn control_flow_block_like_statements_need_no_semicolon() {
    let source = "fn f(c: bool) {\n    if c { a(); }\n    match c { _ => {} }\n    loop { break; }\n    g()\n}\n";
    let tree = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "lossless control-flow suite"
    );
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    // Block-like `if` swallows the else chain; a bare call needs no `;`
    // before `}`.
    let tree = parse("fn f() { if a { } else { } h() }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::IF_EXPR));
    assert!(shape.contains(&nk::CALL));
}

#[test]
fn fn_pointers_and_ref_types() {
    let source = "type Cb = fn(i32) -> bool;\nfn f(r: &mut [u8]) {}\nfn g(t: (u8, u8)) {}\nfn h(a: [u8; 4]) {}\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::FN_PTR_TYPE));
    assert!(shape.contains(&nk::REF_TYPE));
    assert!(shape.contains(&nk::TUPLE_TYPE));
    assert!(shape.contains(&nk::ARRAY_TYPE));
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
}

#[test]
fn recovery_stays_lossless_and_progresses() {
    let source = "fn f() { let x = ; }\nlet = 1;\nfn g() {}\n";
    let tree = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "recovered regions keep every byte"
    );
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 2, "both broken regions report errors");
    let tree2: Arc<GreenNode> = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        tree2.render(&Source::from_str_unchecked(source)),
        "deterministic recovery"
    );
}

#[test]
fn unterminated_string_is_an_error_region() {
    let source = "fn f() { let s = \"oops\n}\n";
    let tree = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "unterminated string stays lossless"
    );
    let scan = lexer::scan(source);
    assert!(scan.errors >= 1, "the scanner flags the bad literal");
}

/// All node kinds in DFS order.
fn node_kinds(tree: &GreenNode) -> Vec<u16> {
    fn walk(node: &GreenNode, out: &mut Vec<u16>) {
        out.push(node.kind);
        for child in &node.children {
            if let GreenChild::Node(sub) = child {
                walk(sub, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// The first `BIN_EXPR` node in DFS order.
fn first_bin(tree: &GreenNode) -> &GreenNode {
    fn walk(node: &GreenNode) -> Option<&GreenNode> {
        if node.kind == nk::BIN_EXPR {
            return Some(node);
        }
        node.children.iter().find_map(|c| match c {
            GreenChild::Node(sub) => walk(sub),
            GreenChild::Token { .. } => None,
        })
    }
    walk(tree).expect("a BIN_EXPR exists")
}

/// Whether the node directly contains a nested `BIN_EXPR` child.
fn has_bin_child(node: &GreenNode) -> bool {
    node.children
        .iter()
        .any(|c| matches!(c, GreenChild::Node(sub) if sub.kind == nk::BIN_EXPR))
}

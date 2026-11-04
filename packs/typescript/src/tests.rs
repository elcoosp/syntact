//! packs-typescript test suite (issue 027 TDD): per-family shapes, the
//! `>>` split in nested generics, ASI cases, template nesting, doc
//! comments as trivia, losslessness, determinism, recovery progress.

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::ROOT_KIND;
use crate::kinds as nk;
use crate::lexer;
use crate::parse;
use crate::reference::parse_events;
use crate::token_kinds as tk;

/// The corpus: `(source, tolerated errors)` across the subset.
const CORPUS: &[(&str, usize)] = &[
    // modules
    ("import x from \"m\";\n", 0),
    ("import { a, b } from \"m\";\n", 0),
    ("import def, { a } from \"m\";\n", 0),
    ("export const x = 1;\n", 0),
    ("export default function f() {}\n", 0),
    // interfaces / type aliases
    ("interface Point { x: number; y: number }\n", 0),
    ("interface P { m(a: string): void; opt?: number }\n", 0),
    ("type A = string | number;\n", 0),
    ("type F = (a: string) => boolean;\n", 0),
    ("type Pair = [string, number];\n", 0),
    ("type Obj = { a: number; b?: string };\n", 0),
    // classes
    (
        "class C {\n  readonly x: number = 1;\n  private m(a: number): string { return \"s\"; }\n}\n",
        0,
    ),
    ("enum E { A, B, C = 2 }\n", 0),
    // functions / params
    (
        "function f(a: number, b?: string, ...rest: boolean[]): void {}\n",
        0,
    ),
    ("function g(a = 1, b = a + 1) { return b; }\n", 0),
    // statements
    ("const x: number = 1;\nlet y = x;\nvar z;\n", 0),
    ("if (x > 1) { y = 2; } else { y = 3; }\n", 0),
    ("while (x) { x = x - 1; }\n", 0),
    ("for (const v of items) { f(v); }\n", 0),
    ("function f() { return 42; }\n", 0),
    // expressions
    ("x = a === b && c !== d || !e;\n", 0),
    ("x = 2 ** 3 ** 2;\n", 0),
    ("x = obj.attr[0](1, 2);\n", 0),
    ("x = [1, { a: 2 }, \"s\"];\n", 0),
    ("x = (a, b) => a + b;\n", 0),
    ("x = (a: number): number => a * 2;\n", 0),
    // template literals
    ("x = `plain`;\n", 0),
    ("x = `a ${b} c`;\n", 0),
    ("x = `a ${`b ${c}`} d`;\n", 0),
    // ASI
    ("let a = 1\nlet b = 2\n", 0),
    ("function f() {\n  return\n}\n", 0),
    // nested generics (the >> split)
    ("let m: Array<Array<number>> = [];\n", 0),
    ("type T = Map<string, Array<number>>;\n", 0),
    // shifts still parse (combined from the split tokens)
    ("x = a >> 2;\ny = a << 1;\n", 0),
    // doc comments as trivia
    ("/** docs */\nclass C {}\n", 0),
    // malformed with budgets
    ("x = ;\n", 1),
    ("function f( {\n", 3),
    ("interface I { x: }\n", 1),
    ("let = 3;\n", 1),
    ("x = [1, 2\n", 1),
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
fn valid_fixtures_are_error_free_deterministic_and_balanced() {
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
fn nested_generics_split_the_shift_tokens() {
    // The lexer never merges `>`: `Array<Array<number>>` closes with two
    // single GT tokens.
    let source = "let m: Array<Array<number>> = [];\n";
    let scan = lexer::scan(source);
    let gts = scan.tokens.iter().filter(|t| t.kind == tk::GT).count();
    assert_eq!(gts, 2, "the >> split");
    assert!(!scan.tokens.iter().any(|t| t.kind == 0xBEEF));
    // And the parser accepts the declaration error-free.
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
}

#[test]
fn shift_operators_combine_from_split_tokens() {
    // `>>` / `<<` in expression position: two split tokens, one BIN_EXPR.
    let source = "x = a >> 2;\n";
    let scan = lexer::scan(source);
    assert_eq!(scan.tokens.iter().filter(|t| t.kind == tk::GT).count(), 2);
    let tree = parse(source).expect("tree");
    let bin = count_kind(&tree, nk::BIN_EXPR);
    assert_eq!(bin, 1, "the shift is one binary node");
    // `>>>` combines three.
    let source = "x = a >>> 2;\n";
    let scan = lexer::scan(source);
    assert_eq!(scan.tokens.iter().filter(|t| t.kind == tk::GT).count(), 3);
    let tree = parse(source).expect("tree");
    assert_eq!(count_kind(&tree, nk::BIN_EXPR), 1);
}

#[test]
fn asi_cases() {
    // No semicolons: statements end at the newline.
    let source = "let a = 1\nlet b = 2\n";
    let tree = parse(source).expect("tree");
    assert_eq!(count_kind(&tree, nk::VAR_DECL), 2);
    // Restricted production: `return` ends at the newline, the value
    // becomes its own expression statement.
    let source = "function f() {\n  return\n}\n";
    let tree = parse(source).expect("tree");
    let ret = find_kind(&tree, nk::RETURN_STMT).expect("return node");
    assert!(
        !contains_kind(ret, nk::BIN_EXPR) && !has_number_child(ret),
        "return carries no value across the newline"
    );
    // Semicolons still work.
    let source = "let a = 1;\nlet b = 2;\n";
    assert_eq!(count_kind(&parse(source).expect("tree"), nk::VAR_DECL), 2);
}

#[test]
fn template_nesting_shape() {
    let source = "x = `a ${`b ${c}`} d`;\n";
    let scan = lexer::scan(source);
    let starts = scan
        .tokens
        .iter()
        .filter(|t| t.kind == tk::TEMPLATE_START)
        .count();
    assert_eq!(starts, 2, "outer + nested template");
    let tree = parse(source).expect("tree");
    assert_eq!(count_kind(&tree, nk::TEMPLATE_EXPR), 2);
    assert_eq!(count_kind(&tree, nk::CALL), 0);
}

#[test]
fn doc_comments_are_trivia_never_tokens() {
    let source = "/** docs */\n// note\n/* block */\nclass C {}\n";
    let scan = lexer::scan(source);
    assert!(scan.tokens.iter().all(|t| t.kind != tk::DOC_COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::DOC_COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::COMMENT));
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

#[test]
fn param_families_shape() {
    let source = "function f(a: number, b?: string, c = 1, ...rest: boolean[]) {}\n";
    let tree = parse(source).expect("tree");
    let params = find_kind(&tree, nk::PARAM_LIST).expect("params");
    assert_eq!(count_kind(params, nk::PARAM), 4, "all param flavors");
    assert!(contains_kind(params, nk::TYPE_ANNOTATION));
}

#[test]
fn declaration_families_present() {
    let source = "import x from \"m\";\nexport interface I { x: number }\ntype T = string;\nclass C {}\nenum E { A }\nfunction f() {}\nconst v = 1;\n";
    let tree = parse(source).expect("tree");
    for kind in [
        nk::IMPORT_DECL,
        nk::EXPORT_DECL,
        nk::INTERFACE_DECL,
        nk::TYPE_ALIAS,
        nk::CLASS_DECL,
        nk::ENUM_DECL,
        nk::FUNC_DECL,
        nk::VAR_DECL,
    ] {
        assert!(contains_kind(&tree, kind), "missing node kind {kind}");
    }
}

#[test]
fn recovery_progresses_and_stays_lossless() {
    let source = "x = ;\nfunction f( {\nlet = 3;\n";
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
    let (events, _, errors) = parse_events(source);
    assert!(errors >= 1, "broken lines report errors");
    assert!(events.iter().any(|e| matches!(e, Event::Error { .. })));
    // Deterministic.
    let again = parse_events(source);
    assert_eq!((events, errors), (again.0, again.2));
}

/// Count nodes of `kind` in the tree.
fn count_kind(tree: &GreenNode, kind: u16) -> usize {
    fn walk(node: &GreenNode, kind: u16, out: &mut usize) {
        if node.kind == kind {
            *out += 1;
        }
        for child in &node.children {
            if let GreenChild::Node(sub) = child {
                walk(sub, kind, out);
            }
        }
    }
    let mut out = 0;
    walk(tree, kind, &mut out);
    out
}

/// Find the first node of `kind`.
fn find_kind(tree: &GreenNode, kind: u16) -> Option<&GreenNode> {
    if tree.kind == kind {
        return Some(tree);
    }
    tree.children.iter().find_map(|child| match child {
        GreenChild::Node(sub) => find_kind(sub, kind),
        GreenChild::Token { .. } => None,
    })
}

/// Whether the subtree contains a node of `kind`.
fn contains_kind(tree: &GreenNode, kind: u16) -> bool {
    find_kind(tree, kind).is_some()
}

/// Whether the node has a direct NUMBER token child.
fn has_number_child(node: &GreenNode) -> bool {
    node.children
        .iter()
        .any(|c| matches!(c, GreenChild::Token { kind, .. } if *kind == tk::NUMBER))
}

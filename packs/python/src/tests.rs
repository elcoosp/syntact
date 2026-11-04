//! packs-python test suite (issue 028 TDD): INDENT/DEDENT stream shape,
//! f-string nesting, per-family shapes, losslessness, determinism,
//! recovery budgets — mirroring the packs-json/toml discipline.

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
    // statements
    ("pass\n", 0),
    ("x = 1\n", 0),
    ("x = 1 + 2 * 3\n", 0),
    ("return\n", 0),
    ("def f():\n    return 1\n", 0),
    ("def f(a, b=2):\n    return a + b\n", 0),
    // suites / indentation
    ("if x:\n    pass\n", 0),
    ("if x:\n    pass\nelse:\n    pass\n", 0),
    ("if a:\n    if b:\n        pass\n", 0),
    ("if a:\n    pass\nelif b:\n    pass\nelse:\n    pass\n", 0),
    ("while x:\n    break\n", 0),
    ("for i in items:\n    continue\n", 0),
    ("if x: pass\n", 0),
    // decorators
    ("@dec\n@mod.fn\ndef f():\n    pass\n", 0),
    // expressions / Pratt
    ("x = a or b and not c\n", 0),
    ("x = a == b and c in d\n", 0),
    ("x = -a + ~b * 2\n", 0),
    ("x = 2 ** 3 ** 2\n", 0),
    ("x = f(a, b).attr[0]\n", 0),
    ("x = (a, b)\n", 0),
    // lists + comprehensions
    ("x = [1, 2, 3]\n", 0),
    ("x = [e for e in items]\n", 0),
    ("x = [e for e in items if e if f]\n", 0),
    // f-strings
    ("x = f\"plain\"\n", 0),
    ("x = f\"a {b} c\"\n", 0),
    ("x = f\"a {f\"{b}\"} c\"\n", 0),
    // comments + blanks as trivia
    ("# header\n\nx = 1  # trailing\n# footer\n", 0),
    // bracket continuation
    ("x = [1,\n    2,\n    3]\n", 0),
    // malformed with budgets
    ("x = \n", 1),
    ("def f(:\n    pass\n", 4),
    ("if x\n    pass\n", 2),
    ("@nodecorator\nx = 1\n", 2),
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
fn valid_fixtures_are_error_free_and_deterministic() {
    for (source, tolerated) in CORPUS {
        if *tolerated != 0 {
            continue;
        }
        let first = parse_events(source);
        let second = parse_events(source);
        assert_eq!(first, second, "determinism on {source:?}");
        assert_eq!(first.2, 0, "error-free on {source:?}");
        // Balanced stream (wrapped root validates).
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
fn indent_dedent_stream_shape_on_nested_suites() {
    let source = "if a:\n    if b:\n        pass\n";
    let scan = lexer::scan(source);
    let kinds: Vec<u16> = scan.tokens.iter().map(|t| t.kind).collect();
    // if NEWLINE INDENT if NEWLINE INDENT pass NEWLINE DEDENT DEDENT
    assert_eq!(
        kinds,
        vec![
            tk::KW_IF,
            tk::NAME,
            tk::COLON,
            tk::NEWLINE,
            tk::INDENT,
            tk::KW_IF,
            tk::NAME,
            tk::COLON,
            tk::NEWLINE,
            tk::INDENT,
            tk::KW_PASS,
            tk::NEWLINE,
            tk::DEDENT,
            tk::DEDENT,
        ],
        "INDENT/DEDENT shape on nested suites"
    );
    // EOF flushes the levels; zero-width DEDENTs at the end.
    assert_eq!(scan.errors, 0);
}

#[test]
fn dedents_flush_one_per_level_at_eof() {
    let source = "if a:\n    if b:\n        if c:\n            pass";
    let scan = lexer::scan(source);
    let dedents = scan.tokens.iter().filter(|t| t.kind == tk::DEDENT).count();
    let indents = scan.tokens.iter().filter(|t| t.kind == tk::INDENT).count();
    assert_eq!(indents, 3);
    assert_eq!(dedents, 3, "one DEDENT per level at EOF");
}

#[test]
fn fstring_nesting_shape() {
    let source = "x = f\"a {f\"{b}\"} c\"\n";
    let scan = lexer::scan(source);
    let kinds: Vec<u16> = scan.tokens.iter().map(|t| t.kind).collect();
    // The nested `f"{b}"` has no interior text: START LBRACE NAME RBRACE END.
    assert_eq!(
        kinds,
        vec![
            tk::NAME,
            tk::ASSIGN,
            tk::FSTRING_START,
            tk::FSTRING_TEXT,
            tk::INTERP_LBRACE,
            tk::FSTRING_START,
            tk::INTERP_LBRACE,
            tk::NAME,
            tk::INTERP_RBRACE,
            tk::FSTRING_END,
            tk::INTERP_RBRACE,
            tk::FSTRING_TEXT,
            tk::FSTRING_END,
            tk::NEWLINE,
        ],
        "nested f-string token shape"
    );
    // The tree nests FSTRING inside FSTRING.
    let tree = parse(source).expect("tree");
    assert_eq!(fstrings(&tree), 2, "outer + nested f-string nodes");
}

#[test]
fn comments_and_blanks_are_trivia_never_tokens() {
    let source = "# header\n\nx = 1  # trailing\n# footer\n";
    let scan = lexer::scan(source);
    assert!(scan.tokens.iter().all(|t| t.kind != tk::COMMENT));
    // Trivia records cover comments AND whitespace.
    assert!(scan.trivia.iter().any(|(_, kind, _)| *kind == tk::COMMENT));
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

#[test]
fn bracket_continuation_suppresses_newlines() {
    let source = "x = [1,\n    2,\n    3]\n";
    let scan = lexer::scan(source);
    let newlines = scan.tokens.iter().filter(|t| t.kind == tk::NEWLINE).count();
    assert_eq!(newlines, 1, "only the final logical-line NEWLINE");
    let indents = scan.tokens.iter().filter(|t| t.kind == tk::INDENT).count();
    assert_eq!(indents, 0, "no INDENT inside brackets");
}

#[test]
fn expression_precedence_golden_shapes() {
    // `a or b and not c` — `not` grabs only `c` (comparison-level bp).
    let tree = parse("x = a or b and not c\n").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::BIN_EXPR), "binary nodes present");
    assert!(shape.contains(&nk::UNARY_EXPR), "unary node present");
    assert!(shape.contains(&nk::ASSIGN), "assignment present");

    // Call/attr/subscript chain: f(a, b).attr[0]
    let tree = parse("x = f(a, b).attr[0]\n").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CALL));
    assert!(shape.contains(&nk::ATTR));
    assert!(shape.contains(&nk::SUBSCRIPT));

    // Comprehension with two filters.
    let tree = parse("x = [e for e in items if e if f]\n").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::COMP_FOR));
    assert_eq!(shape.iter().filter(|k| **k == nk::COMP_IF).count(), 2);
}

#[test]
fn decorated_def_carries_decorators() {
    let source = "@dec\n@mod.fn\ndef f():\n    pass\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::DECORATOR).count(), 2);
    assert!(shape.contains(&nk::FUNC_HEADER));
}

#[test]
fn recovery_stays_lossless_and_progresses() {
    // Garbage lines recover to the newline without losing bytes.
    let source = "x = \n@nodecorator\nx = 1\n";
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1, "broken lines report errors");
    let tree2: Arc<GreenNode> = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        tree2.render(&Source::from_str_unchecked(source)),
        "deterministic recovery"
    );
}

/// The number of FSTRING nodes in the tree (nesting depth evidence).
fn fstrings(node: &GreenNode) -> usize {
    let own = usize::from(node.kind == nk::FSTRING);
    own + node
        .children
        .iter()
        .filter_map(|c| match c {
            GreenChild::Node(sub) => Some(fstrings(sub)),
            GreenChild::Token { .. } => None,
        })
        .sum::<usize>()
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

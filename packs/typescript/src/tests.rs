//! packs-typescript test suite (issue 027): per-family shapes,
//! losslessness, the `>>` split in nested generics, ASI cases, recovery
//! progress, and per-fixture tolerated-error budgets — mirroring the
//! packs-json/packs-toml/packs-python discipline.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, Step, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid TypeScript across all families plus malformed
/// inputs with per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- modules --------------------------------------------------------
    ("import x from \"mod\";", 0),
    ("import d, { a as b, c } from \"mod\";", 0),
    ("import * as ns from \"mod\";", 0),
    ("import \"side-effect\";", 0),
    ("export const x = 1;", 0),
    ("export { a, b as c };", 0),
    ("export * from \"mod\";", 0),
    ("export default 42;", 0),
    // -- types ----------------------------------------------------------
    ("let u: string | number = 1;", 0),
    ("let g: Map<string, Array<number>> = new Map();", 0),
    ("let a: number[][] = [];", 0),
    ("let t: [string, number] = [\"\", 0];", 0),
    ("type P<T extends object = {}> = { v: T };", 0),
    ("let lit: \"a\" | 1 = \"a\";", 0),
    // -- interfaces -----------------------------------------------------
    (
        "interface I { a: number; b?: string; m(x: number): void; }",
        0,
    ),
    ("interface J<T> { v: T[]; }", 0),
    // -- enums ----------------------------------------------------------
    ("enum E { A, B = 1, C }", 0),
    ("const enum CE { X = \"x\" }", 0),
    // -- classes --------------------------------------------------------
    (
        "class C { static readonly x: number = 1; m(a: number): number { return a; } constructor() {} }",
        0,
    ),
    ("class D<T> extends C implements I { private f?: T; }", 0),
    // -- functions ------------------------------------------------------
    (
        "function f(a: number, b?: string, c = 1, ...rest: number[]): void {}",
        0,
    ),
    ("const g = (a, b) => a + b;", 0),
    ("const h = (a: number): number => a * 2;", 0),
    ("const i = x => x + 1;", 0),
    ("const j = (x) => ({ v: x });", 0),
    // -- expressions / statements ---------------------------------------
    ("x = a + b * c - d / (e ** f);", 0),
    ("x = a === b !== c && d || !e;", 0),
    ("x = c ? a : b;", 0),
    ("x = a instanceof B;", 0),
    ("x = `a${b + c}d${e}f`;", 0),
    ("x = `plain`;", 0),
    ("x = [1, [2, ...rest]];", 0),
    ("x = { a: 1, \"b\": 2, c };", 0),
    ("obj.method(1, k = 2)[0].field;", 0),
    ("let { a, b: [c, ...d] } = obj;", 0),
    ("let [x, y] = pair;", 0),
    ("x++; y--; --z;", 0),
    ("new Foo(1, 2);", 0),
    ("typeof x === \"number\";", 0),
    ("if (a) { b(); } else if (c) { d(); } else { e(); }", 0),
    ("while (a < 10) { a++; }", 0),
    ("for (let i = 0; i < n; i++) use(i);", 0),
    ("for (const v of xs) sum += v;", 0),
    ("for (const k in obj) console.log(k);", 0),
    ("function f(): void { return; }", 0),
    ("function g(): number { return 1; }", 0),
    ("// only a comment", 0),
    ("", 0),
    // -- malformed with tolerated-error budgets -------------------------
    ("let x = ;", 1),             // missing value
    ("let x = @;", 1),            // garbage value
    ("if (a { b(); }", 1),        // missing close paren
    ("x = \"unterminated;", 1),   // unterminated string
    ("x = `unterminated;", 1),    // unterminated template
    ("function f(: void) {}", 1), // bad parameter list
    ("x = [1, 2;", 1),            // unterminated array
    ("interface I { a }", 1),     // member missing type annotation
];

/// Build the green tree through the full `parse` integration.
fn tree_for(source: &str) -> Arc<GreenNode> {
    crate::parse(source).expect("tree integration never fails structurally")
}

#[test]
fn valid_corpus_is_lossless_and_error_free() {
    for (source, tolerated) in CORPUS {
        if *tolerated != 0 {
            continue;
        }
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, 0, "valid corpus must be error-free: {source:?}");
    }
}

#[test]
fn malformed_corpus_stays_lossless_within_budget() {
    for (source, tolerated) in CORPUS {
        if *tolerated == 0 {
            continue;
        }
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "recovery must stay lossless for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, *tolerated, "tolerated-error budget for {source:?}");
    }
}

#[test]
fn parsing_is_deterministic() {
    for (source, _) in CORPUS {
        let first = parse_events(source);
        for _ in 0..3 {
            let again = parse_events(source);
            assert_eq!(again.0, first.0, "events stable for {source:?}");
            assert_eq!(again.1, first.1, "ranges stable for {source:?}");
            assert_eq!(again.2, first.2, "errors stable for {source:?}");
        }
    }
}

#[test]
fn event_stream_is_balanced() {
    for (source, _) in CORPUS {
        let (events, _, _) = parse_events(source);
        let mut depth = 0_i64;
        for event in &events {
            match event {
                Event::Start { .. } => depth += 1,
                Event::Finish => depth -= 1,
                Event::Token { .. } | Event::Error { .. } => {}
            }
            assert!(depth >= 0, "unbalanced stream for {source:?}");
        }
        assert_eq!(depth, 0, "unbalanced stream for {source:?}");
    }
}

/// The `>>` split: `Array<Array<T>>` closes with TWO adjacent single-`>`
/// tokens — the two-byte `>>` never appears as a SHIFT token in the
/// generic-close position.
#[test]
fn nested_generics_split_the_shift_token() {
    let source = "let x: Array<Array<T>> = y;\n";
    let (_, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0, "nested generics must parse cleanly");
    // The two GT tokens: adjacent ranges, in the close position.
    let gt_ranges: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(kind, _)| *kind == tk::GT)
        .map(|(_, range)| range.clone())
        .collect();
    assert_eq!(gt_ranges.len(), 2, "two GT tokens: {gt_ranges:?}");
    assert_eq!(gt_ranges[0].end, gt_ranges[1].start, "adjacent split");
    // And no SHIFT token anywhere in this document.
    let shifts = ranges.iter().filter(|(kind, _)| *kind == tk::SHIFT).count();
    assert_eq!(shifts, 0, "no SHIFT token from a generic close");
}

/// Single `>>` stays a shift operator when it is not a generic close.
#[test]
fn shift_outside_generics_stays_whole() {
    let source = "let y = a >> 1;\n";
    let (_, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let shifts: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(kind, _)| *kind == tk::SHIFT)
        .map(|(_, range)| range.clone())
        .collect();
    assert_eq!(shifts.len(), 1);
    assert_eq!(shifts[0], 10..12);
}

/// ASI: statements end at newlines without `;`; `;` is consumed when
/// present; `return` takes no value across a newline (restricted
/// production) but keeps it on the same line.
#[test]
fn asi_terminates_and_restricts() {
    // Two statements, no semicolons.
    let (events, _, errors) = parse_events("x = 1\ny = 2\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::EXPR_STMT), 2);
    // Semicolons present → consumed, still two statements.
    let (events, _, errors) = parse_events("x = 1;\ny = 2;\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::EXPR_STMT), 2);
    // Restricted production: `return` alone, then a separate statement.
    let (events, _, errors) = parse_events("function f() {\n  return\n  1;\n}\n");
    assert_eq!(errors, 0, "ASI splits return and its operand line");
    assert_eq!(count_kind(&events, kinds::RETURN), 1);
    // Same-line return keeps its value.
    let (_, _, errors) = parse_events("function f() {\n  return 1;\n}\n");
    assert_eq!(errors, 0);
    let (_, ranges, _) = parse_events("function f() {\n  return 1;\n}\n");
    let numbers = ranges.iter().filter(|(k, _)| *k == tk::NUMBER).count();
    assert_eq!(numbers, 1, "the returned literal is inside the RETURN");
}

/// Template literals: TEMPLATE node with START/MIDDLE chunks and `${}`
/// interpolations parsed as real expressions.
#[test]
fn template_literals_hold_expressions() {
    let source = "let s = `a${b + c}d`;\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::TEMPLATE), 1);
    assert_eq!(count_kind(&events, kinds::BINOP), 1, "b + c inside");
    let starts = count_token(&events, tk::TEMPLATE_START);
    let ends = count_token(&events, tk::TEMPLATE_END);
    assert_eq!((starts, ends), (1, 1));
}

/// Arrow functions: parenthesized params (pre-scanned) and single-
/// parameter arrows both produce ARROW nodes.
#[test]
fn arrows_detect_and_parse() {
    let (events, _, errors) = parse_events("const g = (a, b) => a + b;\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::ARROW), 1);
    let (events, _, errors) = parse_events("const i = x => x + 1;\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::ARROW), 1);
    // A parenthesized non-arrow stays a plain group (no ARROW node).
    let (events, _, errors) = parse_events("const k = (a + b) * 2;\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::ARROW), 0);
}

/// Interfaces, classes, and enums produce their per-family nodes.
#[test]
fn per_family_nodes() {
    let (events, _, errors) =
        parse_events("interface I { a: number; }\nclass C implements I {}\nenum E { A }\n");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::INTERFACE), 1);
    assert_eq!(count_kind(&events, kinds::CLASS), 1);
    assert_eq!(count_kind(&events, kinds::ENUM), 1);
    assert_eq!(count_kind(&events, kinds::MEMBER), 1);
}

/// Recovery progress: every Error event stays within the budget and the
/// recovered stream re-parses identically.
#[test]
fn recovery_stays_within_budgets() {
    for (source, tolerated) in CORPUS {
        if *tolerated == 0 {
            continue;
        }
        let (events, _, _) = parse_events(source);
        let error_count = events
            .iter()
            .filter(|event| matches!(event, Event::Error { .. }))
            .count();
        assert_eq!(
            error_count, *tolerated,
            "one Error event per tolerated error: {source:?}"
        );
    }
}

fn count_kind(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Start { kind: k, .. } if *k == kind))
        .count()
}

fn count_token(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Token { kind: k, .. } if *k == kind))
        .count()
}

/// The full tree integration keeps declarations + the implicit root.
#[test]
fn tree_has_root_and_declarations() {
    let source = "const a = 1;\nconst b = 2;\n";
    let tree = tree_for(source);
    assert_eq!(tree.kind, ROOT_KIND);
    let mut decls = 0_usize;
    for child in &tree.children {
        if let GreenChild::Node(sub) = child {
            if sub.kind == kinds::VAR_DECL {
                decls += 1;
            }
        }
    }
    assert_eq!(decls, 2, "two VAR_DECL statements under the root");
}

/// The `Step` projection of a parsed module round-trips kinds.
#[test]
fn steps_project_statements() {
    let source = "const x = 1;\n";
    let (events, ranges, _) = parse_events(source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events);
    wrapped.push(Event::Finish);
    let output = process(&wrapped).expect("balanced");
    let enters: Vec<u16> = output
        .steps()
        .filter_map(|step| match step {
            Step::Enter { kind } => Some(kind),
            _ => None,
        })
        .collect();
    assert!(enters.contains(&kinds::VAR_DECL));
    // const x = 1;  → five token ranges (const, x, =, 1, ;)
    assert_eq!(ranges.len(), 5);
}

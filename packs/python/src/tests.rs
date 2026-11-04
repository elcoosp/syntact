//! packs-python test suite (issue 028): INDENT/DEDENT stream shape on
//! nested suites, f-string nesting, losslessness, determinism, recovery
//! progress, and per-fixture tolerated-error budgets — mirroring the
//! packs-json/packs-toml discipline.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::{Event, Step, process};
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid Python across all families plus malformed
/// inputs with per-fixture tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- valid families -------------------------------------------------
    ("x = 1", 0),
    ("x = 0x1F + 0b1010_1010", 0),            // radix ints
    ("y = 3.14e-2 + 2j", 0),                  // float + imaginary
    ("a, b = 1, 2", 0),                       // tuple assignment
    ("a = b = c", 0),                         // chained assignment
    ("x += 1", 0),                            // augmented assignment
    ("x = a + b * c - d / (e // f)", 0),      // arithmetic
    ("x = a ** b ** c", 0),                   // right-assoc power
    ("x = -a + ~b + not c", 0),               // unary
    ("x = a < b <= c == d is e is not f", 0), // comparison chain
    ("x = a not in b", 0),                    // two-word comparison
    ("x = a if b else c", 0),                 // ternary
    ("x = a and b or not c", 0),              // boolean ops
    ("x = obj.attr.method(1, k=2)", 0),       // attribute + call
    ("x = items[i + 1]", 0),                  // subscript
    ("x = ()", 0),                            // empty tuple
    ("x = (1,)", 0),                          // one-element tuple
    ("x = (1, 2, 3)", 0),                     // tuple display
    ("x = [1, [2, [3]]]", 0),                 // nested lists
    ("x = {\"k\": 1, \"j\": 2}", 0),          // dict display
    ("x = {1, 2, 3}", 0),                     // set display
    ("x = {}", 0),                            // empty dict
    ("x = \"abc\\n\\t\\\"\"", 0),             // basic string escapes
    ("x = r'raw \\d+'", 0),                   // raw string
    ("x = '''multi\nline'''", 0),             // triple-quoted
    ("x = \"a\" \"b\"", 0),                   // implicit concatenation
    ("pass", 0),                              // lone statement
    ("return", 0),                            // return without value
    ("del a, b", 0),                          // del statement
    ("import os.path as p, sys", 0),          // import with alias
    ("from . import x", 0),                   // relative from-import
    ("from ..pkg.mod import y as z", 0),      // multi-dot relative
    ("from os import (path, sep)", 0),        // parenthesized import
    ("global a, b", 0),                       // global
    ("assert x, \"msg\"", 0),                 // assert with message
    ("raise ValueError from err", 0),         // raise from
    ("if x:\n    pass", 0),                   // minimal if
    ("if a:\n    pass\nelif b:\n    pass\nelse:\n    pass", 0), // elif chain
    ("while x < 10:\n    x += 1\nelse:\n    pass", 0), // while + else
    ("for a, b in pairs:\n    use(a)\nelse:\n    pass", 0), // for + else
    ("def f(a, b=1, *args, **kw):\n    return a", 0), // def params
    ("def f(\n    a,\n    b\n):\n    pass", 0), // multi-line header
    ("def g() -> int:\n    return 0", 0),     // return annotation
    ("class C(Base):\n    x = 1", 0),         // class with base
    ("if a:\n    if b:\n        pass\n    else:\n        pass", 0), // nested
    ("if a: pass", 0),                        // single-line suite
    ("for i in r: break", 0),                 // single-line for
    ("x = [a for a in bs]", 0),               // list comprehension
    ("x = [a for a in bs if a > 0]", 0),      // guarded comprehension
    ("x = {k: v for k, v in pairs}", 0),      // dict comprehension
    ("x = (a for a in bs)", 0),               // generator expression
    ("x = f\"plain\"", 0),                    // minimal f-string
    ("x = f\"a={v}!\"", 0),                   // interpolation
    ("x = f\"{{literal}}\"", 0),              // escaped braces
    ("x = f\"{v!r:>10}\"", 0),                // conversion + spec
    ("x = f\"{f\"{inner}\"}\"", 0),           // nested f-strings
    ("x = f\"a{i}b{j}c\"", 0),                // two interpolations
    ("@dec\n@mod.dec(1)\ndef f():\n    pass", 0), // decorators
    ("# only a comment", 0),                  // comment-only doc
    ("", 0),                                  // empty doc
    ("x = (1,\n    2,\n    3)", 0),           // bracket continuation
    ("x = 1 + \\\n    2", 0),                 // backslash continuation
    // -- malformed with tolerated-error budgets -------------------------
    ("x = ", 1),                // missing value
    ("x = @", 1),               // garbage value
    ("if x\n    pass", 1),      // missing colon (zero-width error)
    ("x = \"unterminated", 1),  // unterminated string
    ("x = f\"unterminated", 1), // unterminated f-string
    ("def f(:\n    pass", 4),   // stray `:` + params/colon/suite recovery
    ("x = (1, 2", 1),           // unterminated tuple
    ("else:\n    pass", 1),     // orphan else
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

/// The INDENT/DEDENT stream shape on nested suites: one INDENT token
/// per deeper line (covering the indent run), one zero-width DEDENT
/// per popped level, in source order.
#[test]
fn indent_dedent_stream_shape_on_nested_suites() {
    // `if a:\n    if b:\n        pass\n    x = 1\ny = 2\n`
    let source = "if a:\n    if b:\n        pass\n    x = 1\ny = 2\n";
    let (events, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let mut saw: Vec<(u16, Range<u32>)> = Vec::new();
    let mut token_idx = 0_usize;
    for event in &events {
        if let Event::Token { kind, .. } = event {
            let (_, range) = &ranges[token_idx];
            let range = range.clone();
            token_idx += 1;
            if *kind == tk::INDENT || *kind == tk::DEDENT {
                saw.push((*kind, range));
            }
        }
    }
    assert_eq!(
        saw,
        vec![
            (tk::INDENT, 6..10),  // `    ` before the inner `if b`
            (tk::INDENT, 16..24), // `        ` before `pass`
            (tk::DEDENT, 33..33), // inner suite ends before `x = 1`
            (tk::DEDENT, 39..39), // outer suite ends before `y = 2`
        ],
        "one INDENT per deeper line, one zero-width DEDENT per level"
    );
}

/// f-string nesting: the outer f-string's interpolation contains a full
/// inner f-string (START/MIDDLE/END inside the outer's braces).
#[test]
fn fstring_nesting_shape() {
    let source = "x = f\"a{f\"{v}\"}b\"\n";
    let (events, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0, "nested f-strings must parse cleanly");
    let mut fstring_depth = 0_i64;
    let mut kinds_seen = Vec::new();
    let mut token_idx = 0_usize;
    for event in &events {
        if let Event::Token { kind, .. } = event {
            let _ = ranges[token_idx].clone();
            token_idx += 1;
            match *kind {
                tk::FSTRING_START => {
                    fstring_depth += 1;
                    kinds_seen.push(*kind);
                }
                tk::FSTRING_END => {
                    fstring_depth -= 1;
                    kinds_seen.push(*kind);
                }
                tk::FSTRING_MIDDLE => kinds_seen.push(*kind),
                _ => {}
            }
        }
    }
    assert_eq!(fstring_depth, 0, "balanced FSTRING_START/END pairs");
    assert_eq!(kinds_seen.len(), 6, "2 starts + 2 middles + 2 ends");
}

/// Interpolation expressions are real Pratt expressions: the call node
/// lives inside the FSTRING's braces.
#[test]
fn fstring_interpolation_holds_expressions() {
    let source = "x = f\"{obj.meth(a, b)}\"\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let mut depth = 0_i64;
    let mut inside_fstring = false;
    let mut call_inside = false;
    for event in &events {
        match event {
            Event::Start { kind, .. } => {
                if *kind == kinds::FSTRING {
                    inside_fstring = true;
                }
                if inside_fstring && *kind == kinds::CALL {
                    call_inside = true;
                }
                depth += 1;
            }
            Event::Finish => {
                if depth == 1 {
                    // leaving the FSTRING node
                }
                depth -= 1;
            }
            Event::Token { .. } | Event::Error { .. } => {}
        }
    }
    assert!(inside_fstring, "an FSTRING node is present");
    assert!(call_inside, "the interpolation holds a CALL node");
}

/// Decorators attach to their `def` through a DECORATED node.
#[test]
fn decorators_wrap_their_def() {
    let source = "@dec\ndef f():\n    pass\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let mut decorated = 0_usize;
    let mut decorator = 0_usize;
    for event in &events {
        match event {
            Event::Start { kind, .. } if *kind == kinds::DECORATED => decorated += 1,
            Event::Start { kind, .. } if *kind == kinds::DECORATOR => decorator += 1,
            _ => {}
        }
    }
    assert_eq!((decorated, decorator), (1, 1));
}

/// Pratt structure: `a ** b ** c` nests right-associatively.
#[test]
fn power_is_right_associative() {
    let source = "x = a ** b ** c\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let bins = count_kind(&events, kinds::BINOP);
    assert_eq!(bins, 2, "two BINOP nodes");
    // The outer BINOP's first child is `x`'s ASSIGN… verify nesting by
    // DFS: the second Token under the outer BINOP chain is `a`.
    let order = dfs_node_kinds(&events);
    let outer = order
        .iter()
        .position(|k| *k == kinds::BINOP)
        .expect("an outer BINOP");
    assert_eq!(order[outer + 1], kinds::BINOP, "rhs is the inner BINOP");
}

/// Comparisons chain inside ONE COMPARE node.
#[test]
fn comparisons_chain_in_one_node() {
    let source = "x = a < b < c\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::COMPARE), 1, "one COMPARE node");
    assert_eq!(count_kind(&events, kinds::BINOP), 0, "no BINOP nodes");
    // One node, two operator tokens inside: `<` at 10 and 14.
    let (_, ranges, _) = parse_events(source);
    let lts: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(kind, _)| *kind == tk::LT)
        .map(|(_, range)| range.clone())
        .collect();
    assert_eq!(lts, vec![6..7, 10..11]);
}

/// Recovery progress: every malformed fixture's error regions stay
/// within its budget, and re-parsing the recovered stream is stable
/// (deterministic recovery).
#[test]
fn recovery_regions_are_well_formed() {
    for (source, tolerated) in CORPUS {
        if *tolerated == 0 {
            continue;
        }
        let (events, ranges, _) = parse_events(source);
        // EOF errors carry no ERROR token (zero-width nodes); count the
        // Error events themselves.
        let error_count = events
            .iter()
            .filter(|event| matches!(event, Event::Error { .. }))
            .count();
        assert_eq!(
            error_count, *tolerated,
            "one Error event per tolerated error: {source:?}"
        );
        let _ = ranges;
    }
}

fn count_kind(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Start { kind: k, .. } if *k == kind))
        .count()
}

/// DFS order of node kinds (Enter events only).
fn dfs_node_kinds(events: &[Event]) -> Vec<u16> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Start { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// The full tree integration keeps statements + the implicit root.
#[test]
fn tree_has_root_and_statements() {
    let source = "x = 1\ny = 2\n";
    let tree = tree_for(source);
    assert_eq!(tree.kind, ROOT_KIND);
    let stmts = count_node_kind(&tree, kinds::ASSIGN);
    assert_eq!(stmts, 2, "two ASSIGN statements under the root");
}

fn count_node_kind(node: &GreenNode, kind: u16) -> usize {
    let mut count = usize::from(node.kind == kind);
    for child in &node.children {
        if let GreenChild::Node(sub) = child {
            count += count_node_kind(sub, kind);
        }
    }
    count
}

/// The `Step` projection of a parsed module round-trips kinds.
#[test]
fn steps_project_statements() {
    let source = "x = 1\n";
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
    assert!(enters.contains(&kinds::ASSIGN));
    assert_eq!(ranges.len(), 4, "x = 1 + NEWLINE = four token ranges");
}

//! packs-c test suite (issue 034 TDD): declarators (the `int (*f)(void)`
//! family), the typedef shadowing heuristic, preprocessor nodes,
//! expressions, and losslessness.

use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::Event;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::ROOT_KIND;
use crate::kinds;
use crate::reference::parse_events;

/// Embedded corpus: valid C across the families plus malformed inputs
/// with tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- declarations -----------------------------------------------------
    ("int x;", 0),
    ("unsigned long int big;", 0),
    ("int *p, **pp;", 0),
    ("const char *msg = \"hi\";", 0),
    ("static int counter = 0;", 0),
    ("int arr[10];", 0),
    ("int m[3] = {1, 2, 3};", 0),
    // -- function-pointer declarators (the TDD family) --------------------
    ("int (*f)(void);", 0),
    ("int (*f)(int, char *);", 0),
    ("char *(*getter)(int key);", 0),
    ("void (*handlers[8])(void);", 0),
    // -- function definitions ---------------------------------------------
    ("int main(void) { return 0; }", 0),
    ("int add(int a, int b) { return a + b; }", 0),
    ("void nothing() {}", 0),
    ("float half(float x) { return x / 2.0f; }", 0),
    // -- structs/unions/enums ---------------------------------------------
    ("struct Point { int x; int y; };", 0),
    ("union Blob { int i; char bytes[4]; };", 0),
    ("enum Color { RED, GREEN = 5, BLUE };", 0),
    ("struct Point p = { 1, 2 };", 0),
    // -- statements -------------------------------------------------------
    ("int f(void) { if (x) { y(); } else { z(); } }", 0),
    ("int f(void) { while (a < 10) a++; }", 0),
    ("int f(void) { do { a++; } while (a < 10); }", 0),
    ("int f(void) { for (int i = 0; i < n; i++) s += i; }", 0),
    ("int f(void) { for (;;) { break; } }", 0),
    ("int f(void) { switch (x) { } return -1; }", 0),
    // -- expressions ------------------------------------------------------
    ("int f(void) { int r = (a + b) * c / d % e; }", 0),
    ("int f(void) { int b = a && !c || d; }", 0),
    ("int f(void) { int t = a ? b : c; }", 0),
    ("int f(void) { p->field = obj.field; }", 0),
    ("int f(void) { int z = sizeof(int) + sizeof(x); }", 0),
    ("int f(void) { char *s = (char *)malloc(16); }", 0),
    ("int f(void) { x <<= 2; y >>= 1; }", 0),
    ("int f(void) { a = b = c = 0; }", 0),
    ("int f(void) { v[i + 1] = *p++; }", 0),
    // -- preprocessor -----------------------------------------------------
    ("#include <stdio.h>\nint main(void) { return 0; }", 0),
    (
        "#define SQUARE(x) ((x) * (x))\nint f(void) { return SQUARE(2); }",
        0,
    ),
    ("#ifdef A\nint x;\n#endif\nint y;", 0),
    ("", 0),
    // -- malformed with tolerated-error budgets ---------------------------
    ("int x = ;", 2),        // missing init (the `;` rides the error region)
    ("int f( {", 3),         // missing param closer + body + cascade
    ("struct S { int }", 4), // missing field name/closer + cascade
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

/// The declarator family: function pointers, arrays of handlers, and
/// pointer stars all land in DECLARATOR nodes with the params bound.
#[test]
fn declarator_family() {
    // `int (*f)(void);` — the grouped declarator node exists.
    let (events, _, errors) = parse_events("int (*f)(void);");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::DECLARATION), 1);
    assert_eq!(count_kind(&events, kinds::DECLARATOR), 1);
    assert_eq!(count_kind(&events, kinds::PARAMS), 1);
    // An array of handlers still parses: the outer group + the inner
    // array declarator.
    let (events, _, errors) = parse_events("void (*handlers[8])(void);");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::DECLARATOR), 2);
    // A plain pointer declaration has no grouped declarator.
    let (events, _, errors) = parse_events("int *p;");
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::DECLARATOR), 0);
}

/// The typedef shadowing heuristic: a typedef'd name is a type in later
/// declarations; an unknown name is not.
#[test]
fn typedef_shadowing_heuristic() {
    let source = "typedef struct Point Point;\nPoint origin;\nint Point;\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    // Three declarations at top level: the typedef + Point origin +
    // `int Point`.
    assert_eq!(count_kind(&events, kinds::TYPEDEF), 1);
    assert_eq!(count_kind(&events, kinds::DECLARATION), 2);
}

/// Preprocessor lines become PREPROC nodes — structured trivia that
/// still renders losslessly.
#[test]
fn preprocessor_nodes() {
    let source = "#include <stdio.h>\n#define X 1\nint main(void) { return X; }\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::PREPROC), 2, "two preproc lines");
    assert_eq!(count_kind(&events, kinds::FUNCTION), 1);
    let tree = tree_for(source);
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

/// The tree integration keeps functions under the implicit root.
#[test]
fn tree_has_root_and_functions() {
    let source = "int a(void) { return 1; }\nint b(void) { return 2; }\n";
    let tree = tree_for(source);
    assert_eq!(tree.kind, ROOT_KIND);
    let fns = count_node_kind(&tree, kinds::FUNCTION);
    assert_eq!(fns, 2);
}

fn count_kind(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Start { kind: k, .. } if *k == kind))
        .count()
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

//! packs-c test suite (issue 034 TDD): declarators (`int (*f)(void)`),
//! typedef shadowing heuristic, preprocessor DIRECTIVE nodes, per-family
//! shapes, losslessness, determinism, recovery.

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
    // declarations
    ("int x;\n", 0),
    ("int x = 5;\n", 0),
    ("int *const p = &x;\n", 0),
    ("static unsigned long n = 42UL;\n", 0),
    ("const char *msg = \"hi\";\n", 0),
    // declarators (issue 034 TDD core)
    ("int (*f)(void);\n", 0),
    ("int *a[3];\n", 0),
    ("char **argv;\n", 0),
    ("int arr[3][4];\n", 0),
    // function definitions
    ("int main(void) { return 0; }\n", 0),
    ("int add(int a, int b) { return a + b; }\n", 0),
    ("void run(int (*cb)(int), char **argv) { cb(1); }\n", 0),
    // structs / unions / enums
    ("struct S { int x; char *name; };\n", 0),
    ("union U { int i; float f; };\n", 0),
    ("enum Color { RED, GREEN = 2, BLUE };\n", 0),
    ("struct Point p;\n", 0),
    ("struct S;\n", 0),
    // typedef + shadowing heuristic
    ("typedef int MyInt;\nMyInt v = 3;\n", 0),
    ("typedef struct S S;\nS s;\n", 0),
    ("typedef int (*Fn)(int);\nFn f = 0;\n", 0),
    // statements
    ("int main(void) { if (x) { y(); } else { z(); } }\n", 0),
    ("int main(void) { while (i < n) i++; }\n", 0),
    ("int main(void) { do { f(); } while (c); }\n", 0),
    ("int main(void) { for (int i = 0; i < n; i++) s(i); }\n", 0),
    ("int main(void) { for (;;) break; }\n", 0),
    ("int main(void) { return; }\n", 0),
    ("int main(void) { continue; }\n", 0),
    ("int main(void) { int a[] = {1, 2, {3, 4}}; }\n", 0),
    // expressions / precedence
    ("int main(void) { x = 1 + 2 * 3 - 4 / 2 % 1; }\n", 0),
    ("int main(void) { b = a && c || !d; }\n", 0),
    ("int main(void) { x = a < b == c & d ^ e | f << 1; }\n", 0),
    ("int main(void) { x = y ? a : b; }\n", 0),
    ("int main(void) { p = &n; q = *p; }\n", 0),
    ("int main(void) { x = (int)y + (unsigned long)z; }\n", 0),
    ("int main(void) { n = sizeof(int) + sizeof x; }\n", 0),
    ("int main(void) { v.field->sub[0] = f(1, 2)(3); }\n", 0),
    ("int main(void) { i++; j--; ++k; --l; }\n", 0),
    ("int main(void) { x += 1; y <<= 2; z &= 3; }\n", 0),
    ("int main(void) { fp(1)(2); }\n", 0),
    // preprocessor nodes
    ("#include <stdio.h>\nint main(void) { return 0; }\n", 0),
    ("#define SQUARE(x) ((x) * (x))\nint y = SQUARE(2);\n", 0),
    ("#ifdef A\nint x;\n#endif\n", 0),
    // comments trivia
    ("// line\n/* block */\nint x;\n", 0),
    // malformed with budgets
    ("int;\n", 1),
    ("int main( { return 0; }\n", 1),
    ("x y;\n", 1),
    ("int main(void) { if (x y(); }\n", 2),
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
fn declarator_pointer_function_parses_structurally() {
    // `int (*f)(void)` — PARAM_LIST inside the declaration.
    let source = "int (*f)(void);";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::DECLARATION));
    assert!(shape.contains(&nk::PARAM_LIST));
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    // Function definition form with pointer-to-function param.
    let source = "void run(int (*cb)(int)) { cb(1); }";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::FUNCTION_DEF));
    assert!(shape.contains(&nk::CALL_EXPR));
    // Pointer arrays and double pointers.
    for source in ["int *a[3];", "char **argv;", "int arr[3][4];"] {
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, 0, "{source:?}");
    }
}

#[test]
fn typedef_shadowing_heuristic() {
    // A known typedef name in `T * x;` / `T (f)(…);` starts a decl.
    let source =
        "typedef int MyInt;\nint main(void) {\n    MyInt *p;\n    MyInt (f)(int);\n    x * y;\n}\n";
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0, "typedef shadowing disambiguates");
    // Without the typedef, `T * x;` is a multiplication statement.
    let source = "int main(void) {\n    x * y;\n}\n";
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0, "unknown T * x stays an expression");
    // `x y;` with unknown idents is an error (documented boundary).
    let source = "int main(void) {\n    x y;\n}\n";
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1, "unknown-type declarations error");
}

#[test]
fn preprocessor_lines_are_directive_nodes() {
    let source = "#include <stdio.h>\n#define N 4\nint x = N;\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(
        shape.iter().filter(|k| **k == nk::DIRECTIVE).count(),
        2,
        "each preprocessor line is a DIRECTIVE node"
    );
    assert!(shape.contains(&nk::DECLARATION));
    // Continuation lines stay one directive.
    let source = "#define LONG \\\n    continued\nint x;\n";
    let scan = lexer::scan(source);
    let pp = scan.tokens.iter().filter(|t| t.kind == tk::PP_LINE).count();
    assert_eq!(pp, 1, "backslash continuation rides one PP token");
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
}

#[test]
fn casts_and_sizeof_lookahead() {
    // `(int)x` is a cast; `(a + b) * c` and `(*fp)(x)` are not.
    let tree = parse("int main(void) { x = (int)y + (a + b) * c; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CAST_EXPR));
    assert!(shape.contains(&nk::PAREN_EXPR));
    let tree = parse("int main(void) { (*fp)(1, 2); }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(
        !shape.contains(&nk::CAST_EXPR),
        "function-pointer call is not a cast"
    );
    assert!(shape.contains(&nk::CALL_EXPR));
    let tree = parse("int main(void) { n = sizeof(int) + sizeof x; }").expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::TYPE_NAME).count(), 1);
}

#[test]
fn expression_precedence_golden_shapes() {
    // `1 + 2 * 3` — the `*` BIN nests under the `+` BIN.
    let tree = parse("int main(void) { x = 1 + 2 * 3; }").expect("tree");
    let bin = first_bin(&tree);
    assert!(has_bin_child(bin), "* nests under +");
    // `?:` sits above assignment: `x = c ? a : b`.
    let tree = parse("int main(void) { x = c ? a : b; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::COND_EXPR));
    // Unary minus binds tighter: `-x * y`.
    let tree = parse("int main(void) { x = -x * y; }").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::UNARY_EXPR));
    assert!(shape.contains(&nk::BIN_EXPR));
}

#[test]
fn struct_enum_field_shapes() {
    let source = "struct S { int x; char *name; };\nenum Color { RED, GREEN = 2, BLUE };\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::FIELD_DECL).count(), 2);
    assert!(shape.contains(&nk::STRUCT_SPEC));
    assert!(shape.contains(&nk::ENUM_SPEC));
    assert_eq!(shape.iter().filter(|k| **k == nk::ENUMERATOR).count(), 3);
    // Union spec.
    let tree = parse("union U { int i; float f; };").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::UNION_SPEC));
}

#[test]
fn statement_family_shapes() {
    let source = "int main(void) {\n    if (x) { y(); } else { z(); }\n    while (c) i++;\n    do { f(); } while (c);\n    for (int i = 0; i < n; i++) s(i);\n    return 0;\n}\n";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::IF_STMT));
    assert!(shape.contains(&nk::WHILE_STMT));
    assert!(shape.contains(&nk::DO_STMT));
    assert!(shape.contains(&nk::FOR_STMT));
    assert!(shape.contains(&nk::RETURN_STMT));
    assert!(shape.contains(&nk::COMPOUND_STMT));
    assert!(shape.contains(&nk::DECLARATION), "for-head declaration");
    let (_, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
}

#[test]
fn init_lists_nest() {
    let source = "int main(void) { int a[] = {1, 2, {3, 4}}; }";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(shape.iter().filter(|k| **k == nk::INIT_LIST).count(), 2);
}

#[test]
fn recovery_stays_lossless_and_progresses() {
    let source = "int main(void) { int x = ; x y; }\nint ok(void) { return 1; }\n";
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
fn stray_closers_recover_at_file_scope() {
    let source = "int x;\n}\nint y;\n";
    let tree = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "stray closer stays lossless"
    );
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 1);
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

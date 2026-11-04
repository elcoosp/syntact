//! packs-sql test suite (issue 035 TDD): statement families,
//! case-insensitivity, nested subquery depth, CTEs, joins,
//! losslessness, determinism, recovery.

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
    // SELECT families
    ("SELECT * FROM t;\n", 0),
    ("SELECT id, name FROM users;\n", 0),
    ("SELECT u.id, u.name AS uname FROM users u;\n", 0),
    ("SELECT DISTINCT city FROM people WHERE age > 18;\n", 0),
    ("SELECT a, b FROM t WHERE x = 1 AND y <> 2 OR z >= 3;\n", 0),
    // joins
    ("SELECT * FROM a JOIN b ON a.id = b.id;\n", 0),
    ("SELECT * FROM a LEFT OUTER JOIN b ON a.id = b.id;\n", 0),
    (
        "SELECT * FROM a INNER JOIN b ON a.x = b.x CROSS JOIN c;\n",
        0,
    ),
    ("SELECT * FROM a, b, c WHERE a.x = b.x;\n", 0),
    // order / group / limit
    ("SELECT x FROM t GROUP BY x HAVING count(*) > 1;\n", 0),
    (
        "SELECT x FROM t ORDER BY x DESC, y ASC LIMIT 10 OFFSET 5;\n",
        0,
    ),
    ("SELECT x FROM t ORDER BY y LIMIT 2, 10;\n", 0),
    ("SELECT x FROM t UNION ALL SELECT y FROM u;\n", 0),
    // subqueries (nested depth)
    ("SELECT * FROM t WHERE id IN (SELECT id FROM u);\n", 0),
    (
        "SELECT * FROM t WHERE a IN (SELECT b FROM u WHERE c IN (SELECT d FROM v));\n",
        0,
    ),
    ("SELECT * FROM (SELECT * FROM t) sub;\n", 0),
    ("SELECT (SELECT max(x) FROM t) AS m;\n", 0),
    // CTEs
    ("WITH c AS (SELECT 1) SELECT * FROM c;\n", 0),
    (
        "WITH a AS (SELECT x FROM t), b AS (SELECT y FROM u) SELECT * FROM a, b;\n",
        0,
    ),
    // expressions
    (
        "SELECT name FROM t WHERE n BETWEEN 1 AND 9 AND s LIKE 'a%';\n",
        0,
    ),
    (
        "SELECT name FROM t WHERE n NOT BETWEEN 1 AND 9 AND s NOT LIKE 'a%';\n",
        0,
    ),
    ("SELECT name FROM t WHERE x IS NULL OR y IS NOT NULL;\n", 0),
    ("SELECT a || ' ' || b FROM t;\n", 0),
    ("SELECT -x + +y, NOT c FROM t;\n", 0),
    (
        "SELECT CASE WHEN a > 1 THEN 'hi' WHEN a > 2 THEN 'mid' ELSE 'lo' END FROM t;\n",
        0,
    ),
    ("SELECT CASE a WHEN 1 THEN 'one' ELSE '?' END FROM t;\n", 0),
    ("SELECT CAST(x AS INTEGER) FROM t;\n", 0),
    ("SELECT count(*), max(a + b * 2) FROM t;\n", 0),
    ("SELECT (a, b, c);\n", 0),
    // INSERT / UPDATE / DELETE
    ("INSERT INTO t (a, b) VALUES (1, 'x');\n", 0),
    ("INSERT INTO t VALUES (1, 2), (3, 4);\n", 0),
    ("UPDATE t SET a = 1, b = a + 2 WHERE id = 3;\n", 0),
    ("DELETE FROM t WHERE id = 1;\n", 0),
    ("DELETE FROM t;\n", 0),
    // CREATE TABLE
    (
        "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);\n",
        0,
    ),
    (
        "CREATE TABLE IF NOT EXISTS s.t (v REAL DEFAULT 0.5, c CHECK (v > 0));\n",
        0,
    ),
    (
        "CREATE TABLE t (a TEXT, b INT, PRIMARY KEY (a, b), FOREIGN KEY (b) REFERENCES u (id));\n",
        0,
    ),
    ("CREATE TABLE t (id INTEGER, UNIQUE (id));\n", 0),
    // case-insensitive keywords (issue 035 TDD)
    ("sElEcT * FrOm t WhErE x = 1;\n", 0),
    ("SeLeCt CoUnT(*) As n FrOm T;\n", 0),
    // trivia
    ("-- header\nSELECT 1; /* block\ncomment */ SELECT 2;\n", 0),
    // malformed with budgets
    ("SELECT FROM;\n", 2),
    ("INSERT INTO;\n", 2),
    ("SELECT * FROM;\n", 1),
    ("UPDATE SET x = 1;\n", 1),
    ("CREATE TABLE (x INT);\n", 3),
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
fn keywords_are_case_insensitive() {
    let scan = lexer::scan("SeLeCt x FrOm t");
    assert_eq!(scan.tokens[0].kind, tk::KW_SELECT);
    assert_eq!(scan.tokens[2].kind, tk::KW_FROM);
    assert_eq!(scan.tokens[3].kind, tk::IDENT, "table names stay idents");
    assert_eq!(scan.errors, 0);
    // Identifiers keep their original case in the range.
    let scan = lexer::scan("SELECT ColumnName FROM t");
    assert_eq!(
        &scan.tokens[1].range,
        &(7..17),
        "ident range covers original spelling"
    );
}

#[test]
fn nested_subquery_depth() {
    let source = "SELECT * FROM t WHERE a IN (SELECT b FROM u WHERE c IN (SELECT d FROM v WHERE e IN (SELECT f FROM w)));";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert_eq!(
        shape.iter().filter(|k| **k == nk::SUBQUERY).count(),
        3,
        "three levels of nested subqueries"
    );
    assert_eq!(
        shape.iter().filter(|k| **k == nk::SELECT_STMT).count(),
        4,
        "outer + three nested selects"
    );
}

#[test]
fn cte_and_union_shapes() {
    let source = "WITH a AS (SELECT x FROM t), b AS (SELECT y FROM u) SELECT * FROM a UNION ALL SELECT * FROM b;";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::WITH_CLAUSE));
    assert_eq!(shape.iter().filter(|k| **k == nk::CTE).count(), 2);
    assert_eq!(
        shape.iter().filter(|k| **k == nk::SELECT_STMT).count(),
        4,
        "two CTE selects + main + the union body"
    );
}

#[test]
fn join_chain_shapes() {
    let source = "SELECT * FROM a LEFT OUTER JOIN b ON a.x = b.x INNER JOIN c ON b.y = c.y, d;";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::FROM_CLAUSE));
    assert_eq!(shape.iter().filter(|k| **k == nk::JOIN_CLAUSE).count(), 2);
    assert!(!shape.contains(&nk::WHERE_CLAUSE), "no where here");
}

#[test]
fn statement_family_shapes() {
    let sel = parse("SELECT a FROM t WHERE b = 1 GROUP BY a ORDER BY a LIMIT 1;").expect("tree");
    let shape = node_kinds(&sel);
    assert!(shape.contains(&nk::WHERE_CLAUSE));
    assert!(shape.contains(&nk::GROUP_BY_CLAUSE));
    assert!(shape.contains(&nk::ORDER_BY_CLAUSE));
    assert!(shape.contains(&nk::LIMIT_CLAUSE));

    let ins = parse("INSERT INTO t (a, b) VALUES (1, 2), (3, 4);").expect("tree");
    let shape = node_kinds(&ins);
    assert!(shape.contains(&nk::INSERT_STMT));

    let upd = parse("UPDATE t SET a = 1, b = 2 WHERE id = 3;").expect("tree");
    let shape = node_kinds(&upd);
    assert!(shape.contains(&nk::UPDATE_STMT));
    assert_eq!(shape.iter().filter(|k| **k == nk::SET_ASSIGN).count(), 2);

    let del = parse("DELETE FROM t WHERE id = 1;").expect("tree");
    let shape = node_kinds(&del);
    assert!(shape.contains(&nk::DELETE_STMT));
}

#[test]
fn create_table_shapes() {
    let source = "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, v REAL DEFAULT 0.5, CHECK (v > 0), UNIQUE (id));";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CREATE_TABLE));
    assert_eq!(shape.iter().filter(|k| **k == nk::COLUMN_DEF).count(), 3);
    assert_eq!(
        shape.iter().filter(|k| **k == nk::TABLE_CONSTRAINT).count(),
        2
    );
}

#[test]
fn case_when_shapes() {
    let source = "SELECT CASE WHEN a > 1 THEN 'hi' WHEN a > 2 THEN 'mid' ELSE 'lo' END FROM t;";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CASE_EXPR));
    assert_eq!(shape.iter().filter(|k| **k == nk::WHEN_ARM).count(), 2);
    // Operand form.
    let source = "SELECT CASE a WHEN 1 THEN 'one' END FROM t;";
    let tree = parse(source).expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::CASE_EXPR));
}

#[test]
fn comments_are_trivia_never_tokens() {
    let source = "-- header\nSELECT 1 /* mid */ , 2;\n";
    let scan = lexer::scan(source);
    assert!(scan.tokens.iter().all(|t| t.kind != tk::COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::COMMENT));
    assert!(scan.trivia.iter().any(|(_, k, _)| *k == tk::BLOCK_COMMENT));
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

#[test]
fn precedence_golden_shapes() {
    // `1 + 2 * 3` — `*` nests under `+`.
    let tree = parse("SELECT 1 + 2 * 3;").expect("tree");
    let bin = first_bin(&tree);
    assert!(has_bin_child(bin));
    // `a = 1 AND b = 2` — the AND wraps the second comparison.
    let tree = parse("SELECT * FROM t WHERE a = 1 AND b = 2;").expect("tree");
    assert!(bins(&tree).iter().any(|b| has_bin_child(b)), "AND wraps =");
    // NOT binds above AND: `NOT a AND b`.
    let tree = parse("SELECT * FROM t WHERE NOT a AND b;").expect("tree");
    let shape = node_kinds(&tree);
    assert!(shape.contains(&nk::UNARY_EXPR));
    // BETWEEN consumes the joining AND.
    let (_, _, errors) = parse_events("SELECT * FROM t WHERE x BETWEEN 1 AND 9 AND y = 2;");
    assert_eq!(errors, 0);
}

#[test]
fn recovery_stays_lossless_and_progresses() {
    let source = "SELECT FROM;\nINSERT INTO;\nSELECT 1;\n";
    let tree = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "recovered regions keep every byte"
    );
    let (_, _, errors) = parse_events(source);
    assert!(errors >= 2, "both broken statements report errors");
    let tree2: Arc<GreenNode> = parse(source).expect("tree");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        tree2.render(&Source::from_str_unchecked(source)),
        "deterministic recovery"
    );
}

#[test]
fn doubled_quote_escapes_in_strings() {
    let source = "SELECT 'it''s ok' FROM t;";
    let scan = lexer::scan(source);
    assert_eq!(scan.tokens[1].kind, tk::STRING_LIT);
    assert_eq!(scan.errors, 0);
    let tree = parse(source).expect("tree");
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
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

/// Every `BIN_EXPR` node in the tree.
fn bins(tree: &GreenNode) -> Vec<&GreenNode> {
    fn walk<'a>(node: &'a GreenNode, out: &mut Vec<&'a GreenNode>) {
        if node.kind == nk::BIN_EXPR {
            out.push(node);
        }
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

/// Whether the node directly contains a nested `BIN_EXPR` child.
fn has_bin_child(node: &GreenNode) -> bool {
    node.children
        .iter()
        .any(|c| matches!(c, GreenChild::Node(sub) if sub.kind == nk::BIN_EXPR))
}

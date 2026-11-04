//! packs-sql test suite (issue 035 TDD): per-statement-family tests,
//! case-insensitivity, nested subquery depth, and losslessness.

use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::Event;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::ROOT_KIND;
use crate::kinds;
use crate::reference::parse_events;

/// Embedded corpus: valid SQL across the families plus malformed inputs
/// with tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- SELECT family ----------------------------------------------------
    ("SELECT * FROM users;", 0),
    ("select id, name from users;", 0),
    ("SELECT u.id, u.name FROM users AS u;", 0),
    ("SELECT COUNT(*) FROM t;", 0),
    ("SELECT DISTINCT a, b FROM t WHERE x = 1;", 0),
    ("SELECT a FROM t JOIN u ON t.id = u.id;", 0),
    ("SELECT a FROM t LEFT JOIN u ON t.id = u.tid;", 0),
    ("SELECT a FROM t WHERE x > 5 AND y LIKE 'a%';", 0),
    ("SELECT a FROM t GROUP BY a HAVING COUNT(*) > 1;", 0),
    ("SELECT a FROM t ORDER BY a DESC, b LIMIT 10;", 0),
    (
        "SELECT CASE WHEN x > 0 THEN 'pos' ELSE 'neg' END FROM t;",
        0,
    ),
    ("SELECT CASE x WHEN 1 THEN 'one' END FROM t;", 0),
    ("SELECT a IN (1, 2, 3) FROM t;", 0),
    ("SELECT a WHERE b IS NOT NULL;", 0),
    ("SELECT a BETWEEN 1 AND 10 FROM t;", 0),
    // -- subqueries ---------------------------------------------------------
    ("SELECT a FROM (SELECT x FROM t) AS sub;", 0),
    ("SELECT a FROM t WHERE id IN (SELECT id FROM u);", 0),
    ("SELECT (SELECT MAX(x) FROM t) AS m;", 0),
    ("SELECT (SELECT (SELECT 1)) AS deep;", 0),
    // -- CTEs ---------------------------------------------------------------
    ("WITH c AS (SELECT x FROM t) SELECT * FROM c;", 0),
    (
        "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a, b;",
        0,
    ),
    // -- INSERT/UPDATE/DELETE ------------------------------------------------
    ("INSERT INTO t VALUES (1, 'x');", 0),
    ("INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y');", 0),
    ("UPDATE t SET a = 1, b = 'z' WHERE id = 3;", 0),
    ("DELETE FROM t WHERE id = 3;", 0),
    // -- CREATE TABLE ---------------------------------------------------------
    (
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
        0,
    ),
    ("CREATE TABLE t (a VARCHAR(10), b DECIMAL(8, 2));", 0),
    // -- misc ------------------------------------------------------------------
    ("-- only a comment", 0),
    ("/* block */", 0),
    ("", 0),
    // -- malformed with tolerated-error budgets ---------------------------------
    ("SELECT FROM t;", 1),        // missing columns
    ("INSERT INTO t VALUES;", 1), // missing values row
    ("CREATE TABL t (a);", 2),    // typo.d keyword + cascade
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

/// Case-insensitivity: lowercase, uppercase, and mixed casing all
/// produce the same statement node counts (raw text preserved).
#[test]
fn keywords_are_case_insensitive() {
    let lower = parse_events("select a from t where b = 1;");
    let upper = parse_events("SELECT A FROM t WHERE B = 1;");
    let mixed = parse_events("SeLeCt a FrOm t WhErE b = 1;");
    assert_eq!(count_kind(&lower.0, kinds::SELECT), 1, "SELECT in lower");
    assert_eq!(count_kind(&upper.0, kinds::SELECT), 1, "SELECT in upper");
    assert_eq!(count_kind(&mixed.0, kinds::SELECT), 1, "SELECT in mixed");
    // One clause each (WHERE — FROM is a source, not a clause node).
    assert_eq!(count_kind(&lower.0, kinds::CLAUSE), 1, "clauses in lower");
    assert_eq!(count_kind(&upper.0, kinds::CLAUSE), 1, "clauses in upper");
    assert_eq!(count_kind(&mixed.0, kinds::CLAUSE), 1, "clauses in mixed");
    // The raw text survives (lossless, casing intact).
    assert_eq!(upper.1.len(), lower.1.len());
}

/// Subqueries nest to arbitrary depth and stay balanced.
#[test]
fn nested_subquery_depth() {
    let source = "SELECT a FROM t WHERE id IN \
                  (SELECT id FROM u WHERE p IN \
                   (SELECT p FROM v WHERE q IN (SELECT q FROM w)));";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0, "three levels of subqueries");
    assert_eq!(count_kind(&events, kinds::SUBQUERY), 3, "3 SUBQUERY nodes");
}

/// Per-family node counts.
#[test]
fn per_family_nodes() {
    let source = "INSERT INTO t VALUES (1);\nUPDATE t SET a = 1;\nDELETE FROM t;\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::INSERT), 1);
    assert_eq!(count_kind(&events, kinds::UPDATE), 1);
    assert_eq!(count_kind(&events, kinds::DELETE), 1);
}

/// `CREATE TABLE` with column constraints parses into `COLUMN_DEF`s.
#[test]
fn create_table_family() {
    let source = "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::CREATE_TABLE), 1);
    assert_eq!(count_kind(&events, kinds::COLUMN_DEF), 2);
}

/// CTEs parse with their SELECT bodies.
#[test]
fn cte_family() {
    let source = "WITH c AS (SELECT x FROM t) SELECT * FROM c;";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::CTE), 1);
    assert_eq!(count_kind(&events, kinds::SELECT), 2, "CTE body + main");
}

/// The tree integration keeps statements under the implicit root.
#[test]
fn tree_has_root_and_statements() {
    let source = "SELECT 1;\nSELECT 2;\n";
    let tree = tree_for(source);
    assert_eq!(tree.kind, ROOT_KIND);
    let stmts = count_node_kind(&tree, kinds::STATEMENT);
    assert_eq!(stmts, 2);
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

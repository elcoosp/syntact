//! # packs-sql — the SQL grammar pack (issue 035, documented subset)
//!
//! Reference parser for a SQLite-dialect subset on the runtime-core
//! event protocol.
//!
//! **The subset** (everything else errors and recovers):
//! - `SELECT` with result columns (`*`, `t.*`, `e AS a`), `FROM`
//!   with comma and `[LEFT|INNER|CROSS [OUTER]] JOIN … ON` chains,
//!   subqueries in FROM, `WHERE`, `GROUP BY`/`HAVING`, `ORDER BY`
//!   (`ASC`/`DESC`), `LIMIT`/`OFFSET`, `UNION [ALL]` chains;
//! - CTEs: `WITH name AS (…), …` (no `RECURSIVE` — documented);
//! - `INSERT INTO t (cols) VALUES (…), …`, `UPDATE t SET c = e, …
//!   [WHERE …]`, `DELETE FROM t [WHERE …]`;
//! - `CREATE TABLE [IF NOT EXISTS] t (column defs, table
//!   constraints)` — types as token soup, column constraints
//!   (`PRIMARY KEY [AUTOINCREMENT]`, `NOT NULL`, `UNIQUE`,
//!   `DEFAULT`, `CHECK`, `REFERENCES`), table constraints;
//! - expressions: `OR AND = != <> < <= > >= IS [NOT] IN [NOT]
//!   LIKE GLOB [NOT] BETWEEN || + - * / %`, unary `- + NOT`, `CASE
//!   [e] WHEN … THEN … [ELSE …] END`, `CAST(e AS T)`, function
//!   calls, `(subquery)`, `(expr, …)`, literals, dotted names;
//! - `--` line and `/* */` block trivia, **case-insensitive
//!   keywords** (`SeLeCt` lexes as `SELECT`).
//!
//! **Boundaries** (documented): no window functions, no `RETURNING`,
//! no `RECURSIVE` CTEs, no `INSERT … SELECT`, no `PRAGMA`, no
//! qualified `db.table` beyond one dot, no `NULLS FIRST/LAST`.

pub mod lexer;
pub mod reference;

/// Node kinds for the SQL grammar (dense).
pub mod kinds {
    /// `SELECT …` (incl. `UNION [ALL]` chains).
    pub const SELECT_STMT: u16 = 0;
    /// `FROM …` with table refs and join chains.
    pub const FROM_CLAUSE: u16 = 1;
    /// One `[LEFT|INNER|CROSS [OUTER]] JOIN t ON e` step.
    pub const JOIN_CLAUSE: u16 = 2;
    /// `WHERE e`.
    pub const WHERE_CLAUSE: u16 = 3;
    /// `GROUP BY e, … [HAVING e]`.
    pub const GROUP_BY_CLAUSE: u16 = 4;
    /// `ORDER BY e [ASC|DESC], …`.
    pub const ORDER_BY_CLAUSE: u16 = 5;
    /// `LIMIT e [OFFSET e | , e]`.
    pub const LIMIT_CLAUSE: u16 = 6;
    /// `WITH name AS (…), …`.
    pub const WITH_CLAUSE: u16 = 7;
    /// One CTE `name AS (select)`.
    pub const CTE: u16 = 8;
    /// One result column (`*`, `t.*`, `e AS a`).
    pub const RESULT_COLUMN: u16 = 9;
    /// `( SELECT … )`.
    pub const SUBQUERY: u16 = 10;
    /// `INSERT INTO t (…) VALUES (…), …`.
    pub const INSERT_STMT: u16 = 11;
    /// `UPDATE t SET … [WHERE …]`.
    pub const UPDATE_STMT: u16 = 12;
    /// One `c = e` assignment.
    pub const SET_ASSIGN: u16 = 13;
    /// `DELETE FROM t [WHERE …]`.
    pub const DELETE_STMT: u16 = 14;
    /// `CREATE TABLE [IF NOT EXISTS] t (…)`.
    pub const CREATE_TABLE: u16 = 15;
    /// One column definition `name type constraints`.
    pub const COLUMN_DEF: u16 = 16;
    /// One table constraint.
    pub const TABLE_CONSTRAINT: u16 = 17;
    /// `CASE [e] WHEN … THEN … [ELSE …] END`.
    pub const CASE_EXPR: u16 = 18;
    /// One `WHEN e THEN e` arm.
    pub const WHEN_ARM: u16 = 19;
    /// Binary operation (op kind in the child token).
    pub const BIN_EXPR: u16 = 20;
    /// Unary `- + NOT`.
    pub const UNARY_EXPR: u16 = 21;
    /// `f(args)` / `count(*)`.
    pub const CALL_EXPR: u16 = 22;
    /// `(e, e, …)` list / grouped expr.
    pub const EXPR_LIST: u16 = 23;
    /// `CAST(e AS T)`.
    pub const CAST_EXPR: u16 = 24;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// Identifier (keywords lex separately, case-insensitively).
    pub const IDENT: u16 = 25;
    /// Integer literal.
    pub const INT_LIT: u16 = 26;
    /// Float literal (`1.5`, `1e3`).
    pub const FLOAT_LIT: u16 = 27;
    /// `'…'` string literal (doubled-quote escapes).
    pub const STRING_LIT: u16 = 28;
    /// `*` (wildcard or multiply).
    pub const STAR: u16 = 29;
    /// `+`
    pub const PLUS: u16 = 30;
    /// `-`
    pub const MINUS: u16 = 31;
    /// `/`
    pub const SLASH: u16 = 32;
    /// `%`
    pub const PERCENT: u16 = 33;
    /// `=`
    pub const EQ: u16 = 34;
    /// `!=` / `<>`
    pub const NE: u16 = 35;
    /// `<`
    pub const LT: u16 = 36;
    /// `>`
    pub const GT: u16 = 37;
    /// `<=`
    pub const LE: u16 = 38;
    /// `>=`
    pub const GE: u16 = 39;
    /// `,`
    pub const COMMA: u16 = 40;
    /// `;`
    pub const SEMI: u16 = 41;
    /// `(`
    pub const LPAREN: u16 = 42;
    /// `)`
    pub const RPAREN: u16 = 43;
    /// `.` (qualified `t.c` / `t.*`)
    pub const DOT: u16 = 44;
    /// `||` (string concat)
    pub const CONCAT: u16 = 45;
    /// `SELECT`
    pub const KW_SELECT: u16 = 46;
    /// `FROM`
    pub const KW_FROM: u16 = 47;
    /// `WHERE`
    pub const KW_WHERE: u16 = 48;
    /// `GROUP`
    pub const KW_GROUP: u16 = 49;
    /// `BY`
    pub const KW_BY: u16 = 50;
    /// `ORDER`
    pub const KW_ORDER: u16 = 51;
    /// `LIMIT`
    pub const KW_LIMIT: u16 = 52;
    /// `OFFSET`
    pub const KW_OFFSET: u16 = 53;
    /// `HAVING`
    pub const KW_HAVING: u16 = 54;
    /// `WITH`
    pub const KW_WITH: u16 = 55;
    /// `AS`
    pub const KW_AS: u16 = 56;
    /// `ON`
    pub const KW_ON: u16 = 57;
    /// `JOIN`
    pub const KW_JOIN: u16 = 58;
    /// `LEFT`
    pub const KW_LEFT: u16 = 59;
    /// `RIGHT`
    pub const KW_RIGHT: u16 = 60;
    /// `INNER`
    pub const KW_INNER: u16 = 61;
    /// `OUTER`
    pub const KW_OUTER: u16 = 62;
    /// `CROSS`
    pub const KW_CROSS: u16 = 63;
    /// `AND`
    pub const KW_AND: u16 = 64;
    /// `OR`
    pub const KW_OR: u16 = 65;
    /// `NOT`
    pub const KW_NOT: u16 = 66;
    /// `NULL`
    pub const KW_NULL: u16 = 67;
    /// `IS`
    pub const KW_IS: u16 = 68;
    /// `IN`
    pub const KW_IN: u16 = 69;
    /// `LIKE`
    pub const KW_LIKE: u16 = 70;
    /// `BETWEEN`
    pub const KW_BETWEEN: u16 = 71;
    /// `CASE`
    pub const KW_CASE: u16 = 72;
    /// `WHEN`
    pub const KW_WHEN: u16 = 73;
    /// `THEN`
    pub const KW_THEN: u16 = 74;
    /// `ELSE`
    pub const KW_ELSE: u16 = 75;
    /// `END`
    pub const KW_END: u16 = 76;
    /// `INSERT`
    pub const KW_INSERT: u16 = 77;
    /// `INTO`
    pub const KW_INTO: u16 = 78;
    /// `VALUES`
    pub const KW_VALUES: u16 = 79;
    /// `UPDATE`
    pub const KW_UPDATE: u16 = 80;
    /// `SET`
    pub const KW_SET: u16 = 81;
    /// `DELETE`
    pub const KW_DELETE: u16 = 82;
    /// `CREATE`
    pub const KW_CREATE: u16 = 83;
    /// `TABLE`
    pub const KW_TABLE: u16 = 84;
    /// `PRIMARY`
    pub const KW_PRIMARY: u16 = 85;
    /// `KEY`
    pub const KW_KEY: u16 = 86;
    /// `FOREIGN`
    pub const KW_FOREIGN: u16 = 87;
    /// `REFERENCES`
    pub const KW_REFERENCES: u16 = 88;
    /// `UNIQUE`
    pub const KW_UNIQUE: u16 = 89;
    /// `CHECK`
    pub const KW_CHECK: u16 = 90;
    /// `DEFAULT`
    pub const KW_DEFAULT: u16 = 91;
    /// `CAST`
    pub const KW_CAST: u16 = 92;
    /// `DISTINCT`
    pub const KW_DISTINCT: u16 = 93;
    /// `UNION`
    pub const KW_UNION: u16 = 94;
    /// `ALL`
    pub const KW_ALL: u16 = 95;
    /// `ASC`
    pub const KW_ASC: u16 = 96;
    /// `DESC`
    pub const KW_DESC: u16 = 97;
    /// `AUTOINCREMENT`
    pub const KW_AUTOINCREMENT: u16 = 98;
    /// `IF` (`IF NOT EXISTS`)
    pub const KW_IF: u16 = 99;
    /// `GLOB`
    pub const KW_GLOB: u16 = 100;
    /// `EXISTS`
    pub const KW_EXISTS: u16 = 101;
    /// Whitespace trivia (never a token event, ADR-008).
    pub const WHITESPACE: u16 = 102;
    /// `-- …` line-comment trivia.
    pub const COMMENT: u16 = 103;
    /// `/* … */` block-comment trivia.
    pub const BLOCK_COMMENT: u16 = 104;
}

/// Implicit document-root kind — pack-local sentinel below the
/// runtime-core sentinels.
pub const ROOT_KIND: u16 = u16::MAX - 10;

/// Parse SQL source into a green tree (the full integration).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

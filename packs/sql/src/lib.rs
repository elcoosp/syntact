//! # packs-sql — the SQL (SQLite-dialect subset) grammar pack (issue 035)
//!
//! Reference parser on the runtime-core event protocol: `SELECT` with
//! joins, subqueries and CTEs, `INSERT`/`UPDATE`/`DELETE`, `CREATE
//! TABLE` with constraints, scalar expressions, `CASE`, literals,
//! `--`/`/* */` trivia, and case-insensitive keywords.
//!
//! # Documented subset (v0)
//!
//! - **Statements**: `SELECT [DISTINCT] cols FROM … [JOIN … ON …]
//!   [WHERE …] [GROUP BY …] [HAVING …] [ORDER BY … [ASC|DESC]]
//!   [LIMIT …]`; `INSERT INTO t [(cols)] VALUES (…), …`; `UPDATE t SET
//!   c = e, … [WHERE …]`; `DELETE FROM t [WHERE …]`; `CREATE TABLE t
//!   (col constraints…, …)`; `WITH name AS (…), …` prefix (CTEs).
//! - **Expressions**: precedence climbing — `OR`, `AND`, `NOT`,
//!   comparisons (`=`, `!=`, `<>`, `<`, `<=`, `>`, `>=`, `IN`, `LIKE`,
//!   `IS [NOT] NULL`, `BETWEEN`), additive, multiplicative, unary
//!   `-`/`+`/`NOT`, postfix function calls `f(args)`, `.column`,
//!   `*`/`table.*`, literals (`'…'` strings with `''` escapes,
//!   numbers, `NULL`, `TRUE`/`FALSE`), `CASE [expr] WHEN … THEN …
//!   [ELSE …] END`, parenthesised subqueries `(SELECT …)`.
//! - **Case-insensitivity**: every keyword matches any casing
//!   (`select`, `SELECT`, `SeLeCt`) — the scanner emits the raw text
//!   and the parser compares case-folded.
//! - **Trivia**: `--` line comments, `/* … */` block comments, all
//!   whitespace — never token events, `render(parse(x)) == x`.
//! - Nested subqueries recurse through the full statement grammar at
//!   any expression depth.
//!
//! Kind numbering (documented mapping, mirroring the other packs):
//! node kinds in [`kinds`], leaf tokens + punctuation in
//! [`token_kinds`] (dense continuation), offending regions via
//! [`runtime_core::ERROR_KIND`], implicit root [`ROOT_KIND`].

pub mod reference;

/// Node kinds for the SQL-subset grammar (dense).
pub mod kinds {
    /// Any statement (`SELECT`/`INSERT`/`UPDATE`/`DELETE`/`CREATE`).
    pub const STATEMENT: u16 = 0;
    /// A `WITH name AS (…) …` prefix.
    pub const CTE: u16 = 1;
    /// The `SELECT` core (columns + clauses).
    pub const SELECT: u16 = 2;
    /// A selected item (expr [`AS` alias] — the `AS` word is optional).
    pub const SELECT_ITEM: u16 = 3;
    /// `FROM`/`JOIN` table source.
    pub const SOURCE: u16 = 4;
    /// A generic clause keyword region (WHERE/GROUP BY/…).
    pub const CLAUSE: u16 = 5;
    /// `INSERT INTO … VALUES …`.
    pub const INSERT: u16 = 6;
    /// `UPDATE … SET …`.
    pub const UPDATE: u16 = 7;
    /// `DELETE FROM …`.
    pub const DELETE: u16 = 8;
    /// `CREATE TABLE … (…)`.
    pub const CREATE_TABLE: u16 = 9;
    /// One column definition / table constraint.
    pub const COLUMN_DEF: u16 = 10;
    /// `col AS alias` / `col alias` alias binding.
    pub const ALIAS: u16 = 11;
    /// `CASE … END`.
    pub const CASE: u16 = 12;
    /// One `WHEN cond THEN result`.
    pub const WHEN: u16 = 13;
    /// A subquery `(SELECT …)`.
    pub const SUBQUERY: u16 = 14;
    /// A function call `f(args)`.
    pub const CALL: u16 = 15;
    /// Binary operation.
    pub const BINOP: u16 = 16;
    /// Unary `-`/`+`/`NOT`.
    pub const UNARY: u16 = 17;
    /// A parenthesised expression group.
    pub const GROUP: u16 = 18;
    /// `table.*` / bare `*` star.
    pub const STAR: u16 = 19;
    /// `t.c` qualified column.
    pub const QUALIFIED: u16 = 20;
    /// An `IN (…)` / `IN (SELECT …)` list.
    pub const IN_LIST: u16 = 21;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// An identifier or case-folded keyword (raw text preserved).
    pub const WORD: u16 = 22;
    /// A number literal.
    pub const NUMBER: u16 = 23;
    /// A `'…'` string literal (with `''` escapes).
    pub const STRING: u16 = 24;
    /// A `[bracket]` identifier.
    pub const BRACKET_IDENT: u16 = 25;
    /// `(`
    pub const LPAREN: u16 = 26;
    /// `)`
    pub const RPAREN: u16 = 27;
    /// `,`
    pub const COMMA: u16 = 28;
    /// `;`
    pub const SEMI: u16 = 29;
    /// `.`
    pub const DOT: u16 = 30;
    /// `*`
    pub const STAR: u16 = 31;
    /// `=`
    pub const EQUALS: u16 = 32;
    /// `!=`
    pub const NEQ: u16 = 33;
    /// `<>`
    pub const NEQ2: u16 = 34;
    /// `<`
    pub const LT: u16 = 35;
    /// `<=`
    pub const LE: u16 = 36;
    /// `>`
    pub const GT: u16 = 37;
    /// `>=`
    pub const GE: u16 = 38;
    /// `+`
    pub const PLUS: u16 = 39;
    /// `-`
    pub const MINUS: u16 = 40;
    /// `/`
    pub const SLASH: u16 = 41;
    /// `%`
    pub const PERCENT: u16 = 42;
    /// `||` string concat.
    pub const CONCAT: u16 = 43;
    /// Whitespace trivia.
    pub const WHITESPACE: u16 = 44;
    /// `-- …` line-comment trivia.
    pub const COMMENT: u16 = 45;
    /// `/* … */` block-comment trivia.
    pub const BLOCK_COMMENT: u16 = 46;
}

/// Implicit statement-list root kind — pack-local sentinel.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse SQL-subset source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *SQL* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

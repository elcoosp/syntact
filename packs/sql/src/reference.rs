//! Hand-written reference parser for the documented SQLite subset
//! (issue 035): SELECT with joins/subqueries/CTEs, INSERT/UPDATE/
//! DELETE, CREATE TABLE with constraints, CASE/CAST expressions.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::ERROR_KIND;
use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

use crate::kinds as nk;
use crate::lexer::{self, ScanToken};
use crate::token_kinds as tk;

/// Diagnostic side-table index meaning "syntax error".
pub const DIAG_SYNTAX: u32 = 0;

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse SQL source into the raw event stream:
/// `(events, token_ranges, error_count)`.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_script();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse SQL source into a green tree (one implicit root).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_script();
    let mut wrapped = Vec::with_capacity(parser.events.len() + 2);
    wrapped.push(Event::Start {
        kind: crate::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(parser.events.iter().copied());
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped)?;
    let input = Source::from_str_unchecked(source);
    #[allow(clippy::arc_with_non_send_sync)]
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &input);
    let records: Vec<TriviaRecord> = parser
        .trivia
        .iter()
        .map(|(range, kind, trailing)| TriviaRecord {
            kind: *kind,
            range: range.clone(),
            trailing: *trailing,
        })
        .collect();
    builder.attach_trivia_records(&records);
    drive_sink(
        &output,
        &parser
            .tokens
            .iter()
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>(),
        &mut builder,
    )?;
    builder.finish()
}

/// The recursive-descent driver over the scanner feed.
struct Parser<'s> {
    source: &'s str,
    feed: Vec<ScanToken>,
    trivia: Vec<(Range<u32>, u16, bool)>,
    pos: usize,
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    errors: usize,
}

/// Pratt binding-power rows (`rbp == lbp + 1` left-assoc).
struct OpRow {
    token: u16,
    lbp: u8,
    rbp: u8,
    node: u16,
}

/// The infix table: `OR` lowest, then `AND`, comparisons / `IS` /
/// `IN` / `LIKE` / `GLOB` / `BETWEEN` (`BETWEEN` handled specially),
/// `||`, `+ -`, `* / %`.
const INFIX: &[OpRow] = &[
    OpRow {
        token: tk::KW_OR,
        lbp: 1,
        rbp: 2,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_AND,
        lbp: 2,
        rbp: 3,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::EQ,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::NE,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LT,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GT,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LE,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GE,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_IS,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_LIKE,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_GLOB,
        lbp: 3,
        rbp: 4,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::CONCAT,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PLUS,
        lbp: 5,
        rbp: 6,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::MINUS,
        lbp: 5,
        rbp: 6,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::STAR,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SLASH,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PERCENT,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
];

impl<'s> Parser<'s> {
    fn run(source: &'s str, scan: lexer::Scan) -> Self {
        Self {
            source,
            feed: scan.tokens,
            trivia: scan.trivia,
            pos: 0,
            events: Vec::new(),
            tokens: Vec::new(),
            errors: scan.errors,
        }
    }

    fn peek(&self) -> Option<u16> {
        self.feed.get(self.pos).map(|t| t.kind)
    }

    fn peek_at(&self, offset: usize) -> Option<u16> {
        self.feed.get(self.pos + offset).map(|t| t.kind)
    }

    fn bump(&mut self) -> Option<(u16, Range<u32>)> {
        let token = self.feed.get(self.pos)?;
        self.events.push(Event::Token {
            kind: token.kind,
            n_raw: 1,
        });
        self.tokens.push((token.kind, token.range.clone()));
        self.pos += 1;
        Some((token.kind, token.range.clone()))
    }

    /// Error + ERROR region; sync = `;` `,` closers and statement
    /// keywords. Consumes ≥ 1 non-sync token (progress).
    fn fail_skip(&mut self, start: u32) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        let pos0 = self.pos;
        while let Some(token) = self.feed.get(self.pos) {
            if Self::is_sync(token.kind) {
                break;
            }
            self.pos += 1;
        }
        let at_sync = self
            .feed
            .get(self.pos)
            .is_some_and(|t| Self::is_sync(t.kind));
        if self.pos == pos0 && self.pos < self.feed.len() && !at_sync {
            self.pos += 1; // a non-sync stray rides the error region
        }
        let end = self
            .feed
            .get(self.pos.saturating_sub(1))
            .filter(|_| self.pos > pos0)
            .map_or(start, |t| t.range.end);
        if end > start {
            self.trivia
                .retain(|(range, _, _)| range.end <= start || range.start >= end);
            self.events.push(Event::Token {
                kind: ERROR_KIND,
                n_raw: 1,
            });
            self.tokens.push((ERROR_KIND, start..end));
        }
        self.events.push(Event::Finish);
    }

    fn is_sync(kind: u16) -> bool {
        matches!(
            kind,
            tk::SEMI
                | tk::COMMA
                | tk::RPAREN
                | tk::KW_SELECT
                | tk::KW_INSERT
                | tk::KW_UPDATE
                | tk::KW_DELETE
                | tk::KW_CREATE
                | tk::KW_FROM
                | tk::KW_WHERE
                | tk::KW_GROUP
                | tk::KW_ORDER
                | tk::KW_LIMIT
                | tk::KW_HAVING
                | tk::KW_SET
        )
    }

    /// Recovery that force-consumes one stray token (progress).
    fn fail_skip_stray(&mut self, start: u32) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        let end = self.feed.get(self.pos).map_or(start, |t| t.range.end);
        if self.pos < self.feed.len() {
            self.bump();
            self.trivia
                .retain(|(range, _, _)| range.end <= start || range.start >= end);
        }
        self.events.push(Event::Finish);
    }

    fn next_start(&self) -> u32 {
        self.feed
            .get(self.pos)
            .map_or(u32::try_from(self.source.len()).unwrap_or(u32::MAX), |t| {
                t.range.start
            })
    }

    // ------------------------------------------------------------------
    // Script / statements
    // ------------------------------------------------------------------

    fn parse_script(&mut self) {
        loop {
            match self.peek() {
                None => break,
                Some(tk::SEMI) => {
                    self.bump();
                }
                _ => self.parse_statement(),
            }
        }
    }

    fn parse_statement(&mut self) {
        match self.peek() {
            Some(tk::KW_SELECT | tk::KW_WITH) => {
                self.parse_select();
            }
            Some(tk::KW_INSERT) => self.parse_insert(),
            Some(tk::KW_UPDATE) => self.parse_update(),
            Some(tk::KW_DELETE) => self.parse_delete(),
            Some(tk::KW_CREATE) => self.parse_create(),
            Some(tk::LPAREN) => {
                self.parse_select(); // parenthesized select
            }
            _ => {
                let start = self.next_start();
                self.fail_skip_stray(start);
            }
        }
    }

    // ------------------------------------------------------------------
    // SELECT
    // ------------------------------------------------------------------

    fn parse_select(&mut self) {
        self.events.push(Event::Start {
            kind: nk::SELECT_STMT,
            forward_parent: None,
        });
        if self.peek() == Some(tk::KW_WITH) {
            self.parse_with();
        }
        if self.peek() == Some(tk::LPAREN) {
            // Parenthesized select opens its own SUBQUERY.
            self.bump();
            self.parse_select();
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            }
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() == Some(tk::KW_SELECT) {
            self.bump();
            if matches!(self.peek(), Some(tk::KW_DISTINCT | tk::KW_ALL)) {
                self.bump();
            }
            self.parse_result_columns();
            if self.peek() == Some(tk::KW_FROM) {
                self.parse_from();
            }
            if self.peek() == Some(tk::KW_WHERE) {
                self.events.push(Event::Start {
                    kind: nk::WHERE_CLAUSE,
                    forward_parent: None,
                });
                self.bump();
                self.parse_expression(0);
                self.events.push(Event::Finish);
            }
            if self.peek() == Some(tk::KW_GROUP) {
                self.events.push(Event::Start {
                    kind: nk::GROUP_BY_CLAUSE,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::KW_BY) {
                    self.bump();
                }
                self.parse_expr_list();
                if self.peek() == Some(tk::KW_HAVING) {
                    self.bump();
                    self.parse_expression(0);
                }
                self.events.push(Event::Finish);
            }
            if self.peek() == Some(tk::KW_ORDER) {
                self.events.push(Event::Start {
                    kind: nk::ORDER_BY_CLAUSE,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::KW_BY) {
                    self.bump();
                }
                loop {
                    if !self.parse_expression(0) {
                        break;
                    }
                    if matches!(self.peek(), Some(tk::KW_ASC | tk::KW_DESC)) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.events.push(Event::Finish);
            }
            if self.peek() == Some(tk::KW_LIMIT) {
                self.events.push(Event::Start {
                    kind: nk::LIMIT_CLAUSE,
                    forward_parent: None,
                });
                self.bump();
                self.parse_expression(0);
                if matches!(self.peek(), Some(tk::KW_OFFSET | tk::COMMA)) {
                    self.bump();
                    self.parse_expression(0);
                }
                self.events.push(Event::Finish);
            }
        }
        // UNION [ALL] chains stay inside the statement node.
        if self.peek() == Some(tk::KW_UNION) {
            self.bump();
            if self.peek() == Some(tk::KW_ALL) {
                self.bump();
            }
            self.parse_select();
        }
        self.events.push(Event::Finish);
    }

    fn parse_with(&mut self) {
        self.events.push(Event::Start {
            kind: nk::WITH_CLAUSE,
            forward_parent: None,
        });
        self.bump(); // WITH
        loop {
            self.events.push(Event::Start {
                kind: nk::CTE,
                forward_parent: None,
            });
            if self.peek() == Some(tk::IDENT) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            if self.peek() == Some(tk::LPAREN) {
                // Optional column list.
                self.bump();
                while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                    self.bump();
                }
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                }
            }
            if self.peek() == Some(tk::KW_AS) {
                self.bump();
            }
            if self.peek() == Some(tk::LPAREN) {
                self.bump();
                self.parse_select();
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
            self.events.push(Event::Finish);
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_result_columns(&mut self) {
        loop {
            self.events.push(Event::Start {
                kind: nk::RESULT_COLUMN,
                forward_parent: None,
            });
            if self.peek() == Some(tk::STAR) {
                self.bump();
            } else if self.peek() == Some(tk::IDENT)
                && self.peek_at(1) == Some(tk::DOT)
                && self.peek_at(2) == Some(tk::STAR)
            {
                self.bump();
                self.bump();
                self.bump();
            } else if Self::starts_expr(self.peek()) {
                self.parse_expression(0);
                if self.peek() == Some(tk::KW_AS) {
                    self.bump();
                    if self.peek() == Some(tk::IDENT) {
                        self.bump();
                    }
                } else if self.peek() == Some(tk::IDENT) {
                    // Bare alias.
                    self.bump();
                }
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            self.events.push(Event::Finish);
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn parse_from(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FROM_CLAUSE,
            forward_parent: None,
        });
        self.bump(); // FROM
        self.parse_table_ref();
        // Comma joins and JOIN chains.
        loop {
            match self.peek() {
                Some(tk::COMMA) => {
                    self.bump();
                    self.parse_table_ref();
                }
                Some(
                    tk::KW_JOIN
                    | tk::KW_LEFT
                    | tk::KW_RIGHT
                    | tk::KW_INNER
                    | tk::KW_OUTER
                    | tk::KW_CROSS,
                ) => {
                    self.events.push(Event::Start {
                        kind: nk::JOIN_CLAUSE,
                        forward_parent: None,
                    });
                    // Modifier soup to the JOIN keyword.
                    while matches!(
                        self.peek(),
                        Some(
                            tk::KW_LEFT | tk::KW_RIGHT | tk::KW_INNER | tk::KW_OUTER | tk::KW_CROSS
                        )
                    ) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::KW_JOIN) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    self.parse_table_ref();
                    if self.peek() == Some(tk::KW_ON) {
                        self.bump();
                        self.parse_expression(0);
                    }
                    self.events.push(Event::Finish);
                }
                _ => break,
            }
        }
        self.events.push(Event::Finish);
    }

    /// `name [(alias)]` or `( select ) [(alias)]`.
    fn parse_table_ref(&mut self) {
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            if matches!(self.peek(), Some(tk::KW_SELECT | tk::KW_WITH)) {
                self.parse_select();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        } else if self.peek() == Some(tk::IDENT) {
            self.bump();
            if self.peek() == Some(tk::DOT) && self.peek_at(1) == Some(tk::IDENT) {
                self.bump();
                self.bump();
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            return;
        }
        // Optional alias: bare IDENT (not a clause keyword).
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        }
    }

    // ------------------------------------------------------------------
    // INSERT / UPDATE / DELETE / CREATE
    // ------------------------------------------------------------------

    fn parse_insert(&mut self) {
        self.events.push(Event::Start {
            kind: nk::INSERT_STMT,
            forward_parent: None,
        });
        self.bump(); // INSERT
        if self.peek() == Some(tk::KW_INTO) {
            self.bump();
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                if self.peek() == Some(tk::IDENT) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                if self.peek() == Some(tk::COMMA) {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            }
        }
        if self.peek() == Some(tk::KW_VALUES) {
            self.bump();
            loop {
                if self.peek() != Some(tk::LPAREN) {
                    break;
                }
                self.bump();
                self.parse_expr_list();
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                if self.peek() == Some(tk::COMMA) {
                    self.bump();
                } else {
                    break;
                }
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_update(&mut self) {
        self.events.push(Event::Start {
            kind: nk::UPDATE_STMT,
            forward_parent: None,
        });
        self.bump(); // UPDATE
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::KW_SET) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        loop {
            self.events.push(Event::Start {
                kind: nk::SET_ASSIGN,
                forward_parent: None,
            });
            if self.peek() == Some(tk::IDENT) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            if self.peek() == Some(tk::EQ) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            self.parse_expression(0);
            self.events.push(Event::Finish);
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some(tk::KW_WHERE) {
            self.events.push(Event::Start {
                kind: nk::WHERE_CLAUSE,
                forward_parent: None,
            });
            self.bump();
            self.parse_expression(0);
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    fn parse_delete(&mut self) {
        self.events.push(Event::Start {
            kind: nk::DELETE_STMT,
            forward_parent: None,
        });
        self.bump(); // DELETE
        if self.peek() == Some(tk::KW_FROM) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::KW_WHERE) {
            self.events.push(Event::Start {
                kind: nk::WHERE_CLAUSE,
                forward_parent: None,
            });
            self.bump();
            self.parse_expression(0);
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    fn parse_create(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CREATE_TABLE,
            forward_parent: None,
        });
        self.bump(); // CREATE
        if self.peek() == Some(tk::KW_TABLE) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() == Some(tk::KW_IF) {
            self.bump();
            if self.peek() == Some(tk::KW_NOT) {
                self.bump();
            }
            if self.peek() == Some(tk::KW_EXISTS) {
                self.bump();
            }
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
            if self.peek() == Some(tk::DOT) && self.peek_at(1) == Some(tk::IDENT) {
                self.bump();
                self.bump();
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                if matches!(
                    self.peek(),
                    Some(tk::KW_PRIMARY | tk::KW_UNIQUE | tk::KW_CHECK | tk::KW_FOREIGN)
                ) {
                    self.parse_table_constraint();
                } else {
                    self.parse_column_def();
                }
                if self.peek() == Some(tk::COMMA) {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    #[allow(clippy::too_many_lines)] // the column grammar is the data
    fn parse_column_def(&mut self) {
        self.events.push(Event::Start {
            kind: nk::COLUMN_DEF,
            forward_parent: None,
        });
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        // Type name: token soup until a constraint keyword / `,` / `)`.
        while let Some(kind) = self.peek() {
            if matches!(kind, tk::IDENT | tk::INT_LIT | tk::LPAREN | tk::RPAREN)
                && !Self::is_constraint_kw(kind)
            {
                if kind == tk::LPAREN {
                    // e.g. VARCHAR(10)
                    self.bump();
                    while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    }
                    continue;
                }
                if kind == tk::RPAREN {
                    break;
                }
                self.bump();
            } else {
                break;
            }
        }
        // Column constraints.
        loop {
            match self.peek() {
                Some(tk::KW_PRIMARY) => {
                    self.bump();
                    if self.peek() == Some(tk::KW_KEY) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::KW_AUTOINCREMENT) {
                        self.bump();
                    }
                }
                Some(tk::KW_NOT) => {
                    self.bump();
                    if self.peek() == Some(tk::KW_NULL) {
                        self.bump();
                    }
                }
                Some(tk::KW_NULL | tk::KW_UNIQUE) => {
                    self.bump();
                }
                Some(tk::KW_DEFAULT) => {
                    self.bump();
                    if Self::starts_expr(self.peek()) {
                        self.parse_expression(0);
                    } else if self.peek() == Some(tk::LPAREN) {
                        self.bump();
                        self.parse_expression(0);
                        if self.peek() == Some(tk::RPAREN) {
                            self.bump();
                        }
                    }
                }
                Some(tk::KW_CHECK) => {
                    self.bump();
                    if self.peek() == Some(tk::LPAREN) {
                        self.bump();
                        self.parse_expression(0);
                        if self.peek() == Some(tk::RPAREN) {
                            self.bump();
                        }
                    }
                }
                Some(tk::KW_REFERENCES) => {
                    self.bump();
                    if self.peek() == Some(tk::IDENT) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::LPAREN) {
                        self.bump();
                        while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                            self.bump();
                        }
                        if self.peek() == Some(tk::RPAREN) {
                            self.bump();
                        }
                    }
                }
                _ => break,
            }
        }
        self.events.push(Event::Finish);
    }

    fn is_constraint_kw(kind: u16) -> bool {
        matches!(
            kind,
            tk::KW_PRIMARY
                | tk::KW_NOT
                | tk::KW_NULL
                | tk::KW_UNIQUE
                | tk::KW_DEFAULT
                | tk::KW_CHECK
                | tk::KW_REFERENCES
        )
    }

    fn parse_table_constraint(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TABLE_CONSTRAINT,
            forward_parent: None,
        });
        // Constraint soup to the matching close or `,`.
        let mut depth = 0_usize;
        while let Some(kind) = self.peek() {
            if kind == tk::LPAREN {
                depth += 1;
            }
            if kind == tk::RPAREN {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                self.bump();
                if depth == 0 {
                    continue;
                }
                continue;
            }
            if depth == 0 && matches!(kind, tk::COMMA) {
                break;
            }
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Expressions (Pratt)
    // ------------------------------------------------------------------

    fn parse_expr_list(&mut self) {
        loop {
            if !Self::starts_expr(self.peek()) {
                break;
            }
            self.parse_expression(0);
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn starts_expr(kind: Option<u16>) -> bool {
        matches!(
            kind,
            Some(
                tk::IDENT
                    | tk::INT_LIT
                    | tk::FLOAT_LIT
                    | tk::STRING_LIT
                    | tk::LPAREN
                    | tk::MINUS
                    | tk::PLUS
                    | tk::KW_NOT
                    | tk::KW_NULL
                    | tk::KW_CAST
                    | tk::KW_CASE
                    | tk::KW_EXISTS
                    | tk::STAR
            )
        )
    }

    #[allow(clippy::too_many_lines)] // the expression grammar is the data
    fn parse_expression(&mut self, min_bp: u8) -> bool {
        if !self.parse_prefix() {
            return false;
        }
        while let Some(kind) = self.peek() {
            // `NOT IN / NOT LIKE / NOT GLOB / NOT BETWEEN`.
            if kind == tk::KW_NOT
                && matches!(
                    self.peek_at(1),
                    Some(tk::KW_IN | tk::KW_LIKE | tk::KW_GLOB | tk::KW_BETWEEN)
                )
            {
                if 3 < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
                    forward_parent: None,
                });
                self.bump(); // NOT
                self.bump(); // op
                if !self.parse_match_rhs(kind) {
                    self.events.pop();
                    break;
                }
                self.events.push(Event::Finish);
                continue;
            }
            if kind == tk::KW_IN {
                if 3 < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_in_rhs() {
                    self.events.pop();
                    break;
                }
                self.events.push(Event::Finish);
                continue;
            }
            if kind == tk::KW_BETWEEN {
                if 3 < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
                    forward_parent: None,
                });
                self.bump();
                // Lower bound above `AND`; then the joining `AND`.
                if !self.parse_expression(3) {
                    self.events.pop();
                    break;
                }
                if self.peek() == Some(tk::KW_AND) {
                    self.bump();
                }
                if !self.parse_expression(3) {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                continue;
            }
            if kind == tk::KW_IS {
                if 3 < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::KW_NOT) {
                    self.bump();
                }
                if !self.parse_expression(4) {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                continue;
            }
            if let Some(row) = INFIX.iter().find(|row| row.token == kind) {
                if row.lbp < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: row.node,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_expression(row.rbp) {
                    self.events.pop();
                    break;
                }
                self.events.push(Event::Finish);
                continue;
            }
            break;
        }
        true
    }

    /// The rhs of `IN`: `(select)`, `(e, …)` or a bare table name.
    fn parse_in_rhs(&mut self) -> bool {
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            if matches!(self.peek(), Some(tk::KW_SELECT | tk::KW_WITH)) {
                self.events.push(Event::Start {
                    kind: nk::SUBQUERY,
                    forward_parent: None,
                });
                self.parse_select();
                self.events.push(Event::Finish);
            } else {
                self.parse_expr_list();
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            true
        } else if self.peek() == Some(tk::IDENT) {
            self.bump();
            true
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            false
        }
    }

    /// The rhs of `LIKE` / `GLOB` / `NOT LIKE` / … (plain expr).
    fn parse_match_rhs(&mut self, op: u16) -> bool {
        let _ = op;
        if Self::starts_expr(self.peek()) {
            self.parse_expression(3)
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            false
        }
    }

    fn parse_prefix(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::MINUS | tk::PLUS | tk::KW_NOT => {
                self.events.push(Event::Start {
                    kind: nk::UNARY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                let operand_bp = if kind == tk::KW_NOT { 2 } else { 7 };
                if !self.parse_expression(operand_bp) {
                    self.events.pop();
                    return false;
                }
                self.events.push(Event::Finish);
                true
            }
            tk::KW_CASE => {
                self.parse_case();
                true
            }
            tk::KW_CAST => {
                self.parse_cast();
                true
            }
            tk::LPAREN => {
                // Subquery or grouped expr/list.
                if matches!(self.peek_at(1), Some(tk::KW_SELECT | tk::KW_WITH)) {
                    self.events.push(Event::Start {
                        kind: nk::SUBQUERY,
                        forward_parent: None,
                    });
                    self.bump();
                    self.parse_select();
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    self.events.push(Event::Finish);
                    return true;
                }
                self.events.push(Event::Start {
                    kind: nk::EXPR_LIST,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() != Some(tk::RPAREN) {
                    self.parse_expr_list();
                }
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                true
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_case(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CASE_EXPR,
            forward_parent: None,
        });
        self.bump(); // CASE
        if self.peek() != Some(tk::KW_WHEN) {
            // Operand form: CASE e WHEN …
            self.parse_expression(0);
        }
        while self.peek() == Some(tk::KW_WHEN) {
            self.events.push(Event::Start {
                kind: nk::WHEN_ARM,
                forward_parent: None,
            });
            self.bump();
            self.parse_expression(0);
            if self.peek() == Some(tk::KW_THEN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            self.parse_expression(0);
            self.events.push(Event::Finish);
        }
        if self.peek() == Some(tk::KW_ELSE) {
            self.bump();
            self.parse_expression(0);
        }
        if self.peek() == Some(tk::KW_END) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_cast(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CAST_EXPR,
            forward_parent: None,
        });
        self.bump(); // CAST
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            self.parse_expression(0);
            if self.peek() == Some(tk::KW_AS) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
            // Type name soup.
            while matches!(self.peek(), Some(tk::IDENT)) {
                self.bump();
                if self.peek() == Some(tk::LPAREN) {
                    self.bump();
                    while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    }
                }
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_atom(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::IDENT | tk::INT_LIT | tk::FLOAT_LIT | tk::STRING_LIT | tk::KW_NULL => {
                self.bump();
                // Qualified `t.c` / function call `f(…)`.
                loop {
                    if self.peek() == Some(tk::DOT) && matches!(self.peek_at(1), Some(tk::IDENT)) {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    if self.peek() == Some(tk::LPAREN) {
                        self.events.push(Event::Start {
                            kind: nk::CALL_EXPR,
                            forward_parent: None,
                        });
                        self.bump();
                        if self.peek() == Some(tk::STAR) {
                            self.bump();
                        } else if self.peek() != Some(tk::RPAREN) {
                            if self.peek() == Some(tk::KW_DISTINCT) {
                                self.bump();
                            }
                            self.parse_expr_list();
                        }
                        if self.peek() == Some(tk::RPAREN) {
                            self.bump();
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start);
                        }
                        self.events.push(Event::Finish);
                        continue;
                    }
                    break;
                }
                true
            }
            // Bare `*` only valid as a result column — the caller
            // handled it; inside expressions it is an error.
            _ => false,
        }
    }
}

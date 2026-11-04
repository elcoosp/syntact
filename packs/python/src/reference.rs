//! Hand-written reference parser for the documented Python subset
//! (issue 028): statements and suites over INDENT/DEDENT, Pratt
//! expressions, decorators, comprehensions, f-strings.

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

/// Parse Python source into the raw event stream:
/// `(events, token_ranges, error_count)`.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse Python source into a green tree (one implicit root).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_module();
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
    last_end: u32,
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    errors: usize,
}

/// Pratt binding-power table rows.
struct OpRow {
    token: u16,
    lbp: u8,
    rbp: u8,
    node: u16,
}

/// The expression operator table (documented subset; `rbp == lbp + 1`
/// left-assoc, `rbp == lbp` right-assoc).
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
        token: tk::KW_IN,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_IS,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::EQ,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::NE,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LT,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GT,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LE,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GE,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PIPE,
        lbp: 5,
        rbp: 6,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::CARET,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::AMP,
        lbp: 7,
        rbp: 8,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LSHIFT,
        lbp: 8,
        rbp: 9,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::RSHIFT,
        lbp: 8,
        rbp: 9,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PLUS,
        lbp: 9,
        rbp: 10,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::MINUS,
        lbp: 9,
        rbp: 10,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::STAR,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SLASH,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::DSLASH,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PERCENT,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PSTAR,
        lbp: 13,
        rbp: 13,
        node: nk::BIN_EXPR,
    }, // right-assoc
];

impl<'s> Parser<'s> {
    fn run(source: &'s str, scan: lexer::Scan) -> Self {
        Self {
            source,
            feed: scan.tokens,
            trivia: scan.trivia,
            pos: 0,
            last_end: 0,
            events: Vec::new(),
            tokens: Vec::new(),
            errors: scan.errors,
        }
    }

    fn peek(&self) -> Option<u16> {
        self.feed.get(self.pos).map(|t| t.kind)
    }

    fn bump(&mut self) -> Option<(u16, Range<u32>)> {
        let token = self.feed.get(self.pos)?;
        self.events.push(Event::Token {
            kind: token.kind,
            n_raw: 1,
        });
        self.tokens.push((token.kind, token.range.clone()));
        self.last_end = token.range.end;
        self.pos += 1;
        Some((token.kind, token.range.clone()))
    }

    /// Error + ERROR region around the offending token; sync = newline /
    /// dedent / colon / comma / closers (line-structured recovery).
    fn fail_skip(&mut self, start: u32, _consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        let pos0 = self.pos;
        while let Some(token) = self.feed.get(self.pos) {
            if matches!(
                token.kind,
                tk::NEWLINE | tk::DEDENT | tk::COLON | tk::COMMA | tk::RPAREN | tk::RBRACK
            ) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == pos0 && self.pos < self.feed.len() {
            self.pos += 1; // the stray token rides the error region (progress)
        }
        let end = self
            .feed
            .get(self.pos.saturating_sub(1))
            .filter(|_| self.pos > pos0)
            .map_or(start, |t| t.range.end);
        if end > start {
            // The error region covers inter-token bytes that the scanner
            // already filed as trivia — drop those records (every byte is
            // covered exactly once: losslessness).
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

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        loop {
            match self.peek() {
                None | Some(tk::DEDENT) => break,
                _ => self.parse_statement(),
            }
        }
    }

    fn parse_statement(&mut self) {
        match self.peek() {
            Some(tk::AT) => self.parse_decorated(),
            Some(tk::KW_IF) => self.parse_if(),
            Some(tk::KW_WHILE) => self.parse_while(),
            Some(tk::KW_FOR) => self.parse_for(),
            Some(tk::KW_DEF) => self.parse_def(),
            Some(tk::KW_RETURN) => self.parse_return(),
            Some(tk::KW_PASS) => self.parse_simple_node(nk::PASS, tk::KW_PASS),
            Some(tk::KW_BREAK) => self.parse_simple_node(nk::BREAK, tk::KW_BREAK),
            Some(tk::KW_CONTINUE) => self.parse_simple_node(nk::CONTINUE, tk::KW_CONTINUE),
            _ => self.parse_expr_statement(),
        }
    }

    fn parse_simple_node(&mut self, node: u16, kw: u16) {
        self.events.push(Event::Start {
            kind: node,
            forward_parent: None,
        });
        if self.peek() == Some(kw) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        self.expect_line_end();
        self.events.push(Event::Finish);
    }

    fn next_start(&self) -> u32 {
        self.feed
            .get(self.pos)
            .map_or(u32::try_from(self.source.len()).unwrap_or(u32::MAX), |t| {
                t.range.start
            })
    }

    /// NEWLINE (or DEDENT/EOF for the last line without a newline).
    fn expect_line_end(&mut self) {
        match self.peek() {
            Some(tk::NEWLINE) => {
                self.bump();
            }
            Some(tk::DEDENT) | None => {}
            _ => {
                let start = self.next_start();
                self.fail_skip(start, true);
                if self.peek() == Some(tk::NEWLINE) {
                    self.bump();
                }
            }
        }
    }

    fn parse_expr_statement(&mut self) {
        self.events.push(Event::Start {
            kind: nk::EXPR_STMT,
            forward_parent: None,
        });
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        // Assignment: `target = value` (single, documented subset).
        if self.peek() == Some(tk::ASSIGN) {
            self.events.push(Event::Start {
                kind: nk::ASSIGN,
                forward_parent: None,
            });
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start, true);
            }
            self.events.push(Event::Finish);
            // Re-shape: the EXPR_STMT wrapper carries the ASSIGN child.
        }
        self.expect_line_end();
        self.events.push(Event::Finish);
    }

    fn parse_return(&mut self) {
        self.events.push(Event::Start {
            kind: nk::RETURN,
            forward_parent: None,
        });
        self.bump();
        if !matches!(self.peek(), Some(tk::NEWLINE | tk::DEDENT) | None)
            && !self.parse_expression(0)
        {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        self.expect_line_end();
        self.events.push(Event::Finish);
    }

    fn parse_decorated(&mut self) {
        while self.peek() == Some(tk::AT) {
            self.events.push(Event::Start {
                kind: nk::DECORATOR,
                forward_parent: None,
            });
            self.bump();
            // dotted name subset
            if self.peek() == Some(tk::NAME) {
                self.bump();
                while self.peek() == Some(tk::DOT)
                    && self.feed.get(self.pos + 1).map(|t| t.kind) == Some(tk::NAME)
                {
                    self.bump();
                    self.bump();
                }
            } else {
                let start = self.next_start();
                self.fail_skip(start, true);
            }
            self.expect_line_end();
            self.events.push(Event::Finish);
        }
        if self.peek() == Some(tk::KW_DEF) {
            self.parse_def();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
    }

    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IF_STMT,
            forward_parent: None,
        });
        self.bump(); // if / elif
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if !self.parse_suite() {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
        // elif / else chains.
        match self.peek() {
            Some(tk::KW_ELIF) => self.parse_if(),
            Some(tk::KW_ELSE) => {
                self.events.push(Event::Start {
                    kind: nk::ELSE_CLAUSE,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_suite() {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                }
                self.events.push(Event::Finish);
            }
            _ => {}
        }
    }

    fn parse_while(&mut self) {
        self.events.push(Event::Start {
            kind: nk::WHILE_STMT,
            forward_parent: None,
        });
        self.bump();
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if !self.parse_suite() {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FOR_STMT,
            forward_parent: None,
        });
        self.bump();
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if self.peek() == Some(tk::KW_IN) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if !self.parse_suite() {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    fn parse_def(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FUNC_HEADER,
            forward_parent: None,
        });
        self.bump(); // def
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            loop {
                match self.peek() {
                    Some(tk::RPAREN) => {
                        self.bump();
                        break;
                    }
                    None | Some(tk::NEWLINE | tk::DEDENT) => {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                        break;
                    }
                    _ => {
                        if self.peek() == Some(tk::NAME) {
                            self.bump();
                            if self.peek() == Some(tk::ASSIGN) {
                                self.bump();
                                if !self.parse_expression(0) {
                                    let start = self.next_start();
                                    self.fail_skip(start, true);
                                }
                            }
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start, true);
                        }
                        if self.peek() == Some(tk::COMMA) {
                            self.bump();
                        }
                    }
                }
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if self.peek() == Some(tk::ARROW) {
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start, true);
            }
        }
        self.events.push(Event::Finish);
        if !self.parse_suite() {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
    }

    /// `:` (NEWLINE INDENT statements DEDENT | simple-stmt NEWLINE).
    fn parse_suite(&mut self) -> bool {
        if self.peek() != Some(tk::COLON) {
            return false;
        }
        self.bump();
        if self.peek() != Some(tk::NEWLINE) {
            // Inline suite: one simple statement.
            self.parse_statement();
            return true;
        }
        self.bump();
        if self.peek() != Some(tk::INDENT) {
            return false;
        }
        self.bump();
        while !matches!(self.peek(), Some(tk::DEDENT) | None) {
            self.parse_statement();
        }
        if self.peek() == Some(tk::DEDENT) {
            self.bump();
        }
        true
    }

    // ------------------------------------------------------------------
    // Expressions (Pratt)
    // ------------------------------------------------------------------

    fn parse_expression(&mut self, min_bp: u8) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        // Prefix unaries: `- + ~` bind tight (11); `not` sits between
        // the comparisons and `and` (operand bp 3 — Python semantics).
        if matches!(kind, tk::MINUS | tk::PLUS | tk::TILDE | tk::KW_NOT) {
            self.events.push(Event::Start {
                kind: nk::UNARY_EXPR,
                forward_parent: None,
            });
            self.bump();
            let operand_bp = if kind == tk::KW_NOT { 3 } else { 11 };
            if !self.parse_expression(operand_bp) {
                self.events.pop(); // drop the unary Start
                return false;
            }
            self.events.push(Event::Finish);
        } else if !self.parse_atom() {
            return false;
        }
        // Climb.
        while let Some(kind) = self.peek() {
            if kind == tk::LPAREN {
                // Call.
                self.events.push(Event::Start {
                    kind: nk::CALL,
                    forward_parent: None,
                });
                self.bump();
                self.parse_call_args(tk::RPAREN);
                self.events.push(Event::Finish);
            } else if kind == tk::DOT {
                self.events.push(Event::Start {
                    kind: nk::ATTR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::NAME) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                }
                self.events.push(Event::Finish);
            } else if kind == tk::LBRACK {
                self.events.push(Event::Start {
                    kind: nk::SUBSCRIPT,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_expression(0) {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                }
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                }
                self.events.push(Event::Finish);
            } else if let Some(row) = INFIX.iter().find(|row| row.token == kind) {
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
            } else {
                break;
            }
        }
        true
    }

    fn parse_call_args(&mut self, closer: u16) {
        loop {
            match self.peek() {
                Some(k) if k == closer => {
                    self.bump();
                    break;
                }
                None | Some(tk::NEWLINE | tk::DEDENT) => {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                    break;
                }
                _ => {
                    if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
    }

    fn parse_atom(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::NAME | tk::NUMBER | tk::STRING | tk::KW_TRUE | tk::KW_FALSE | tk::KW_NONE => {
                self.bump();
                true
            }
            tk::FSTRING_START => {
                self.parse_fstring();
                true
            }
            tk::LPAREN => {
                self.events.push(Event::Start {
                    kind: nk::TUPLE_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                } else {
                    if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                    while self.peek() == Some(tk::COMMA) {
                        self.bump();
                        if self.peek() == Some(tk::RPAREN) {
                            break;
                        }
                        if !self.parse_expression(0) {
                            let start = self.next_start();
                            self.fail_skip(start, true);
                        }
                    }
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                }
                self.events.push(Event::Finish);
                true
            }
            tk::LBRACK => {
                self.events.push(Event::Start {
                    kind: nk::LIST_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                    if self.peek() == Some(tk::KW_FOR) {
                        // comprehension: (for target in iter (if cond)*)
                        self.parse_comp_for();
                    } else {
                        while self.peek() == Some(tk::COMMA) {
                            self.bump();
                            if self.peek() == Some(tk::RBRACK) {
                                break;
                            }
                            if !self.parse_expression(0) {
                                let start = self.next_start();
                                self.fail_skip(start, true);
                            }
                        }
                    }
                    if self.peek() == Some(tk::RBRACK) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                }
                self.events.push(Event::Finish);
                true
            }
            _ => false,
        }
    }

    fn parse_comp_for(&mut self) {
        self.events.push(Event::Start {
            kind: nk::COMP_FOR,
            forward_parent: None,
        });
        self.bump(); // for
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if self.peek() == Some(tk::KW_IN) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        if !self.parse_expression(4) {
            let start = self.next_start();
            self.fail_skip(start, true);
        }
        while self.peek() == Some(tk::KW_IF) {
            self.events.push(Event::Start {
                kind: nk::COMP_IF,
                forward_parent: None,
            });
            self.bump();
            if !self.parse_expression(4) {
                let start = self.next_start();
                self.fail_skip(start, true);
            }
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    /// `FSTRING_START (FSTRING_TEXT | INTERP_LBRACE expr INTERP_RBRACE)*
    /// FSTRING_END` — nested f-strings recurse through the atom rule.
    fn parse_fstring(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FSTRING,
            forward_parent: None,
        });
        self.bump(); // FSTRING_START
        loop {
            match self.peek() {
                Some(tk::FSTRING_TEXT) => {
                    self.bump();
                }
                Some(tk::INTERP_LBRACE) => {
                    self.bump();
                    if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                    if self.peek() == Some(tk::INTERP_RBRACE) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start, true);
                    }
                }
                Some(tk::FSTRING_END) => {
                    self.bump();
                    break;
                }
                _ => {
                    let start = self.next_start();
                    self.fail_skip(start, true);
                    break;
                }
            }
        }
        self.events.push(Event::Finish);
    }
}

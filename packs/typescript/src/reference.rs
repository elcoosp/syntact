//! Hand-written reference parser for the documented TypeScript subset
//! (issue 027): modules, interfaces/classes/enums/type aliases, Pratt
//! expressions with the `>>`-split climb, template literals, and ASI.

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

/// Parse TypeScript source into the raw event stream.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse TypeScript source into a green tree (one implicit root).
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
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    errors: usize,
}

/// One Pratt infix row. Shifts use `u8::MAX` markers: the climb combines
/// the SPLIT `>` / `<` tokens (the generics rule) into shift nodes.
const INFIX: &[(u16, u8, u8, u16)] = &[
    (tk::OROR, 1, 2, nk::BIN_EXPR),
    (tk::ANDAND, 2, 3, nk::BIN_EXPR),
    (tk::PIPE, 4, 5, nk::BIN_EXPR),
    (tk::CARET, 5, 6, nk::BIN_EXPR),
    (tk::AMP, 6, 7, nk::BIN_EXPR),
    (tk::EQ3, 7, 8, nk::BIN_EXPR),
    (tk::NEQ3, 7, 8, nk::BIN_EXPR),
    (tk::NE, 7, 8, nk::BIN_EXPR),
    (tk::LE, 7, 8, nk::BIN_EXPR),
    (tk::GE, 7, 8, nk::BIN_EXPR),
    (tk::LT, 7, 8, nk::BIN_EXPR),
    (tk::GT, 7, 8, nk::BIN_EXPR),
    (tk::PLUS, 9, 10, nk::BIN_EXPR),
    (tk::MINUS, 9, 10, nk::BIN_EXPR),
    (tk::STAR, 10, 11, nk::BIN_EXPR),
    (tk::SLASH, 10, 11, nk::BIN_EXPR),
    (tk::PERCENT, 10, 11, nk::BIN_EXPR),
    (tk::PSTAR, 13, 13, nk::BIN_EXPR), // right-assoc
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

    fn peek2(&self) -> Option<u16> {
        self.feed.get(self.pos + 1).map(|t| t.kind)
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

    fn next_start(&self) -> u32 {
        self.feed.get(self.pos).map_or_else(
            || u32::try_from(self.source.len()).unwrap_or(u32::MAX),
            |t| t.range.start,
        )
    }

    /// ASI: whether the current token starts a new line relative to the
    /// previous token (a newline byte between the two ranges).
    fn at_new_line(&self) -> bool {
        let (Some(prev), Some(next)) = (
            self.pos.checked_sub(1).and_then(|i| self.feed.get(i)),
            self.feed.get(self.pos),
        ) else {
            return false;
        };
        self.source[prev.range.end as usize..next.range.start as usize].contains('\n')
    }

    /// Statement terminator: `;` (consumed), a newline (ASI), `}`/EOF.
    fn consume_terminator(&mut self) {
        match self.peek() {
            Some(tk::SEMI) => {
                self.bump();
            }
            Some(tk::RBRACE) | None => {}
            Some(_) if self.at_new_line() => {}
            _ => {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
    }

    /// Error + ERROR region around the skipped tokens; sync on statement
    /// boundaries (`;`, newline, closers). Stray-char rule guarantees
    /// progress.
    fn fail_skip(&mut self, start: u32) {
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
                tk::SEMI
                    | tk::RBRACE
                    | tk::RPAREN
                    | tk::RBRACK
                    | tk::COMMA
                    | tk::COLON
                    | tk::LBRACE
            ) || (self.pos > pos0 && self.newline_before(self.pos))
            {
                break;
            }
            self.pos += 1;
        }
        if self.pos == pos0 && self.pos < self.feed.len() {
            self.pos += 1; // stray token rides the region (progress)
        }
        let end = self
            .feed
            .get(self.pos.saturating_sub(1))
            .filter(|_| self.pos > pos0)
            .map_or(start, |t| t.range.end);
        if end > start {
            // Drop trivia records inside the region (byte-exact coverage).
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

    /// Whether a newline sits between tokens `idx - 1` and `idx`.
    fn newline_before(&self, idx: usize) -> bool {
        let (Some(prev), Some(next)) = (
            idx.checked_sub(1).and_then(|i| self.feed.get(i)),
            self.feed.get(idx),
        ) else {
            return false;
        };
        self.source[prev.range.end as usize..next.range.start as usize].contains('\n')
    }

    // ------------------------------------------------------------------
    // Module
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        while self.peek().is_some() {
            self.parse_statement();
        }
    }

    fn parse_statement(&mut self) {
        match self.peek() {
            Some(tk::KW_IMPORT) => self.parse_import(),
            Some(tk::KW_EXPORT) => self.parse_export(),
            Some(tk::KW_INTERFACE) => self.parse_interface(),
            Some(tk::KW_TYPE) if self.peek2() == Some(tk::NAME) => self.parse_type_alias(),
            Some(tk::KW_CLASS) => self.parse_class(),
            Some(tk::KW_ENUM) => self.parse_enum(),
            Some(tk::KW_FUNCTION) => self.parse_function(),
            Some(tk::KW_CONST | tk::KW_LET | tk::KW_VAR) => {
                self.parse_var_decl();
            }
            Some(tk::KW_IF) => self.parse_if(),
            Some(tk::KW_WHILE) => self.parse_while(),
            Some(tk::KW_FOR) => self.parse_for(),
            Some(tk::KW_RETURN) => self.parse_return(),
            Some(tk::LBRACE) => self.parse_block_stmt(),
            _ => self.parse_expr_statement(),
        }
    }

    fn parse_import(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IMPORT_DECL,
            forward_parent: None,
        });
        self.bump(); // import
        match self.peek() {
            Some(tk::STRING) => {
                self.bump(); // side-effect import
            }
            Some(tk::NAME) => {
                self.bump(); // default import
                if self.peek() == Some(tk::COMMA) {
                    self.bump();
                }
                self.parse_named_imports();
            }
            Some(tk::LBRACE) => self.parse_named_imports(),
            _ => {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        if self.peek() == Some(tk::KW_FROM) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::STRING) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    fn parse_named_imports(&mut self) {
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            loop {
                match self.peek() {
                    Some(tk::RBRACE) => {
                        self.bump();
                        break;
                    }
                    Some(tk::NAME) => {
                        self.bump();
                        if self.peek() == Some(tk::COMMA) {
                            self.bump();
                        }
                    }
                    _ => {
                        let start = self.next_start();
                        self.fail_skip(start);
                        break;
                    }
                }
            }
        }
    }

    fn parse_export(&mut self) {
        self.events.push(Event::Start {
            kind: nk::EXPORT_DECL,
            forward_parent: None,
        });
        self.bump(); // export
        if self.peek() == Some(tk::KW_DEFAULT) {
            self.bump();
        }
        self.parse_statement();
        self.events.push(Event::Finish);
    }

    fn parse_interface(&mut self) {
        self.events.push(Event::Start {
            kind: nk::INTERFACE_DECL,
            forward_parent: None,
        });
        self.bump(); // interface
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_body(nk::INTERFACE_MEMBER);
        self.events.push(Event::Finish);
    }

    /// `{ member* }` shared by interfaces and object types: members are
    /// `name(?:)? Type` or `name(params): Type`, `;`- or `,`-separated.
    fn parse_type_body(&mut self, member_node: u16) {
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            loop {
                match self.peek() {
                    Some(tk::RBRACE) => {
                        self.bump();
                        break;
                    }
                    None => break,
                    _ => {
                        self.events.push(Event::Start {
                            kind: member_node,
                            forward_parent: None,
                        });
                        if self.peek() == Some(tk::NAME) {
                            self.bump();
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start);
                        }
                        if self.peek() == Some(tk::QUESTION) {
                            self.bump();
                        }
                        if self.peek() == Some(tk::LPAREN) {
                            // method signature: params + return type
                            self.parse_params();
                            if self.peek() == Some(tk::COLON) {
                                self.parse_type_annotation();
                            }
                        } else if self.peek() == Some(tk::COLON) {
                            self.parse_type_annotation();
                        }
                        self.events.push(Event::Finish);
                        if matches!(self.peek(), Some(tk::SEMI | tk::COMMA)) {
                            self.bump();
                        }
                    }
                }
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
    }

    fn parse_type_alias(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TYPE_ALIAS,
            forward_parent: None,
        });
        self.bump(); // type
        if self.peek() == Some(tk::NAME) {
            self.bump();
        }
        if self.peek() == Some(tk::EQUALS) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type();
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    fn parse_class(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CLASS_DECL,
            forward_parent: None,
        });
        self.bump(); // class
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            loop {
                match self.peek() {
                    Some(tk::RBRACE) => {
                        self.bump();
                        break;
                    }
                    None => break,
                    _ => self.parse_class_member(),
                }
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_class_member(&mut self) {
        // Modifiers.
        while matches!(
            self.peek(),
            Some(tk::KW_READONLY | tk::KW_PRIVATE | tk::KW_PUBLIC | tk::KW_STATIC)
        ) {
            self.bump();
        }
        let methodish = self.peek() == Some(tk::NAME)
            && matches!(self.peek2(), Some(tk::LPAREN | tk::QUESTION | tk::LT));
        let node = if methodish {
            nk::METHOD_DECL
        } else {
            nk::CLASS_FIELD
        };
        self.events.push(Event::Start {
            kind: node,
            forward_parent: None,
        });
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::QUESTION) {
            self.bump();
        }
        if methodish {
            self.parse_params();
            if self.peek() == Some(tk::COLON) {
                self.parse_type_annotation();
            }
            if self.peek() == Some(tk::LBRACE) {
                self.parse_block_stmt();
            }
        } else {
            if self.peek() == Some(tk::COLON) {
                self.parse_type_annotation();
            }
            if self.peek() == Some(tk::EQUALS) {
                self.bump();
                if !self.parse_expression(0) {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
            self.consume_terminator();
        }
        self.events.push(Event::Finish);
    }

    fn parse_enum(&mut self) {
        self.events.push(Event::Start {
            kind: nk::ENUM_DECL,
            forward_parent: None,
        });
        self.bump(); // enum
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            loop {
                match self.peek() {
                    Some(tk::RBRACE) => {
                        self.bump();
                        break;
                    }
                    None => break,
                    _ => {
                        self.events.push(Event::Start {
                            kind: nk::ENUM_MEMBER,
                            forward_parent: None,
                        });
                        if self.peek() == Some(tk::NAME) {
                            self.bump();
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start);
                        }
                        if self.peek() == Some(tk::EQUALS) {
                            self.bump();
                            if !self.parse_expression(0) {
                                let start = self.next_start();
                                self.fail_skip(start);
                            }
                        }
                        self.events.push(Event::Finish);
                        if self.peek() == Some(tk::COMMA) {
                            self.bump();
                        }
                    }
                }
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    fn parse_function(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FUNC_DECL,
            forward_parent: None,
        });
        self.bump(); // function
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_params();
        if self.peek() == Some(tk::COLON) {
            self.parse_type_annotation();
        }
        if self.peek() == Some(tk::LBRACE) {
            self.parse_block_stmt();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_params(&mut self) {
        self.events.push(Event::Start {
            kind: nk::PARAM_LIST,
            forward_parent: None,
        });
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        loop {
            match self.peek() {
                Some(tk::RPAREN) => {
                    self.bump();
                    break;
                }
                None => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                _ => {
                    self.parse_param();
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_param(&mut self) {
        self.events.push(Event::Start {
            kind: nk::PARAM,
            forward_parent: None,
        });
        if self.peek() == Some(tk::ELLIPSIS) {
            self.bump(); // rest parameter
        }
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() == Some(tk::QUESTION) {
            self.bump(); // optional
        }
        if self.peek() == Some(tk::COLON) {
            self.parse_type_annotation();
        }
        if self.peek() == Some(tk::EQUALS) {
            self.bump(); // default
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_type_annotation(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TYPE_ANNOTATION,
            forward_parent: None,
        });
        self.bump(); // :
        self.parse_type();
        self.events.push(Event::Finish);
    }

    /// Type grammar: union level → postfix (generics, arrays) → primary.
    fn parse_type(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TYPE_NODE,
            forward_parent: None,
        });
        self.parse_type_primary();
        // Union `|` chain (and the `=>` of function types via the primary).
        while self.peek() == Some(tk::PIPE) {
            self.bump();
            self.parse_type_primary();
        }
        self.events.push(Event::Finish);
    }

    fn parse_type_primary(&mut self) {
        match self.peek() {
            Some(tk::NAME) => {
                self.bump();
                // Generic arguments `T<A, B>` — the `>>`-split rule makes
                // the closing brackets single GT tokens.
                if self.peek() == Some(tk::LT) {
                    self.bump();
                    loop {
                        match self.peek() {
                            Some(tk::GT) => {
                                self.bump();
                                break;
                            }
                            None => {
                                let start = self.next_start();
                                self.fail_skip(start);
                                break;
                            }
                            _ => {
                                self.parse_type();
                                if self.peek() == Some(tk::COMMA) {
                                    self.bump();
                                }
                            }
                        }
                    }
                }
                // Array suffixes `T[]`.
                while self.peek() == Some(tk::LBRACK) && self.peek2() == Some(tk::RBRACK) {
                    self.bump();
                    self.bump();
                }
            }
            Some(tk::LPAREN) => {
                // Function type `(a: X) => Y` or parenthesized type.
                self.bump();
                loop {
                    match self.peek() {
                        Some(tk::RPAREN) => {
                            self.bump();
                            break;
                        }
                        None => break,
                        _ => {
                            if self.peek() == Some(tk::NAME) {
                                self.bump();
                            }
                            if self.peek() == Some(tk::COLON) {
                                self.parse_type_annotation();
                            }
                            if self.peek() == Some(tk::COMMA) {
                                self.bump();
                            }
                        }
                    }
                }
                if self.peek() == Some(tk::FAT_ARROW) {
                    self.bump();
                    self.parse_type();
                }
            }
            Some(tk::LBRACE) => {
                self.parse_type_body(nk::INTERFACE_MEMBER);
            }
            Some(tk::LBRACK) => {
                // Tuple type `[A, B]`.
                self.bump();
                loop {
                    match self.peek() {
                        Some(tk::RBRACK) => {
                            self.bump();
                            break;
                        }
                        None => break,
                        _ => {
                            self.parse_type();
                            if self.peek() == Some(tk::COMMA) {
                                self.bump();
                            }
                        }
                    }
                }
            }
            _ => {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn parse_var_decl(&mut self) {
        self.events.push(Event::Start {
            kind: nk::VAR_DECL,
            forward_parent: None,
        });
        self.bump(); // const / let / var
        if self.peek() == Some(tk::NAME) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::COLON) {
            self.parse_type_annotation();
        }
        if self.peek() == Some(tk::EQUALS) {
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IF_STMT,
            forward_parent: None,
        });
        self.bump();
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start);
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
        self.parse_statement();
        if self.peek() == Some(tk::KW_ELSE) {
            self.events.push(Event::Start {
                kind: nk::ELSE_CLAUSE,
                forward_parent: None,
            });
            self.bump();
            self.parse_statement();
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    fn parse_while(&mut self) {
        self.events.push(Event::Start {
            kind: nk::WHILE_STMT,
            forward_parent: None,
        });
        self.bump();
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start);
            }
            if self.peek() == Some(tk::RPAREN) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        self.parse_statement();
        self.events.push(Event::Finish);
    }

    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FOR_STMT,
            forward_parent: None,
        });
        self.bump();
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
        }
        if matches!(self.peek(), Some(tk::KW_CONST | tk::KW_LET | tk::KW_VAR)) {
            self.bump(); // loop-head declaration
        }
        if self.peek() == Some(tk::NAME) {
            self.bump();
        }
        if self.peek() == Some(tk::KW_OF) {
            self.bump();
        }
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::RPAREN) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_statement();
        self.events.push(Event::Finish);
    }

    fn parse_return(&mut self) {
        self.events.push(Event::Start {
            kind: nk::RETURN_STMT,
            forward_parent: None,
        });
        self.bump(); // return
        // Restricted production: a newline (or `;`/`}`) ends the return.
        if !matches!(self.peek(), None | Some(tk::SEMI | tk::RBRACE))
            && !self.at_new_line()
            && !self.parse_expression(0)
        {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    fn parse_block_stmt(&mut self) {
        self.events.push(Event::Start {
            kind: nk::BLOCK,
            forward_parent: None,
        });
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                self.parse_statement();
            }
            if self.peek() == Some(tk::RBRACE) {
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

    fn parse_expr_statement(&mut self) {
        self.events.push(Event::Start {
            kind: nk::EXPR_STMT,
            forward_parent: None,
        });
        if !self.parse_expression(0) {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::EQUALS) {
            self.events.push(Event::Start {
                kind: nk::ASSIGN,
                forward_parent: None,
            });
            self.bump();
            if !self.parse_expression(0) {
                let start = self.next_start();
                self.fail_skip(start);
            }
            self.events.push(Event::Finish);
        }
        self.consume_terminator();
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Expressions (Pratt)
    // ------------------------------------------------------------------

    fn parse_expression(&mut self, min_bp: u8) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        // Prefix unaries.
        if matches!(kind, tk::BANG | tk::MINUS | tk::PLUS | tk::TILDE) {
            self.events.push(Event::Start {
                kind: nk::UNARY_EXPR,
                forward_parent: None,
            });
            self.bump();
            if !self.parse_expression(11) {
                self.events.pop();
                return false;
            }
            self.events.push(Event::Finish);
        } else if !self.parse_atom() {
            return false;
        }
        self.parse_postfix(min_bp);
        true
    }

    /// The climb: postfix ops at [`POSTFIX_BP`], infix rows, and the
    /// `>>`-split shift combination (two/three adjacent `>` or `<`).
    fn parse_postfix(&mut self, min_bp: u8) {
        while let Some(kind) = self.peek() {
            if kind == tk::LPAREN {
                self.events.push(Event::Start {
                    kind: nk::CALL,
                    forward_parent: None,
                });
                self.bump();
                loop {
                    match self.peek() {
                        Some(tk::RPAREN) => {
                            self.bump();
                            break;
                        }
                        None => break,
                        _ => {
                            if !self.parse_expression(0) {
                                let start = self.next_start();
                                self.fail_skip(start);
                                break;
                            }
                            if self.peek() == Some(tk::COMMA) {
                                self.bump();
                            }
                        }
                    }
                }
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
                    self.fail_skip(start);
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
                    self.fail_skip(start);
                }
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
            } else if (kind == tk::GT || kind == tk::LT) && min_bp <= 8 {
                // The split shift: combine adjacent `>` (or `<`) tokens
                // into one node — the Start comes FIRST so the operator
                // tokens stay inside it.
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
                    forward_parent: None,
                });
                let op_kind = self.feed[self.pos].kind;
                let mut count = 0;
                while self.peek() == Some(op_kind) && count < 3 {
                    self.bump();
                    count += 1;
                }
                if !self.parse_expression(9) {
                    self.events.pop();
                    break;
                }
                self.events.push(Event::Finish);
            } else if let Some((_, lbp, rbp, node)) =
                INFIX.iter().find(|(token, _, _, _)| *token == kind)
            {
                if *lbp < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: *node,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_expression(*rbp) {
                    self.events.pop();
                    break;
                }
                self.events.push(Event::Finish);
            } else {
                break;
            }
        }
    }

    fn parse_atom(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::NAME
            | tk::NUMBER
            | tk::STRING
            | tk::KW_TRUE
            | tk::KW_FALSE
            | tk::KW_NULL
            | tk::KW_UNDEFINED => {
                self.bump();
                true
            }
            tk::TEMPLATE_START => {
                self.parse_template();
                true
            }
            tk::LPAREN => {
                self.events.push(Event::Start {
                    kind: nk::GROUP_EXPR,
                    forward_parent: None,
                });
                self.bump();
                // Read-only lookahead: `(params) => body` (with an
                // optional return annotation) parses as params + arrow;
                // anything else is a grouped expression.
                if self.arrow_after_parens() {
                    self.parse_arrow_params();
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    if self.peek() == Some(tk::COLON) {
                        self.parse_type_annotation();
                    }
                    self.events.push(Event::Start {
                        kind: nk::ARROW_FUNC,
                        forward_parent: None,
                    });
                    self.bump(); // =>
                    if self.peek() == Some(tk::LBRACE) {
                        self.parse_block_stmt();
                    } else if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    self.events.push(Event::Finish);
                    // The `)` was consumed before the arrow: the group
                    // closes directly over params + arrow + body.
                    self.events.push(Event::Finish);
                    return true;
                }
                if !self.parse_expression(0) {
                    let start = self.next_start();
                    self.fail_skip(start);
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
            tk::LBRACK => {
                self.events.push(Event::Start {
                    kind: nk::ARRAY_LIT,
                    forward_parent: None,
                });
                self.bump();
                loop {
                    match self.peek() {
                        Some(tk::RBRACK) => {
                            self.bump();
                            break;
                        }
                        None => {
                            let start = self.next_start();
                            self.fail_skip(start);
                            break;
                        }
                        _ => {
                            if !self.parse_expression(0) {
                                let start = self.next_start();
                                self.fail_skip(start);
                            }
                            if self.peek() == Some(tk::COMMA) {
                                self.bump();
                            }
                        }
                    }
                }
                self.events.push(Event::Finish);
                true
            }
            tk::LBRACE => self.parse_object_lit(),
            _ => false,
        }
    }

    /// `{ key: value, … }` object literal.
    fn parse_object_lit(&mut self) -> bool {
        self.events.push(Event::Start {
            kind: nk::OBJECT_LIT,
            forward_parent: None,
        });
        self.bump();
        loop {
            match self.peek() {
                Some(tk::RBRACE) => {
                    self.bump();
                    break;
                }
                None => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                _ => {
                    if matches!(self.peek(), Some(tk::NAME | tk::STRING)) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::COLON) {
                        self.bump();
                        if !self.parse_expression(0) {
                            let start = self.next_start();
                            self.fail_skip(start);
                        }
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
        self.events.push(Event::Finish);
        true
    }

    /// Read-only lookahead from just after a `(`: does the matching `)`
    /// (optionally followed by a `: Type` return annotation) come with a
    /// `=>`? Bounded scan, deterministic.
    fn arrow_after_parens(&self) -> bool {
        let mut depth = 1_usize;
        let mut idx = self.pos;
        while let Some(token) = self.feed.get(idx) {
            match token.kind {
                tk::LPAREN | tk::LBRACK | tk::LBRACE => depth += 1,
                tk::RPAREN | tk::RBRACK | tk::RBRACE => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            idx += 1;
        }
        // After the matching `)` an annotation may precede the `=>`.
        let mut probe = idx + 1;
        while let Some(token) = self.feed.get(probe) {
            match token.kind {
                tk::FAT_ARROW => return true,
                tk::SEMI | tk::LBRACE | tk::COMMA => return false,
                _ => {}
            }
            probe += 1;
            if probe > idx + 64 {
                return false; // bounded lookahead
            }
        }
        false
    }

    /// Arrow parameter list inside `(` … `)`: names with optional `:`
    /// annotations; stops at `)` WITHOUT consuming it (the caller owns
    /// the closer for both the arrow and the grouped-expression shapes).
    fn parse_arrow_params(&mut self) {
        loop {
            match self.peek() {
                Some(tk::RPAREN) | None => break,
                _ => {
                    if self.peek() == Some(tk::NAME) {
                        self.bump();
                    }
                    if self.peek() == Some(tk::COLON) {
                        self.parse_type_annotation();
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    } else if self.peek() != Some(tk::RPAREN) {
                        break;
                    }
                }
            }
        }
    }

    /// `` `text ${expr} text` `` — the lexer's mode stack feeds the parts;
    /// nested templates recurse through the atom rule.
    fn parse_template(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TEMPLATE_EXPR,
            forward_parent: None,
        });
        self.bump(); // TEMPLATE_START
        loop {
            match self.peek() {
                Some(tk::TEMPLATE_TEXT) => {
                    self.bump();
                }
                Some(tk::TEMPLATE_DOLLAR) => {
                    self.bump();
                    if !self.parse_expression(0) {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    if self.peek() == Some(tk::RBRACE) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                }
                Some(tk::TEMPLATE_END) => {
                    self.bump();
                    break;
                }
                _ => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
            }
        }
        self.events.push(Event::Finish);
    }
}

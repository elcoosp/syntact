//! Hand-written reference parser for the documented Rust subset
//! (issue 033): items with generics/where clauses, Pratt expressions
//! with closures and ranges, patterns, attributes, macro token trees.

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

/// The recovery sync set: recovery stops before these tokens and
/// they never ride a stray error region.
const SYNC: &[u16] = &[
    tk::SEMI,
    tk::COMMA,
    tk::RBRACE,
    tk::RPAREN,
    tk::RBRACK,
    tk::LBRACE,
    tk::FATARROW,
];

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse Rust source into the raw event stream:
/// `(events, token_ranges, error_count)`.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse Rust source into a green tree (one implicit root).
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
    /// The last parsed atom was block-like (`if`/`match`/block/…) —
    /// expression statements then need no `;`.
    block_like: bool,
    /// Depth guard: heads of `if`/`while`/`match`/`for` forbid struct
    /// literals so `{` always opens the body/arms.
    no_struct: usize,
}

/// Pratt binding-power table rows.
struct OpRow {
    token: u16,
    lbp: u8,
    rbp: u8,
    node: u16,
}

/// The infix operator table (`rbp == lbp + 1` left-assoc; assignment
/// `rbp < lbp` right-assoc; ranges below `||`; `as` tightest).
const INFIX: &[OpRow] = &[
    OpRow {
        token: tk::ASSIGN,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PLUSEQ,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::MINUSEQ,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::STAREQ,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SLASHEQ,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PERCENTEQ,
        lbp: 2,
        rbp: 1,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::DOTDOT,
        lbp: 3,
        rbp: 4,
        node: nk::RANGE_EXPR,
    },
    OpRow {
        token: tk::DOTDOTEQ,
        lbp: 3,
        rbp: 4,
        node: nk::RANGE_EXPR,
    },
    OpRow {
        token: tk::OROR,
        lbp: 4,
        rbp: 5,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::ANDAND,
        lbp: 5,
        rbp: 6,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::EQ,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::NE,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LT,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GT,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::LE,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::GE,
        lbp: 6,
        rbp: 7,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PIPE,
        lbp: 7,
        rbp: 8,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::CARET,
        lbp: 8,
        rbp: 9,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::AMP,
        lbp: 9,
        rbp: 10,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SHL,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SHR,
        lbp: 10,
        rbp: 11,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PLUS,
        lbp: 11,
        rbp: 12,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::MINUS,
        lbp: 11,
        rbp: 12,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::STAR,
        lbp: 12,
        rbp: 13,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::SLASH,
        lbp: 12,
        rbp: 13,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::PERCENT,
        lbp: 12,
        rbp: 13,
        node: nk::BIN_EXPR,
    },
    OpRow {
        token: tk::KW_AS,
        lbp: 13,
        rbp: 14,
        node: nk::CAST_EXPR,
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
            block_like: false,
            no_struct: 0,
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

    /// Error + ERROR region around the offending tokens; sync = `;`
    /// `,` closers and `{` (statement-local recovery). Always
    /// consumes ≥ 1 token (progress).
    fn fail_skip(&mut self, start: u32) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        let pos0 = self.pos;
        while let Some(token) = self.feed.get(self.pos) {
            if SYNC.contains(&token.kind) {
                break;
            }
            self.pos += 1;
        }
        let at_sync = self
            .feed
            .get(self.pos)
            .is_some_and(|t| SYNC.contains(&t.kind));
        if self.pos == pos0 && self.pos < self.feed.len() && !at_sync {
            self.pos += 1; // a non-sync stray rides the error region
        }
        let end = self
            .feed
            .get(self.pos.saturating_sub(1))
            .filter(|_| self.pos > pos0)
            .map_or(start, |t| t.range.end);
        if end > start {
            // The error region covers inter-token bytes the scanner
            // already filed as trivia — drop those records (every
            // byte is covered exactly once: losslessness).
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

    /// Recovery that force-consumes one stray token (module/block
    /// level: stray closers must ride the error region for progress).
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

    fn starts_expr(kind: Option<u16>) -> bool {
        matches!(
            kind,
            Some(
                tk::IDENT
                    | tk::INT_LIT
                    | tk::FLOAT_LIT
                    | tk::STRING_LIT
                    | tk::CHAR_LIT
                    | tk::MINUS
                    | tk::BANG
                    | tk::STAR
                    | tk::AMP
                    | tk::ANDAND
                    | tk::PIPE
                    | tk::OROR
                    | tk::DOTDOT
                    | tk::DOTDOTEQ
                    | tk::LPAREN
                    | tk::LBRACK
                    | tk::LBRACE
                    | tk::KW_IF
                    | tk::KW_MATCH
                    | tk::KW_LOOP
                    | tk::KW_WHILE
                    | tk::KW_FOR
                    | tk::KW_RETURN
                    | tk::KW_BREAK
                    | tk::KW_CONTINUE
                    | tk::KW_TRUE
                    | tk::KW_FALSE
                    | tk::KW_MOVE
                    | tk::KW_UNSAFE
                    | tk::KW_SELF
                    | tk::KW_CRATE
                    | tk::KW_SUPER
            )
        )
    }

    // ------------------------------------------------------------------
    // Module / blocks
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        loop {
            match self.peek() {
                None => break,
                Some(tk::SEMI) => {
                    self.bump();
                }
                Some(tk::POUND) => self.parse_attr(),
                Some(tk::COMMA | tk::RPAREN | tk::RBRACK | tk::RBRACE) => {
                    let start = self.next_start();
                    self.fail_skip_stray(start);
                }
                _ => self.parse_item(),
            }
        }
    }

    fn parse_block(&mut self) -> bool {
        if self.peek() != Some(tk::LBRACE) {
            let start = self.next_start();
            self.fail_skip(start);
            return false;
        }
        self.events.push(Event::Start {
            kind: nk::BLOCK,
            forward_parent: None,
        });
        self.bump();
        loop {
            match self.peek() {
                Some(tk::RBRACE) | None => break,
                Some(tk::SEMI) => {
                    self.bump();
                }
                Some(tk::POUND) => self.parse_attr(),
                Some(tk::COMMA | tk::RPAREN | tk::RBRACK) => {
                    let start = self.next_start();
                    self.fail_skip_stray(start);
                }
                _ => {
                    if self.peek_item_keyword() {
                        self.parse_item();
                    } else {
                        self.parse_statement();
                    }
                }
            }
        }
        if self.peek() == Some(tk::RBRACE) {
            self.bump();
        }
        self.events.push(Event::Finish);
        true
    }

    fn peek_item_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                tk::KW_FN
                    | tk::KW_STRUCT
                    | tk::KW_ENUM
                    | tk::KW_IMPL
                    | tk::KW_TRAIT
                    | tk::KW_USE
                    | tk::KW_MOD
                    | tk::KW_CONST
                    | tk::KW_STATIC
                    | tk::KW_TYPE
                    | tk::KW_PUB
            )
        )
    }

    // ------------------------------------------------------------------
    // Attributes
    // ------------------------------------------------------------------

    /// `#[ … ]` with balanced inner brackets (ATTR + token payload).
    fn parse_attr(&mut self) {
        self.events.push(Event::Start {
            kind: nk::ATTR,
            forward_parent: None,
        });
        self.bump(); // #
        if self.peek() == Some(tk::BANG) {
            self.bump(); // inner attribute `#![…]`
        }
        if matches!(self.peek(), Some(tk::LBRACK)) {
            self.parse_token_tree_group();
        } else if self.peek().is_some() {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    /// One balanced group — `(...)`, `[…]` or `{…}` — recursive;
    /// nested groups become their own `TOKEN_TREE` nodes.
    fn parse_token_tree_group(&mut self) {
        let opener = self.peek();
        self.events.push(Event::Start {
            kind: nk::TOKEN_TREE,
            forward_parent: None,
        });
        self.bump(); // the opener
        let closer = match opener {
            Some(tk::LPAREN) => tk::RPAREN,
            Some(tk::LBRACK) => tk::RBRACK,
            Some(tk::LBRACE) => tk::RBRACE,
            _ => {
                self.events.push(Event::Finish);
                return;
            }
        };
        while let Some(kind) = self.peek() {
            if matches!(kind, tk::LPAREN | tk::LBRACK | tk::LBRACE) {
                self.parse_token_tree_group();
            } else if kind == closer {
                self.bump();
                break;
            } else if matches!(kind, tk::RPAREN | tk::RBRACK | tk::RBRACE) {
                // Mismatched closer: bail (the caller's loop regains
                // control; progress was made by the opener).
                break;
            } else {
                self.bump();
            }
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Items
    // ------------------------------------------------------------------

    fn parse_item(&mut self) {
        // Attributes ride as sibling ATTR nodes inside the parent.
        while self.peek() == Some(tk::POUND) {
            self.parse_attr();
        }
        if self.peek() == Some(tk::KW_PUB) {
            self.bump();
            if self.peek() == Some(tk::LPAREN) {
                // `pub(crate)` / `pub(in path)`: bare tokens to `)`.
                self.bump();
                let mut depth = 1_usize;
                while let Some(kind) = self.peek() {
                    if kind == tk::LPAREN {
                        depth += 1;
                    }
                    if kind == tk::RPAREN {
                        depth -= 1;
                        self.bump();
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    self.bump();
                }
            }
        }
        match self.peek() {
            Some(tk::KW_FN) => self.parse_fn(),
            Some(tk::KW_STRUCT) => self.parse_struct(),
            Some(tk::KW_ENUM) => self.parse_enum(),
            Some(tk::KW_IMPL) => self.parse_impl(),
            Some(tk::KW_TRAIT) => self.parse_trait(),
            Some(tk::KW_USE) => self.parse_use(),
            Some(tk::KW_MOD) => self.parse_mod(),
            Some(tk::KW_CONST | tk::KW_STATIC) => self.parse_const(),
            Some(tk::KW_TYPE) => self.parse_type_alias(),
            _ => {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
    }

    fn parse_type_params(&mut self) -> bool {
        if self.peek() != Some(tk::LT) {
            return false;
        }
        self.events.push(Event::Start {
            kind: nk::TYPE_PARAMS,
            forward_parent: None,
        });
        self.bump();
        loop {
            match self.peek() {
                Some(tk::GT) => {
                    self.bump();
                    break;
                }
                None | Some(tk::LBRACE | tk::SEMI) => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                _ => {
                    // Lifetime or type parameter with optional bounds.
                    if !matches!(self.peek(), Some(tk::LIFETIME | tk::IDENT)) {
                        let start = self.next_start();
                        self.fail_skip(start);
                        break;
                    }
                    self.bump();
                    if self.peek() == Some(tk::COLON) {
                        self.bump();
                        self.parse_bounds();
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

    /// `B1 + B2 + 'a` bounds.
    fn parse_bounds(&mut self) {
        loop {
            match self.peek() {
                Some(tk::IDENT) => {
                    self.bump();
                    while self.peek() == Some(tk::COLONCOLON) && self.peek_at(1) == Some(tk::IDENT)
                    {
                        self.bump();
                        self.bump();
                    }
                }
                Some(tk::LIFETIME) => {
                    self.bump();
                }
                _ => break,
            }
            if self.peek() == Some(tk::PLUS) {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn parse_where(&mut self) -> bool {
        if self.peek() != Some(tk::KW_WHERE) {
            return false;
        }
        self.events.push(Event::Start {
            kind: nk::WHERE_CLAUSE,
            forward_parent: None,
        });
        self.bump();
        loop {
            if !matches!(
                self.peek(),
                Some(tk::IDENT | tk::AMP | tk::LBRACK | tk::LPAREN)
            ) {
                break;
            }
            self.parse_type();
            if self.peek() == Some(tk::COLON) {
                self.bump();
                self.parse_bounds();
            }
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        self.events.push(Event::Finish);
        true
    }

    fn parse_fn(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FN_DEF,
            forward_parent: None,
        });
        self.bump(); // fn
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_params();
        self.parse_param_list();
        if self.peek() == Some(tk::ARROW) {
            self.bump();
            self.parse_type();
        }
        self.parse_where();
        if self.peek() == Some(tk::LBRACE) {
            self.parse_block();
        } else if self.peek() == Some(tk::SEMI) {
            self.bump(); // trait signature
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_param_list(&mut self) {
        if self.peek() != Some(tk::LPAREN) {
            let start = self.next_start();
            self.fail_skip(start);
            return;
        }
        self.events.push(Event::Start {
            kind: nk::PARAM_LIST,
            forward_parent: None,
        });
        self.bump();
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
                    // `self`, `&self`, `&mut self`, `mut pat: Type`.
                    if self.peek() == Some(tk::AMP) {
                        self.bump();
                        if self.peek() == Some(tk::LIFETIME) {
                            self.bump();
                        }
                        if self.peek() == Some(tk::KW_MUT) {
                            self.bump();
                        }
                        if self.peek() == Some(tk::KW_SELF) {
                            self.bump();
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start);
                            break;
                        }
                    } else if self.peek() == Some(tk::KW_SELF) {
                        self.bump();
                    } else {
                        if self.peek() == Some(tk::KW_MUT) {
                            self.bump();
                        }
                        if !self.parse_pattern() {
                            let start = self.next_start();
                            self.fail_skip(start);
                            break;
                        }
                        if self.peek() == Some(tk::COLON) {
                            self.bump();
                            self.parse_type();
                        }
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_struct(&mut self) {
        self.events.push(Event::Start {
            kind: nk::STRUCT_DEF,
            forward_parent: None,
        });
        self.bump(); // struct
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_params();
        match self.peek() {
            Some(tk::LPAREN) => {
                self.parse_tuple_struct_body();
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                }
            }
            Some(tk::LBRACE) => {
                self.bump();
                while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                    self.parse_field();
                }
                if self.peek() == Some(tk::RBRACE) {
                    self.bump();
                }
            }
            _ => {
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_tuple_struct_body(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TUPLE_STRUCT_BODY,
            forward_parent: None,
        });
        self.bump(); // (
        while !matches!(self.peek(), Some(tk::RPAREN) | None) {
            while self.peek() == Some(tk::POUND) {
                self.parse_attr();
            }
            if self.peek() == Some(tk::KW_PUB) {
                self.bump();
            }
            self.parse_type();
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some(tk::RPAREN) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn parse_field(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FIELD,
            forward_parent: None,
        });
        while self.peek() == Some(tk::POUND) {
            self.parse_attr();
        }
        if self.peek() == Some(tk::KW_PUB) {
            self.bump();
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::COLON) {
            self.bump();
            self.parse_type();
        }
        if self.peek() == Some(tk::COMMA) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn parse_enum(&mut self) {
        self.events.push(Event::Start {
            kind: nk::ENUM_DEF,
            forward_parent: None,
        });
        self.bump(); // enum
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_params();
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                self.parse_enum_variant();
            }
            if self.peek() == Some(tk::RBRACE) {
                self.bump();
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_enum_variant(&mut self) {
        self.events.push(Event::Start {
            kind: nk::ENUM_VARIANT,
            forward_parent: None,
        });
        while self.peek() == Some(tk::POUND) {
            self.parse_attr();
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        match self.peek() {
            Some(tk::LPAREN) => self.parse_tuple_struct_body(),
            Some(tk::LBRACE) => {
                self.bump();
                while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                    self.parse_field();
                }
                if self.peek() == Some(tk::RBRACE) {
                    self.bump();
                }
            }
            _ => {}
        }
        if self.peek() == Some(tk::COMMA) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn parse_impl(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IMPL_DEF,
            forward_parent: None,
        });
        self.bump(); // impl
        self.parse_type_params();
        // `Trait for Type` or bare `Type`.
        self.parse_type();
        if self.peek() == Some(tk::KW_FOR) {
            self.bump();
            self.parse_type();
        }
        self.parse_where();
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                } else {
                    self.parse_item();
                }
            }
            if self.peek() == Some(tk::RBRACE) {
                self.bump();
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_trait(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TRAIT_DEF,
            forward_parent: None,
        });
        self.bump(); // trait
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_params();
        if self.peek() == Some(tk::COLON) {
            self.bump();
            self.parse_bounds();
        }
        self.parse_where();
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                } else {
                    self.parse_item();
                }
            }
            if self.peek() == Some(tk::RBRACE) {
                self.bump();
            }
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_use(&mut self) {
        self.events.push(Event::Start {
            kind: nk::USE_DECL,
            forward_parent: None,
        });
        self.bump(); // use
        self.parse_use_tree();
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    /// `a::b::{c, d as e, *}` (recursive brace groups).
    fn parse_use_tree(&mut self) {
        self.events.push(Event::Start {
            kind: nk::USE_TREE,
            forward_parent: None,
        });
        if self.peek() == Some(tk::STAR) {
            self.bump();
        } else if self.peek() == Some(tk::LBRACE) {
            self.parse_use_group();
        } else {
            // Path segments (leading `::` allowed).
            if self.peek() == Some(tk::COLONCOLON) {
                self.bump();
            }
            loop {
                if matches!(
                    self.peek(),
                    Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                ) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                if self.peek() == Some(tk::COLONCOLON) {
                    if matches!(self.peek_at(1), Some(tk::LBRACE)) {
                        self.bump();
                        self.parse_use_group();
                        break;
                    }
                    if self.peek_at(1) == Some(tk::STAR) {
                        self.bump();
                        self.bump();
                        break;
                    }
                    self.bump();
                    continue;
                }
                if self.peek() == Some(tk::KW_AS) && self.peek_at(1) == Some(tk::IDENT) {
                    self.bump();
                    self.bump();
                }
                break;
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_use_group(&mut self) {
        self.bump(); // {
        while !matches!(self.peek(), Some(tk::RBRACE) | None) {
            self.parse_use_tree();
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some(tk::RBRACE) {
            self.bump();
        }
    }

    fn parse_mod(&mut self) {
        self.events.push(Event::Start {
            kind: nk::MOD_DECL,
            forward_parent: None,
        });
        self.bump(); // mod
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                } else {
                    self.parse_item();
                }
            }
            if self.peek() == Some(tk::RBRACE) {
                self.bump();
            }
        } else if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_const(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CONST_DEF,
            forward_parent: None,
        });
        self.bump(); // const / static
        if self.peek() == Some(tk::KW_MUT) {
            self.bump();
        }
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::COLON) {
            self.bump();
            self.parse_type();
        }
        if self.peek() == Some(tk::ASSIGN) {
            self.bump();
            if Self::starts_expr(self.peek()) {
                self.parse_expression(0);
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_type_alias(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TYPE_ALIAS,
            forward_parent: None,
        });
        self.bump(); // type
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.parse_type_params();
        if self.peek() == Some(tk::ASSIGN) {
            self.bump();
            self.parse_type();
        }
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn parse_statement(&mut self) {
        match self.peek() {
            Some(tk::KW_LET) => self.parse_let(),
            Some(tk::KW_RETURN) => self.parse_returnish(nk::RETURN_EXPR),
            Some(tk::KW_BREAK) => self.parse_returnish(nk::BREAK_EXPR),
            Some(tk::KW_CONTINUE) => self.parse_returnish(nk::CONTINUE_EXPR),
            Some(tk::POUND) => {
                self.parse_attr();
                self.parse_statement();
            }
            _ => self.parse_expr_statement(),
        }
    }

    fn parse_let(&mut self) {
        self.events.push(Event::Start {
            kind: nk::LET_STMT,
            forward_parent: None,
        });
        self.bump(); // let
        if !self.parse_pattern() {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::COLON) {
            self.bump();
            self.parse_type();
        }
        if self.peek() == Some(tk::ASSIGN) {
            self.bump();
            if Self::starts_expr(self.peek()) {
                self.parse_expression(0);
            } else {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_returnish(&mut self, node: u16) {
        self.events.push(Event::Start {
            kind: node,
            forward_parent: None,
        });
        self.bump(); // return / break / continue
        if node == nk::BREAK_EXPR && self.peek() == Some(tk::LIFETIME) {
            self.bump(); // loop label
        }
        if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        }
        self.finish_statement();
        self.events.push(Event::Finish);
    }

    /// `;` required unless the expression was block-like or the block
    /// / group closes here.
    fn finish_statement(&mut self) {
        match self.peek() {
            Some(tk::SEMI) => {
                self.bump();
            }
            Some(tk::RBRACE) | None => {}
            _ if self.block_like => {}
            _ => {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
    }

    fn parse_expr_statement(&mut self) {
        self.events.push(Event::Start {
            kind: nk::EXPR_STMT,
            forward_parent: None,
        });
        if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        } else {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        self.finish_statement();
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Expressions (Pratt)
    // ------------------------------------------------------------------

    fn parse_expression(&mut self, min_bp: u8) -> bool {
        if !self.parse_prefix() {
            return false;
        }
        // Climb.
        while let Some(kind) = self.peek() {
            if kind == tk::LPAREN {
                self.events.push(Event::Start {
                    kind: nk::CALL,
                    forward_parent: None,
                });
                self.bump();
                self.parse_call_args(tk::RPAREN);
                self.events.push(Event::Finish);
                self.block_like = false;
                continue;
            }
            if kind == tk::DOT {
                // Method call `.m(…)` vs field `.m` / `.0`.
                let name = self.peek_at(1);
                let is_method = name == Some(tk::IDENT) || name == Some(tk::INT_LIT);
                let has_args = is_method && self.peek_at(2) == Some(tk::LPAREN);
                if has_args {
                    self.events.push(Event::Start {
                        kind: nk::CALL,
                        forward_parent: None,
                    });
                    self.bump(); // .
                    self.bump(); // name
                    self.bump(); // (
                    self.parse_call_args(tk::RPAREN);
                    self.events.push(Event::Finish);
                } else {
                    self.events.push(Event::Start {
                        kind: nk::FIELD_EXPR,
                        forward_parent: None,
                    });
                    self.bump(); // .
                    if matches!(self.peek(), Some(tk::IDENT | tk::INT_LIT)) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    self.events.push(Event::Finish);
                }
                self.block_like = false;
                continue;
            }
            if kind == tk::LBRACK {
                self.events.push(Event::Start {
                    kind: nk::INDEX_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(0);
                }
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                self.block_like = false;
                continue;
            }
            if kind == tk::COLONCOLON && self.peek_at(1) == Some(tk::LT) {
                // Turbofish `::<T, U>`.
                self.events.push(Event::Start {
                    kind: nk::GENERIC_ARGS,
                    forward_parent: None,
                });
                self.bump();
                self.bump();
                self.parse_generic_arg_list();
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
                self.block_like = false;
                continue;
            }
            break;
        }
        true
    }

    fn parse_generic_arg_list(&mut self) {
        loop {
            match self.peek() {
                Some(tk::GT) => {
                    self.bump();
                    break;
                }
                None | Some(tk::LPAREN | tk::SEMI | tk::LBRACE) => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                _ => {
                    if matches!(self.peek(), Some(tk::LIFETIME)) {
                        self.bump();
                    } else {
                        self.parse_type();
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
    }

    fn parse_call_args(&mut self, closer: u16) {
        loop {
            match self.peek() {
                Some(k) if k == closer => {
                    self.bump();
                    break;
                }
                None => {
                    let start = self.next_start();
                    self.fail_skip(start);
                    break;
                }
                _ => {
                    if Self::starts_expr(self.peek()) {
                        self.parse_expression(0);
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    }
                }
            }
        }
    }

    fn parse_prefix(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::MINUS | tk::BANG | tk::STAR | tk::AMP | tk::ANDAND => {
                self.events.push(Event::Start {
                    kind: nk::UNARY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if kind == tk::AMP && self.peek() == Some(tk::KW_MUT) {
                    self.bump();
                }
                if !self.parse_expression(13) {
                    self.events.pop(); // drop the unary Start
                    return false;
                }
                self.events.push(Event::Finish);
                self.block_like = false;
                true
            }
            tk::DOTDOT | tk::DOTDOTEQ => {
                self.events.push(Event::Start {
                    kind: nk::RANGE_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(3);
                }
                self.events.push(Event::Finish);
                self.block_like = false;
                true
            }
            tk::PIPE | tk::OROR => {
                self.parse_closure();
                true
            }
            tk::KW_MOVE => {
                self.events.push(Event::Start {
                    kind: nk::CLOSURE_EXPR,
                    forward_parent: None,
                });
                self.bump(); // move
                if matches!(self.peek(), Some(tk::PIPE | tk::OROR)) {
                    self.parse_closure_params();
                }
                if !Self::starts_expr(self.peek()) || !self.parse_expression(0) {
                    self.events.push(Event::Finish);
                    return true;
                }
                self.events.push(Event::Finish);
                true
            }
            tk::KW_IF => {
                self.parse_if();
                true
            }
            tk::KW_MATCH => {
                self.parse_match();
                true
            }
            tk::KW_LOOP | tk::KW_WHILE | tk::KW_FOR => {
                self.parse_loop(kind);
                true
            }
            tk::KW_UNSAFE => {
                self.events.push(Event::Start {
                    kind: nk::BLOCK,
                    forward_parent: None,
                });
                self.bump();
                self.parse_block();
                self.events.push(Event::Finish);
                self.block_like = true;
                true
            }
            tk::LBRACE => {
                self.parse_block();
                self.block_like = true;
                true
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_closure(&mut self) {
        self.events.push(Event::Start {
            kind: nk::CLOSURE_EXPR,
            forward_parent: None,
        });
        self.parse_closure_params();
        if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        }
        self.events.push(Event::Finish);
        self.block_like = false;
    }

    /// `|params…|` — patterns with optional `: Type`, `,` separated.
    fn parse_closure_params(&mut self) {
        self.bump(); // | or ||
        while !matches!(self.peek(), Some(tk::PIPE) | None) {
            if !self.parse_pattern() {
                break;
            }
            if self.peek() == Some(tk::COLON) {
                self.bump();
                self.parse_type();
            }
            if self.peek() == Some(tk::COMMA) {
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some(tk::PIPE) {
            self.bump();
        }
    }

    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IF_EXPR,
            forward_parent: None,
        });
        self.bump(); // if
        if self.peek() == Some(tk::KW_LET) {
            self.bump();
            if !self.parse_pattern() {
                let start = self.next_start();
                self.fail_skip(start);
            }
            if self.peek() == Some(tk::ASSIGN) {
                self.bump();
                self.no_struct += 1;
                self.parse_expression(0);
                self.no_struct -= 1;
            }
        } else if Self::starts_expr(self.peek()) {
            self.no_struct += 1;
            self.parse_expression(0);
            self.no_struct -= 1;
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if !self.parse_block() {
            self.events.push(Event::Finish);
            return;
        }
        // else chains stay inside the IF_EXPR node.
        if self.peek() == Some(tk::KW_ELSE) {
            self.bump();
            if self.peek() == Some(tk::KW_IF) {
                self.parse_if();
            } else {
                self.parse_block();
            }
        }
        self.events.push(Event::Finish);
        self.block_like = true;
    }

    fn parse_match(&mut self) {
        self.events.push(Event::Start {
            kind: nk::MATCH_EXPR,
            forward_parent: None,
        });
        self.bump(); // match
        if Self::starts_expr(self.peek()) {
            self.no_struct += 1;
            self.parse_expression(0);
            self.no_struct -= 1;
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() != Some(tk::LBRACE) {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            self.block_like = true;
            return;
        }
        self.bump();
        while !matches!(self.peek(), Some(tk::RBRACE) | None) {
            self.parse_match_arm();
        }
        if self.peek() == Some(tk::RBRACE) {
            self.bump();
        }
        self.events.push(Event::Finish);
        self.block_like = true;
    }

    fn parse_match_arm(&mut self) {
        self.events.push(Event::Start {
            kind: nk::MATCH_ARM,
            forward_parent: None,
        });
        // Or-patterns are flat siblings (documented shape).
        if !self.parse_pattern() {
            let start = self.next_start();
            self.fail_skip(start);
        }
        while self.peek() == Some(tk::PIPE) {
            self.bump();
            if !self.parse_pattern() {
                let start = self.next_start();
                self.fail_skip(start);
            }
        }
        if self.peek() == Some(tk::KW_IF) {
            self.events.push(Event::Start {
                kind: nk::GUARD,
                forward_parent: None,
            });
            self.bump();
            self.parse_expression(0);
            self.events.push(Event::Finish);
        }
        if self.peek() == Some(tk::FATARROW) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::LBRACE) {
            self.parse_block();
        } else if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        }
        // Arm separator: `,` optional before `}`.
        if self.peek() == Some(tk::COMMA) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn parse_loop(&mut self, kind: u16) {
        self.events.push(Event::Start {
            kind: nk::LOOP_EXPR,
            forward_parent: None,
        });
        self.bump(); // loop / while / for
        match kind {
            tk::KW_LOOP => {}
            tk::KW_WHILE => {
                if self.peek() == Some(tk::KW_LET) {
                    self.bump();
                    if !self.parse_pattern() {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                    if self.peek() == Some(tk::ASSIGN) {
                        self.bump();
                        self.no_struct += 1;
                        self.parse_expression(0);
                        self.no_struct -= 1;
                    }
                } else if Self::starts_expr(self.peek()) {
                    self.no_struct += 1;
                    self.parse_expression(0);
                    self.no_struct -= 1;
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
            _ => {
                // for PAT in ITER
                if !self.parse_pattern() {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                if self.peek() == Some(tk::KW_IN) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                if Self::starts_expr(self.peek()) {
                    self.no_struct += 1;
                    self.parse_expression(0);
                    self.no_struct -= 1;
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
        }
        if !self.parse_block() {
            self.events.push(Event::Finish);
            self.block_like = true;
            return;
        }
        self.events.push(Event::Finish);
        self.block_like = true;
    }

    fn parse_atom(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        self.block_like = false;
        match kind {
            tk::INT_LIT
            | tk::FLOAT_LIT
            | tk::STRING_LIT
            | tk::CHAR_LIT
            | tk::KW_TRUE
            | tk::KW_FALSE => {
                self.bump();
                true
            }
            tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER => {
                if self.path_then_macro() {
                    self.events.push(Event::Start {
                        kind: nk::MACRO_CALL,
                        forward_parent: None,
                    });
                    self.parse_path_tokens();
                    self.bump(); // !
                    self.parse_token_tree_group();
                    self.events.push(Event::Finish);
                    return true;
                }
                self.parse_path_expr();
                // Struct literal `S { x: 1 }` (forbidden in heads).
                if self.peek() == Some(tk::LBRACE) && self.no_struct == 0 {
                    self.parse_record_body();
                }
                true
            }
            tk::LPAREN => {
                self.events.push(Event::Start {
                    kind: nk::TUPLE_EXPR,
                    forward_parent: None,
                });
                self.bump();
                self.parse_call_args(tk::RPAREN);
                self.events.push(Event::Finish);
                true
            }
            tk::LBRACK => {
                self.events.push(Event::Start {
                    kind: nk::ARRAY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(0);
                    if self.peek() == Some(tk::SEMI) {
                        // `[0; n]` repeat form.
                        self.bump();
                        if Self::starts_expr(self.peek()) {
                            self.parse_expression(0);
                        }
                    } else {
                        while self.peek() == Some(tk::COMMA) {
                            self.bump();
                            if self.peek() == Some(tk::RBRACK) {
                                break;
                            }
                            if Self::starts_expr(self.peek()) {
                                self.parse_expression(0);
                            } else {
                                let start = self.next_start();
                                self.fail_skip(start);
                            }
                        }
                    }
                }
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                true
            }
            _ => false,
        }
    }

    /// `a::b::c` and `Vec::<T>::new()`-style path expressions.
    fn parse_path_expr(&mut self) {
        self.events.push(Event::Start {
            kind: nk::PATH_EXPR,
            forward_parent: None,
        });
        if self.peek() == Some(tk::COLONCOLON) {
            self.bump();
        }
        loop {
            if matches!(
                self.peek(),
                Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
            ) {
                self.bump();
            } else {
                let start = self.next_start();
                self.fail_skip(start);
                break;
            }
            if self.peek() == Some(tk::COLONCOLON) && self.peek_at(1) == Some(tk::LT) {
                self.events.push(Event::Start {
                    kind: nk::GENERIC_ARGS,
                    forward_parent: None,
                });
                self.bump();
                self.bump();
                self.parse_generic_arg_list();
                self.events.push(Event::Finish);
            }
            if self.peek() == Some(tk::COLONCOLON)
                && matches!(
                    self.peek_at(1),
                    Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                )
            {
                self.bump();
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Patterns
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // the pattern grammar is the data
    fn parse_pattern(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::AMP => {
                self.events.push(Event::Start {
                    kind: nk::REF_PAT,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::KW_MUT) {
                    self.bump();
                }
                let ok = self.parse_pattern();
                self.events.push(Event::Finish);
                ok
            }
            tk::KW_MUT | tk::KW_REF => {
                self.bump();
                self.parse_pattern()
            }
            tk::LPAREN => {
                self.events.push(Event::Start {
                    kind: nk::TUPLE_PAT,
                    forward_parent: None,
                });
                self.bump();
                while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                    if !self.parse_pattern() {
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
                self.events.push(Event::Finish);
                true
            }
            tk::INT_LIT
            | tk::FLOAT_LIT
            | tk::STRING_LIT
            | tk::CHAR_LIT
            | tk::KW_TRUE
            | tk::KW_FALSE => {
                self.bump();
                true
            }
            tk::MINUS if matches!(self.peek_at(1), Some(tk::INT_LIT | tk::FLOAT_LIT)) => {
                self.bump();
                self.bump();
                true
            }
            tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER => {
                if self.token_text_is("_") {
                    self.bump(); // wildcard rides as the `_` identifier
                    return true;
                }
                self.events.push(Event::Start {
                    kind: nk::PATH_PAT,
                    forward_parent: None,
                });
                self.parse_path_tokens();
                // Tuple-variant / struct-pattern payloads.
                match self.peek() {
                    Some(tk::LPAREN) => {
                        self.events.push(Event::Start {
                            kind: nk::TUPLE_PAT,
                            forward_parent: None,
                        });
                        self.bump();
                        while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                            if !self.parse_pattern() {
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
                        self.events.push(Event::Finish);
                    }
                    Some(tk::LBRACE) => {
                        self.events.push(Event::Start {
                            kind: nk::STRUCT_PAT,
                            forward_parent: None,
                        });
                        self.bump();
                        while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                            if matches!(self.peek(), Some(tk::IDENT | tk::INT_LIT)) {
                                self.bump();
                                if self.peek() == Some(tk::COLON) {
                                    self.bump();
                                    if !self.parse_pattern() {
                                        let start = self.next_start();
                                        self.fail_skip(start);
                                    }
                                }
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
                        if self.peek() == Some(tk::RBRACE) {
                            self.bump();
                        }
                        self.events.push(Event::Finish);
                    }
                    _ => {}
                }
                self.events.push(Event::Finish);
                true
            }
            _ => false,
        }
    }

    /// Path tokens without a wrapper (shared by pattern parsing).
    fn parse_path_tokens(&mut self) {
        if self.peek() == Some(tk::COLONCOLON) {
            self.bump();
        }
        while matches!(
            self.peek(),
            Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
        ) {
            self.bump();
            if self.peek() == Some(tk::COLONCOLON)
                && matches!(
                    self.peek_at(1),
                    Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                )
            {
                self.bump();
                continue;
            }
            break;
        }
    }

    /// Whether a macro invocation follows from here: a path, then
    /// `!`, then a group opener.
    fn path_then_macro(&self) -> bool {
        let mut i = self.pos;
        let at = |i: usize| self.feed.get(i).map(|t| t.kind);
        if at(i) == Some(tk::COLONCOLON) {
            i += 1;
        }
        loop {
            match at(i) {
                Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER) => i += 1,
                _ => return false,
            }
            if at(i) == Some(tk::COLONCOLON) {
                i += 1;
                continue;
            }
            break;
        }
        at(i) == Some(tk::BANG) && matches!(at(i + 1), Some(tk::LPAREN | tk::LBRACK | tk::LBRACE))
    }

    /// `S { x: 1, y: e }` record body (expr position; `STRUCT_PAT`
    /// node — the pattern shape, documented).
    fn parse_record_body(&mut self) {
        self.events.push(Event::Start {
            kind: nk::STRUCT_PAT,
            forward_parent: None,
        });
        self.bump(); // {
        while !matches!(self.peek(), Some(tk::RBRACE) | None) {
            if self.peek() == Some(tk::IDENT) {
                self.bump();
                if self.peek() == Some(tk::COLON) {
                    self.bump();
                    if Self::starts_expr(self.peek()) {
                        self.parse_expression(0);
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                }
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
        if self.peek() == Some(tk::RBRACE) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn token_text_is(&self, text: &str) -> bool {
        self.feed.get(self.pos).is_some_and(|t| {
            let range = &t.range;
            self.source.get(
                usize::try_from(range.start).unwrap_or(usize::MAX)
                    ..usize::try_from(range.end).unwrap_or(usize::MAX),
            ) == Some(text)
        })
    }

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // the type grammar is the data
    fn parse_type(&mut self) {
        match self.peek() {
            Some(tk::AMP) => {
                self.events.push(Event::Start {
                    kind: nk::REF_TYPE,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::LIFETIME) {
                    self.bump();
                }
                if self.peek() == Some(tk::KW_MUT) {
                    self.bump();
                }
                self.parse_type();
                self.events.push(Event::Finish);
            }
            Some(tk::LBRACK) => {
                self.events.push(Event::Start {
                    kind: nk::ARRAY_TYPE,
                    forward_parent: None,
                });
                self.bump();
                self.parse_type();
                if self.peek() == Some(tk::SEMI) {
                    self.bump();
                    if matches!(self.peek(), Some(tk::INT_LIT)) {
                        self.bump();
                    } else {
                        let start = self.next_start();
                        self.fail_skip(start);
                    }
                }
                if self.peek() == Some(tk::RBRACK) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
            }
            Some(tk::LPAREN) => {
                self.events.push(Event::Start {
                    kind: nk::TUPLE_TYPE,
                    forward_parent: None,
                });
                self.bump();
                while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                    self.parse_type();
                    if self.peek() == Some(tk::COMMA) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                }
                self.events.push(Event::Finish);
            }
            Some(tk::KW_FN) => {
                self.events.push(Event::Start {
                    kind: nk::FN_PTR_TYPE,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::LPAREN) {
                    self.bump();
                    while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                        self.parse_type();
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
                if self.peek() == Some(tk::ARROW) {
                    self.bump();
                    self.parse_type();
                }
                self.events.push(Event::Finish);
            }
            _ => {
                // Path type with optional generic args.
                if matches!(
                    self.peek(),
                    Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                ) {
                    if self.peek() == Some(tk::COLONCOLON) {
                        self.bump();
                    }
                    loop {
                        if matches!(
                            self.peek(),
                            Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                        ) {
                            self.bump();
                        } else {
                            let start = self.next_start();
                            self.fail_skip(start);
                            break;
                        }
                        if self.peek() == Some(tk::LT) {
                            self.events.push(Event::Start {
                                kind: nk::GENERIC_ARGS,
                                forward_parent: None,
                            });
                            self.bump();
                            self.parse_generic_arg_list();
                            self.events.push(Event::Finish);
                        }
                        if self.peek() == Some(tk::COLONCOLON)
                            && matches!(
                                self.peek_at(1),
                                Some(tk::IDENT | tk::KW_SELF | tk::KW_CRATE | tk::KW_SUPER)
                            )
                        {
                            self.bump();
                            continue;
                        }
                        break;
                    }
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
        }
    }
}

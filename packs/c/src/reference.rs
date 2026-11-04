//! Hand-written reference parser for the documented C11 subset
//! (issue 034): declarations with full declarators, function
//! definitions, struct/union/enum specifiers, statements, precedence
//! expressions with the `(T)`-cast lookahead, and preprocessor lines
//! as DIRECTIVE nodes. Includes the typedef shadowing heuristic.

use std::collections::HashSet;
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

/// Parse C source into the raw event stream:
/// `(events, token_ranges, error_count)`.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let scan = lexer::scan(source);
    let mut parser = Parser::run(source, scan);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse C source into a green tree (one implicit root).
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
    /// The typedef shadowing heuristic's name set (issue 034 TDD).
    typedefs: HashSet<String>,
}

/// Pratt binding-power table rows.
struct OpRow {
    token: u16,
    lbp: u8,
    rbp: u8,
}

/// The infix operator table: assignment right-assoc (`rbp < lbp`),
/// `?:` handled specially, then the C precedence ladder.
const INFIX: &[OpRow] = &[
    OpRow {
        token: tk::ASSIGN,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::PLUSEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::MINUSEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::STAREQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::SLASHEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::PERCENTEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::ANDEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::OREQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::XOREQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::SHLEQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::SHREQ,
        lbp: 2,
        rbp: 1,
    },
    OpRow {
        token: tk::OROR,
        lbp: 4,
        rbp: 5,
    },
    OpRow {
        token: tk::ANDAND,
        lbp: 5,
        rbp: 6,
    },
    OpRow {
        token: tk::PIPE,
        lbp: 6,
        rbp: 7,
    },
    OpRow {
        token: tk::CARET,
        lbp: 7,
        rbp: 8,
    },
    OpRow {
        token: tk::AMP,
        lbp: 8,
        rbp: 9,
    },
    OpRow {
        token: tk::EQ,
        lbp: 9,
        rbp: 10,
    },
    OpRow {
        token: tk::NE,
        lbp: 9,
        rbp: 10,
    },
    OpRow {
        token: tk::LT,
        lbp: 10,
        rbp: 11,
    },
    OpRow {
        token: tk::GT,
        lbp: 10,
        rbp: 11,
    },
    OpRow {
        token: tk::LE,
        lbp: 10,
        rbp: 11,
    },
    OpRow {
        token: tk::GE,
        lbp: 10,
        rbp: 11,
    },
    OpRow {
        token: tk::SHL,
        lbp: 11,
        rbp: 12,
    },
    OpRow {
        token: tk::SHR,
        lbp: 11,
        rbp: 12,
    },
    OpRow {
        token: tk::PLUS,
        lbp: 12,
        rbp: 13,
    },
    OpRow {
        token: tk::MINUS,
        lbp: 12,
        rbp: 13,
    },
    OpRow {
        token: tk::STAR,
        lbp: 13,
        rbp: 14,
    },
    OpRow {
        token: tk::SLASH,
        lbp: 13,
        rbp: 14,
    },
    OpRow {
        token: tk::PERCENT,
        lbp: 13,
        rbp: 14,
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
            typedefs: HashSet::new(),
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
    /// `,` closers and `{`. Always consumes ≥ 1 non-sync token
    /// (progress).
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
                    | tk::COMMA
                    | tk::RBRACE
                    | tk::RPAREN
                    | tk::RBRACK
                    | tk::LBRACE
                    | tk::PP_LINE
            ) {
                break;
            }
            self.pos += 1;
        }
        let at_sync = self.feed.get(self.pos).is_some_and(|t| {
            matches!(
                t.kind,
                tk::SEMI
                    | tk::COMMA
                    | tk::RBRACE
                    | tk::RPAREN
                    | tk::RBRACK
                    | tk::LBRACE
                    | tk::PP_LINE
            )
        });
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

    /// Recovery that force-consumes one stray token (progress when a
    /// sync token sits where a statement should start).
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

    fn token_text(&self, offset: usize) -> Option<&'s str> {
        self.feed.get(self.pos + offset).map(|t| {
            &self.source[usize::try_from(t.range.start).unwrap_or(0)
                ..usize::try_from(t.range.end).unwrap_or(0)]
        })
    }

    /// Whether the token kind is a type keyword / qualifier.
    fn is_type_kw(kind: u16) -> bool {
        matches!(
            kind,
            tk::KW_INT
                | tk::KW_CHAR
                | tk::KW_FLOAT
                | tk::KW_DOUBLE
                | tk::KW_VOID
                | tk::KW_LONG
                | tk::KW_SHORT
                | tk::KW_SIGNED
                | tk::KW_UNSIGNED
                | tk::KW_BOOL
                | tk::KW_CONST
                | tk::KW_VOLATILE
                | tk::KW_STRUCT
                | tk::KW_UNION
                | tk::KW_ENUM
                | tk::KW_STATIC
                | tk::KW_EXTERN
                | tk::KW_TYPEDEF
                | tk::KW_INLINE
        )
    }

    /// Whether an identifier at `offset` is a known typedef name.
    fn is_typedef_name(&self, offset: usize) -> bool {
        self.peek_at(offset) == Some(tk::IDENT)
            && self
                .token_text(offset)
                .is_some_and(|text| self.typedefs.contains(text))
    }

    // ------------------------------------------------------------------
    // Module / external declarations
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        loop {
            match self.peek() {
                None => break,
                Some(tk::SEMI) => {
                    self.bump();
                }
                Some(tk::PP_LINE) => self.parse_directive(),
                Some(tk::RBRACE) => {
                    // Stray closer at file scope: rides an error region;
                    // trivia after it stays (those bytes need cover).
                    self.errors += 1;
                    self.events.push(Event::Error { diag: DIAG_SYNTAX });
                    self.events.push(Event::Start {
                        kind: ERROR_KIND,
                        forward_parent: None,
                    });
                    let start = self.next_start();
                    let end = self.feed.get(self.pos).map_or(start, |t| t.range.end);
                    self.bump();
                    self.trivia
                        .retain(|(range, _, _)| range.end <= start || range.start >= end);
                    self.events.push(Event::Finish);
                }
                _ => self.parse_external(),
            }
        }
    }

    /// One preprocessor line as a structured DIRECTIVE node.
    fn parse_directive(&mut self) {
        self.events.push(Event::Start {
            kind: nk::DIRECTIVE,
            forward_parent: None,
        });
        self.bump();
        self.events.push(Event::Finish);
    }

    /// A top-level function definition or declaration.
    fn parse_external(&mut self) {
        let open = self.events.len();
        self.events.push(Event::Start {
            kind: nk::DECLARATION,
            forward_parent: None,
        });
        if !self.parse_decl_specifiers() {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() == Some(tk::SEMI) {
            self.bump(); // bare specifier decl (`struct S;`)
            self.events.push(Event::Finish);
            return;
        }
        self.parse_declarator();
        if self.peek() == Some(tk::LBRACE) {
            self.retag_function_def(open);
            self.parse_compound();
            self.events.push(Event::Finish);
        } else {
            self.retag_declaration();
            self.parse_initializer_opt();
            self.expect_semi();
            self.events.push(Event::Finish);
        }
    }

    /// Re-shape the open `DECLARATION` node (at `open`) into a
    /// `FUNCTION_DEF` once the `{` body proves it.
    fn retag_function_def(&mut self, open: usize) {
        if let Some(Event::Start { kind, .. }) = self.events.get_mut(open) {
            *kind = nk::FUNCTION_DEF;
        }
    }

    /// Declarations with a typedef keyword register the declared
    /// names for the shadowing heuristic.
    fn retag_declaration(&mut self) {
        let has_typedef = self.feed[..self.pos]
            .iter()
            .any(|t| t.kind == tk::KW_TYPEDEF);
        if !has_typedef {
            return;
        }
        // The declared name: the last plain identifier of the
        // declarator (documented heuristic).
        for (kind, range) in self.tokens.iter().rev() {
            if *kind == tk::IDENT {
                let text = &self.source[usize::try_from(range.start).unwrap_or(0)
                    ..usize::try_from(range.end).unwrap_or(0)];
                self.typedefs.insert(text.to_string());
                break;
            }
        }
    }

    /// Declaration specifiers: type keywords, qualifiers, storage,
    /// struct/union/enum specifiers (as nodes), known typedef names;
    /// an unknown leading IDENT is a user type (documented leniency).
    fn parse_decl_specifiers(&mut self) -> bool {
        let mut any = false;
        loop {
            match self.peek() {
                Some(k)
                    if Self::is_type_kw(k)
                        && k != tk::KW_STRUCT
                        && k != tk::KW_UNION
                        && k != tk::KW_ENUM =>
                {
                    self.bump();
                    any = true;
                }
                Some(tk::KW_STRUCT | tk::KW_UNION | tk::KW_ENUM) => {
                    self.parse_aggregate_spec();
                    any = true;
                }
                Some(tk::IDENT) if self.is_typedef_name(0) => {
                    self.bump();
                    any = true;
                }
                _ => break,
            }
        }
        if !any && self.peek() == Some(tk::IDENT) {
            // Unknown user type at a declaration head (`Foo bar;`).
            self.bump();
            any = true;
        }
        any
    }

    /// `struct S { … }` / `union U { … }` / `enum E { … }`.
    fn parse_aggregate_spec(&mut self) {
        let node = match self.peek() {
            Some(tk::KW_STRUCT) => nk::STRUCT_SPEC,
            Some(tk::KW_UNION) => nk::UNION_SPEC,
            _ => nk::ENUM_SPEC,
        };
        self.events.push(Event::Start {
            kind: node,
            forward_parent: None,
        });
        self.bump(); // struct / union / enum
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        }
        if self.peek() == Some(tk::LBRACE) {
            self.bump();
            if node == nk::ENUM_SPEC {
                while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                    self.parse_enumerator();
                }
            } else {
                while !matches!(self.peek(), Some(tk::RBRACE) | None) {
                    if self.peek() == Some(tk::SEMI) {
                        self.bump();
                        continue;
                    }
                    self.parse_field_decl();
                }
            }
            if self.peek() == Some(tk::RBRACE) {
                self.bump();
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_enumerator(&mut self) {
        self.events.push(Event::Start {
            kind: nk::ENUMERATOR,
            forward_parent: None,
        });
        if self.peek() == Some(tk::IDENT) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        if self.peek() == Some(tk::ASSIGN) {
            self.bump();
            self.parse_expression(0);
        }
        if self.peek() == Some(tk::COMMA) {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn parse_field_decl(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FIELD_DECL,
            forward_parent: None,
        });
        if self.parse_decl_specifiers() {
            if self.peek() != Some(tk::SEMI) {
                self.parse_declarator();
            }
            self.expect_semi();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Declarators (token soup with structural PARAM_LIST nodes)
    // ------------------------------------------------------------------

    /// `* const` pointers then the direct declarator.
    fn parse_declarator(&mut self) {
        while self.peek() == Some(tk::STAR) {
            self.bump();
            while matches!(self.peek(), Some(tk::KW_CONST | tk::KW_VOLATILE)) {
                self.bump();
            }
        }
        self.parse_direct_declarator();
    }

    /// `name`, `( declarator )`, `[size]` and `(params)` suffixes.
    fn parse_direct_declarator(&mut self) {
        match self.peek() {
            Some(tk::IDENT) => {
                self.bump();
            }
            Some(tk::LPAREN) => {
                // Could be `(declarator)` grouping or a parameter
                // list following a name — decided by what follows:
                // a type keyword / `)` / `void` means parameters.
                if self.param_paren_ahead() {
                    self.parse_param_list();
                    return;
                }
                self.bump();
                self.parse_declarator();
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
            }
            _ => {}
        }
        loop {
            match self.peek() {
                Some(tk::LBRACK) => {
                    // `[…]` size soup to the matching `]`.
                    self.bump();
                    let mut depth = 1_usize;
                    while let Some(kind) = self.peek() {
                        if kind == tk::LBRACK {
                            depth += 1;
                        }
                        if kind == tk::RBRACK {
                            depth -= 1;
                            self.bump();
                            if depth == 0 {
                                break;
                            }
                            continue;
                        }
                        if kind == tk::SEMI || kind == tk::LBRACE {
                            break; // unterminated: let the caller sync
                        }
                        self.bump();
                    }
                }
                Some(tk::LPAREN) if self.param_paren_ahead() => {
                    self.parse_param_list();
                }
                Some(tk::LPAREN) => {
                    // K&R-style identifier list — token soup.
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
                _ => break,
            }
        }
    }

    /// Whether the `(` at the cursor opens a parameter list: the next
    /// token is a type keyword, `void`, `)` or `…`-ish.
    fn param_paren_ahead(&self) -> bool {
        if self.peek() != Some(tk::LPAREN) {
            return false;
        }
        matches!(
            self.peek_at(1),
            Some(
                tk::RPAREN
                    | tk::KW_VOID
                    | tk::KW_INT
                    | tk::KW_CHAR
                    | tk::KW_FLOAT
                    | tk::KW_DOUBLE
                    | tk::KW_LONG
                    | tk::KW_SHORT
                    | tk::KW_SIGNED
                    | tk::KW_UNSIGNED
                    | tk::KW_BOOL
                    | tk::KW_CONST
                    | tk::KW_STRUCT
                    | tk::KW_UNION
                    | tk::KW_ENUM
            )
        )
    }

    fn parse_param_list(&mut self) {
        self.events.push(Event::Start {
            kind: nk::PARAM_LIST,
            forward_parent: None,
        });
        self.bump(); // (
        while !matches!(self.peek(), Some(tk::RPAREN) | None) {
            self.parse_param_decl();
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
        self.events.push(Event::Finish);
    }

    fn parse_param_decl(&mut self) {
        self.events.push(Event::Start {
            kind: nk::PARAM_DECL,
            forward_parent: None,
        });
        if !self.parse_decl_specifiers() {
            let start = self.next_start();
            self.fail_skip(start);
        } else if self.peek() != Some(tk::COMMA) && self.peek() != Some(tk::RPAREN) {
            self.parse_declarator();
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn parse_compound(&mut self) {
        self.events.push(Event::Start {
            kind: nk::COMPOUND_STMT,
            forward_parent: None,
        });
        self.bump(); // {
        loop {
            match self.peek() {
                Some(tk::RBRACE) | None => break,
                Some(tk::SEMI) => {
                    self.bump();
                }
                Some(tk::PP_LINE) => self.parse_directive(),
                _ => self.parse_statement(),
            }
        }
        if self.peek() == Some(tk::RBRACE) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_statement(&mut self) {
        match self.peek() {
            Some(tk::LBRACE) => self.parse_compound(),
            Some(tk::SEMI) => {
                self.bump(); // null statement (documented)
            }
            Some(tk::KW_IF) => self.parse_if(),
            Some(tk::KW_WHILE) => self.parse_while(),
            Some(tk::KW_DO) => self.parse_do(),
            Some(tk::KW_FOR) => self.parse_for(),
            Some(tk::KW_RETURN) => self.parse_return(),
            Some(tk::KW_BREAK) => {
                self.events.push(Event::Start {
                    kind: nk::BREAK_STMT,
                    forward_parent: None,
                });
                self.bump();
                self.expect_semi();
                self.events.push(Event::Finish);
            }
            Some(tk::KW_CONTINUE) => {
                self.events.push(Event::Start {
                    kind: nk::CONTINUE_STMT,
                    forward_parent: None,
                });
                self.bump();
                self.expect_semi();
                self.events.push(Event::Finish);
            }
            Some(tk::PP_LINE) => self.parse_directive(),
            _ if self.starts_declaration() => self.parse_local_declaration(),
            _ => self.parse_expr_statement(),
        }
    }

    /// The typedef shadowing heuristic (issue 034 TDD): a statement
    /// starting with a type keyword is a declaration; `T x`, `T *x`,
    /// `T (f)(…)`, `T x[]` with a known typedef `T` are declarations.
    fn starts_declaration(&self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        if Self::is_type_kw(kind) {
            return true;
        }
        if kind != tk::IDENT {
            return false;
        }
        let known = self.is_typedef_name(0);
        known
            && matches!(
                self.peek_at(1),
                Some(tk::IDENT | tk::STAR | tk::LPAREN | tk::KW_CONST | tk::KW_VOLATILE)
            )
    }

    fn parse_local_declaration(&mut self) {
        self.events.push(Event::Start {
            kind: nk::DECLARATION,
            forward_parent: None,
        });
        if !self.parse_decl_specifiers() {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() != Some(tk::SEMI) {
            self.parse_declarator();
        }
        self.retag_declaration();
        self.parse_initializer_opt();
        self.expect_semi();
        self.events.push(Event::Finish);
    }

    /// `= expr` / `= { init-list }`.
    fn parse_initializer_opt(&mut self) {
        if self.peek() != Some(tk::ASSIGN) {
            return;
        }
        self.bump();
        if self.peek() == Some(tk::LBRACE) {
            self.parse_init_list();
        } else if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
    }

    fn parse_init_list(&mut self) {
        self.events.push(Event::Start {
            kind: nk::INIT_LIST,
            forward_parent: None,
        });
        self.bump(); // {
        while !matches!(self.peek(), Some(tk::RBRACE) | None) {
            if self.peek() == Some(tk::LBRACE) {
                self.parse_init_list();
            } else if Self::starts_expr(self.peek()) {
                self.parse_expression(0);
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
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
        self.events.push(Event::Finish);
    }

    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: nk::IF_STMT,
            forward_parent: None,
        });
        self.bump();
        self.parse_paren_cond();
        self.parse_statement();
        if self.peek() == Some(tk::KW_ELSE) {
            self.bump();
            self.parse_statement();
        }
        self.events.push(Event::Finish);
    }

    fn parse_while(&mut self) {
        self.events.push(Event::Start {
            kind: nk::WHILE_STMT,
            forward_parent: None,
        });
        self.bump();
        self.parse_paren_cond();
        self.parse_statement();
        self.events.push(Event::Finish);
    }

    fn parse_do(&mut self) {
        self.events.push(Event::Start {
            kind: nk::DO_STMT,
            forward_parent: None,
        });
        self.bump();
        self.parse_statement();
        if self.peek() == Some(tk::KW_WHILE) {
            self.bump();
            self.parse_paren_cond();
        }
        self.expect_semi();
        self.events.push(Event::Finish);
    }

    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: nk::FOR_STMT,
            forward_parent: None,
        });
        self.bump();
        if self.peek() != Some(tk::LPAREN) {
            let start = self.next_start();
            self.fail_skip(start);
            self.events.push(Event::Finish);
            return;
        }
        self.bump();
        // Head: declaration or expression or empty, then `;`, cond,
        // `;`, step.
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else if self.starts_declaration() {
            self.parse_local_declaration();
        } else {
            self.parse_expression(0);
            self.expect_semi();
        }
        if self.peek() != Some(tk::SEMI) && Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        }
        self.expect_semi();
        if self.peek() != Some(tk::RPAREN) && Self::starts_expr(self.peek()) {
            self.parse_expression(0);
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
        self.bump();
        if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        }
        self.expect_semi();
        self.events.push(Event::Finish);
    }

    /// `( condition )`.
    fn parse_paren_cond(&mut self) {
        if self.peek() == Some(tk::LPAREN) {
            self.bump();
            if self.peek() != Some(tk::RPAREN) {
                self.parse_expression(0);
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
    }

    fn parse_expr_statement(&mut self) {
        self.events.push(Event::Start {
            kind: nk::EXPR_STMT,
            forward_parent: None,
        });
        if Self::starts_expr(self.peek()) {
            self.parse_expression(0);
        } else {
            // Force-consume: a sync token must not stall the loop.
            let start = self.next_start();
            self.fail_skip_stray(start);
        }
        self.expect_semi();
        self.events.push(Event::Finish);
    }

    fn expect_semi(&mut self) {
        if self.peek() == Some(tk::SEMI) {
            self.bump();
        } else {
            let start = self.next_start();
            self.fail_skip(start);
        }
    }

    fn starts_expr(kind: Option<u16>) -> bool {
        matches!(
            kind,
            Some(
                tk::IDENT
                    | tk::INT_LIT
                    | tk::FLOAT_LIT
                    | tk::CHAR_LIT
                    | tk::STRING_LIT
                    | tk::LPAREN
                    | tk::PLUS
                    | tk::MINUS
                    | tk::STAR
                    | tk::AMP
                    | tk::BANG
                    | tk::TILDE
                    | tk::INCR
                    | tk::DECR
                    | tk::KW_SIZEOF
            )
        )
    }

    // ------------------------------------------------------------------
    // Expressions (Pratt)
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // the expression grammar is the data
    fn parse_expression(&mut self, min_bp: u8) -> bool {
        if !self.parse_prefix() {
            return false;
        }
        while let Some(kind) = self.peek() {
            if kind == tk::LPAREN {
                self.events.push(Event::Start {
                    kind: nk::CALL_EXPR,
                    forward_parent: None,
                });
                self.bump();
                while !matches!(self.peek(), Some(tk::RPAREN) | None) {
                    if Self::starts_expr(self.peek()) {
                        self.parse_expression(0);
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
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
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
                continue;
            }
            if matches!(kind, tk::DOT | tk::ARROW) {
                self.events.push(Event::Start {
                    kind: nk::FIELD_ACCESS,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::IDENT) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                self.events.push(Event::Finish);
                continue;
            }
            if matches!(kind, tk::INCR | tk::DECR) {
                self.events.push(Event::Start {
                    kind: nk::UNARY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                self.events.push(Event::Finish);
                continue;
            }
            if kind == tk::QUESTION {
                // `c ? t : e` — the lowest branch (right-assoc).
                self.events.push(Event::Start {
                    kind: nk::COND_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(0);
                }
                if self.peek() == Some(tk::COLON) {
                    self.bump();
                } else {
                    let start = self.next_start();
                    self.fail_skip(start);
                }
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(1);
                }
                self.events.push(Event::Finish);
                continue;
            }
            if let Some(row) = INFIX.iter().find(|row| row.token == kind) {
                if row.lbp < min_bp {
                    break;
                }
                self.events.push(Event::Start {
                    kind: nk::BIN_EXPR,
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

    fn parse_prefix(&mut self) -> bool {
        let Some(kind) = self.peek() else {
            return false;
        };
        match kind {
            tk::PLUS
            | tk::MINUS
            | tk::BANG
            | tk::TILDE
            | tk::STAR
            | tk::AMP
            | tk::INCR
            | tk::DECR => {
                self.events.push(Event::Start {
                    kind: nk::UNARY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if !self.parse_expression(14) {
                    self.events.pop();
                    return false;
                }
                self.events.push(Event::Finish);
                true
            }
            tk::KW_SIZEOF => {
                self.events.push(Event::Start {
                    kind: nk::UNARY_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() == Some(tk::LPAREN) && self.type_paren_ahead() {
                    self.bump();
                    self.parse_type_name();
                    if self.peek() == Some(tk::RPAREN) {
                        self.bump();
                    }
                } else if Self::starts_expr(self.peek()) {
                    self.parse_expression(14);
                }
                self.events.push(Event::Finish);
                true
            }
            tk::LPAREN if self.cast_ahead() => {
                self.events.push(Event::Start {
                    kind: nk::CAST_EXPR,
                    forward_parent: None,
                });
                self.bump(); // (
                self.parse_type_name();
                if self.peek() == Some(tk::RPAREN) {
                    self.bump();
                }
                if Self::starts_expr(self.peek()) {
                    self.parse_expression(14);
                }
                self.events.push(Event::Finish);
                true
            }
            tk::LPAREN => {
                self.events.push(Event::Start {
                    kind: nk::PAREN_EXPR,
                    forward_parent: None,
                });
                self.bump();
                if self.peek() != Some(tk::RPAREN) {
                    self.parse_expression(0);
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

    fn parse_atom(&mut self) -> bool {
        match self.peek() {
            Some(tk::IDENT | tk::INT_LIT | tk::FLOAT_LIT | tk::CHAR_LIT | tk::STRING_LIT) => {
                self.bump();
                true
            }
            _ => false,
        }
    }

    /// Whether the `(` at the cursor opens a type name: the matching
    /// `)` exists and the contents are only type tokens.
    fn type_paren_ahead(&self) -> bool {
        self.paren_contents_are_type().is_some()
    }

    /// The `(T)`-cast lookahead: contents are pure type tokens and
    /// an expression follows the closing paren.
    fn cast_ahead(&self) -> bool {
        let Some(end) = self.paren_contents_are_type() else {
            return false;
        };
        matches!(
            self.feed.get(end).map(|t| t.kind),
            Some(
                tk::IDENT
                    | tk::INT_LIT
                    | tk::FLOAT_LIT
                    | tk::CHAR_LIT
                    | tk::STRING_LIT
                    | tk::LPAREN
                    | tk::PLUS
                    | tk::MINUS
                    | tk::STAR
                    | tk::AMP
                    | tk::BANG
                    | tk::TILDE
                    | tk::INCR
                    | tk::DECR
                    | tk::KW_SIZEOF
            )
        )
    }

    /// If the cursor `(` … `)` spans a type spelling, return the
    /// feed index just past the `)`. A cast needs a type keyword or
    /// a known typedef inside — bare idents/stars stay paren
    /// expressions (function-pointer calls).
    fn paren_contents_are_type(&self) -> Option<usize> {
        if self.peek() != Some(tk::LPAREN) {
            return None;
        }
        let mut i = self.pos + 1;
        let mut depth = 1_usize;
        let mut saw_type = false;
        let mut first_ident_typedef = false;
        while i < self.feed.len() {
            let kind = self.feed[i].kind;
            match kind {
                tk::LPAREN => {
                    depth += 1;
                    i += 1;
                }
                tk::RPAREN => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return if saw_type || first_ident_typedef {
                            Some(i)
                        } else {
                            None
                        };
                    }
                }
                _ if Self::is_type_kw(kind) => {
                    saw_type = true;
                    i += 1;
                }
                tk::IDENT if !saw_type && !first_ident_typedef && i == self.pos + 1 => {
                    first_ident_typedef = self.feed[i..]
                        .first()
                        .map(|t| {
                            &self.source[usize::try_from(t.range.start).unwrap_or(0)
                                ..usize::try_from(t.range.end).unwrap_or(0)]
                        })
                        .is_some_and(|text| self.typedefs.contains(text));
                    saw_type |= first_ident_typedef;
                    i += 1;
                }
                tk::IDENT | tk::STAR | tk::KW_CONST | tk::KW_VOLATILE => {
                    i += 1;
                }
                _ => return None,
            }
        }
        None
    }

    /// A `TYPE_NAME` node wrapping the type token soup.
    fn parse_type_name(&mut self) {
        self.events.push(Event::Start {
            kind: nk::TYPE_NAME,
            forward_parent: None,
        });
        while let Some(kind) = self.peek() {
            if Self::is_type_kw(kind) || kind == tk::STAR || kind == tk::IDENT {
                self.bump();
            } else {
                break;
            }
        }
        self.events.push(Event::Finish);
    }
}

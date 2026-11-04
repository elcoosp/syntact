//! Hand-written reference SQL (SQLite-dialect subset) parser on the
//! runtime-core event protocol (issue 035). Case-insensitive keywords
//! with raw text preserved, precedence-climbing expressions, recursive
//! subqueries, and `--`/`/* */` trivia.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::ERROR_KIND;
use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

use crate::kinds;
use crate::token_kinds as tk;

/// Diagnostic side-table index meaning "syntax error".
pub const DIAG_SYNTAX: u32 = 0;

/// Maximum expression nesting depth the recursive-descent parser will
/// follow (the `parse_expr_bp → parse_atom → paren → parse_expr_bp`
/// cycle, plus unary chains). Deeper nesting fails cleanly: the opener
/// rides an ERROR region (lossless), the parse resynchronizes and the
/// process stays alive. Nesting bombs (`SELECT ((((…`) are a classic
/// stack-smash `DoS` and MUST NOT reach the stack.
pub const MAX_DEPTH: usize = 512;

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse SQL-subset source into the raw event stream.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_program();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse SQL-subset source into a green tree (one implicit
/// [`crate::ROOT_KIND`] root).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let mut parser = Parser::run(source);
    parser.parse_program();
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

struct Parser<'s> {
    bytes: &'s [u8],
    pos: u32,
    last_end: u32,
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    trivia: Vec<(Range<u32>, u16, bool)>,
    errors: usize,
    /// Current expression nesting depth, bounded by [`MAX_DEPTH`].
    depth: usize,
}

impl<'s> Parser<'s> {
    fn run(source: &'s str) -> Self {
        Self {
            bytes: source.as_bytes(),
            pos: 0,
            last_end: 0,
            events: Vec::new(),
            tokens: Vec::new(),
            trivia: Vec::new(),
            errors: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos as usize).copied()
    }

    fn starts_with(&self, text: &[u8]) -> bool {
        let start = self.pos as usize;
        self.bytes.len() >= start + text.len() && &self.bytes[start..start + text.len()] == text
    }

    fn at(&self, offset: u32) -> Option<u8> {
        self.bytes.get(self.pos as usize + offset as usize).copied()
    }

    /// Case-folded keyword test at the cursor (word-bounded).
    fn peek_kw(&self, kw: &str) -> bool {
        let n = kw.len();
        if self.bytes.len() < self.pos as usize + n {
            return false;
        }
        for (i, k) in kw.bytes().enumerate() {
            let b = match self.bytes.get(self.pos as usize + i) {
                Some(b) => *b,
                None => return false,
            };
            if !b.eq_ignore_ascii_case(&k) {
                return false;
            }
        }
        // Word boundary.
        match self.at(u32::try_from(n).unwrap_or(u32::MAX)) {
            None => true,
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'_'),
        }
    }

    fn bump_kw(&mut self, kw: &str) {
        self.bump(tk::WORD, u32::try_from(kw.len()).unwrap_or(u32::MAX));
    }

    fn skip_trivia(&mut self) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') => self.pos += 1,
                Some(b'-') if self.at(1) == Some(b'-') => {
                    if self.pos > start && kind != tk::COMMENT {
                        self.push_trivia(start, tk::WHITESPACE, false);
                        start = self.pos;
                    }
                    kind = tk::COMMENT;
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.at(1) == Some(b'*') => {
                    if self.pos > start && kind != tk::BLOCK_COMMENT {
                        self.push_trivia(start, kind, false);
                        start = self.pos;
                    }
                    kind = tk::BLOCK_COMMENT;
                    self.pos += 2;
                    while self.pos + 1 < u32::try_from(self.bytes.len()).unwrap_or(u32::MAX) {
                        if self.peek() == Some(b'*') && self.at(1) == Some(b'/') {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
        if self.pos > start {
            let trailing = start == self.last_end && self.last_end > 0;
            self.push_trivia(start, kind, trailing);
        }
    }

    fn push_trivia(&mut self, start: u32, kind: u16, trailing: bool) {
        self.trivia.push((start..self.pos, kind, trailing));
        self.last_end = self.last_end.max(self.pos);
    }

    fn bump(&mut self, kind: u16, n: u32) {
        let start = self.pos;
        self.pos += n;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, start..self.pos));
        self.last_end = self.pos;
    }

    fn wrap_node(&mut self, mark: usize, kind: u16) {
        self.events.insert(
            mark,
            Event::Start {
                kind,
                forward_parent: None,
            },
        );
        self.events.push(Event::Finish);
    }

    fn fail_skip(&mut self, start: u32, consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if matches!(b, b';' | b')') {
                break;
            }
            self.pos += 1;
        }
        if consume_stray && self.pos == start && self.peek().is_some() {
            self.pos += 1; // the stray char rides the error region —
            // progress is unconditional: exempting the sync byte here
            // let a caller retry fail_skip at the same cursor forever,
            // growing the event stream without bound (OOM kill).
        }
        if self.pos > start {
            self.events.push(Event::Token {
                kind: ERROR_KIND,
                n_raw: 1,
            });
            self.tokens.push((ERROR_KIND, start..self.pos));
            self.last_end = self.pos;
        }
        self.events.push(Event::Finish);
    }

    fn force_byte(&mut self) {
        let start = self.pos;
        if self.peek().is_none() {
            return;
        }
        self.pos += 1;
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        self.events.push(Event::Token {
            kind: ERROR_KIND,
            n_raw: 1,
        });
        self.tokens.push((ERROR_KIND, start..self.pos));
        self.last_end = self.pos;
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Scanner pieces
    // ------------------------------------------------------------------

    fn scan_word_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if !self
            .bytes
            .get(idx)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return 0;
        }
        idx += 1;
        while self
            .bytes
            .get(idx)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
        {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    fn scan_number_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx < self.bytes.len() && self.bytes[idx] == b'.' {
            idx += 1;
            while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
                idx += 1;
            }
        }
        if idx < self.bytes.len() && matches!(self.bytes[idx], b'e' | b'E') {
            let mut exp_end = idx + 1;
            if matches!(self.bytes.get(exp_end), Some(b'+' | b'-')) {
                exp_end += 1;
            }
            if exp_end < self.bytes.len() && self.bytes[exp_end].is_ascii_digit() {
                idx = exp_end;
                while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
                    idx += 1;
                }
            }
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// `'…'` with `''` escapes; `None` = unterminated.
    fn scan_string_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        loop {
            match self.bytes.get(idx)? {
                b'\'' if self.bytes.get(idx + 1) == Some(&b'\'') => idx += 2,
                b'\'' => return u32::try_from(idx + 1 - start).ok(),
                b'\n' => return None,
                _ => idx += 1,
            }
        }
    }

    fn bump_string(&mut self) {
        if let Some(len) = self.scan_string_len() {
            self.bump(tk::STRING, len);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    // ------------------------------------------------------------------
    // Program / statements
    // ------------------------------------------------------------------

    fn parse_program(&mut self) {
        while self.peek().is_some() {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
                continue;
            }
            let before = self.pos;
            self.parse_statement();
            if self.pos == before && self.peek().is_some() {
                self.force_byte();
            }
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            }
        }
    }

    /// One statement: optional CTE prefix, then the verb.
    fn parse_statement(&mut self) {
        self.skip_trivia();
        let mark = self.events.len();
        self.events.push(Event::Start {
            kind: kinds::STATEMENT,
            forward_parent: None,
        });
        self.skip_trivia();
        // CTE prefix: WITH name AS (…), …
        if self.peek_kw("with") {
            self.parse_ctes();
        }
        self.skip_trivia();
        if self.peek_kw("select") {
            self.parse_select();
        } else if self.peek_kw("insert") {
            self.parse_insert();
        } else if self.peek_kw("update") {
            self.parse_update();
        } else if self.peek_kw("delete") {
            self.parse_delete();
        } else if self.peek_kw("create") {
            self.parse_create();
        } else if self.peek_kw("alter") {
            self.parse_alter();
        } else if self.peek_kw("drop") {
            self.parse_drop();
        } else if self.peek_kw("pragma") {
            self.parse_pragma();
        } else if self.peek_kw("begin") || self.peek_kw("commit") || self.peek_kw("rollback") {
            self.parse_txn();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
        let _ = mark;
    }

    /// `WITH name AS (select), name2 AS (select)`.
    fn parse_ctes(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CTE,
            forward_parent: None,
        });
        self.bump_kw("with");
        loop {
            self.skip_trivia();
            self.bump_word();
            self.skip_trivia();
            if self.peek_kw("as") {
                self.bump_kw("as");
            }
            self.skip_trivia();
            if self.peek() == Some(b'(') {
                self.bump(tk::LPAREN, 1);
                self.skip_trivia();
                if self.peek_kw("select") {
                    self.parse_select();
                }
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    /// `SELECT [DISTINCT] items [FROM sources] [WHERE …] [GROUP BY …]
    /// [HAVING …] (UNION [ALL] | EXCEPT | INTERSECT core)* [ORDER BY …]
    /// [LIMIT …] [OFFSET …]` — each core is its own [`kinds::SELECT`]
    /// node; the compound operators sit between them as
    /// [`kinds::COMPOUND`] siblings.
    fn parse_select(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::SELECT,
            forward_parent: None,
        });
        self.bump_kw("select");
        self.skip_trivia();
        if self.peek_kw("distinct") {
            self.bump_kw("distinct");
            self.skip_trivia();
        }
        self.parse_core_tail();
        self.events.push(Event::Finish);
        // Compound operators chain full cores.
        loop {
            self.skip_trivia();
            let op = if self.peek_kw("union") {
                "union"
            } else if self.peek_kw("except") {
                "except"
            } else if self.peek_kw("intersect") {
                "intersect"
            } else {
                break;
            };
            self.events.push(Event::Start {
                kind: kinds::COMPOUND,
                forward_parent: None,
            });
            self.bump_kw(op);
            self.skip_trivia();
            if self.peek_kw("all") {
                self.bump_kw("all");
                self.skip_trivia();
            }
            self.events.push(Event::Finish);
            self.skip_trivia();
            if self.peek_kw("select") {
                self.events.push(Event::Start {
                    kind: kinds::SELECT,
                    forward_parent: None,
                });
                self.bump_kw("select");
                self.skip_trivia();
                if self.peek_kw("distinct") {
                    self.bump_kw("distinct");
                    self.skip_trivia();
                }
                self.parse_core_tail();
                self.events.push(Event::Finish);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        for clause in ["order", "limit", "offset"] {
            self.skip_trivia();
            if self.peek_kw(clause) {
                self.parse_clause(clause);
            }
        }
    }

    /// The per-core tail of a select: items, `FROM` + joins, and the
    /// `WHERE`/`GROUP BY`/`HAVING` clauses.
    #[allow(clippy::too_many_lines)]
    fn parse_core_tail(&mut self) {
        // Select items.
        loop {
            self.skip_trivia();
            self.parse_select_item();
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                continue;
            }
            break;
        }
        // FROM + joins.
        if self.peek_kw("from") {
            self.bump_kw("from");
            self.skip_trivia();
            self.parse_source();
            // Comma-separated sources: `FROM a, b`.
            loop {
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    self.skip_trivia();
                    self.parse_source();
                    continue;
                }
                break;
            }
            loop {
                self.skip_trivia();
                if self.peek_kw("join")
                    || self.peek_kw("inner")
                    || self.peek_kw("left")
                    || self.peek_kw("cross")
                {
                    if self.peek_kw("inner") || self.peek_kw("left") || self.peek_kw("cross") {
                        self.bump_kw(if self.peek_kw("inner") {
                            "inner"
                        } else if self.peek_kw("left") {
                            "left"
                        } else {
                            "cross"
                        });
                        self.skip_trivia();
                        if self.peek_kw("outer") {
                            self.bump_kw("outer");
                            self.skip_trivia();
                        }
                        if self.peek_kw("join") {
                            self.bump_kw("join");
                        }
                    } else {
                        self.bump_kw("join");
                    }
                    self.skip_trivia();
                    self.parse_source();
                    self.skip_trivia();
                    if self.peek_kw("on") {
                        self.parse_clause("on");
                    }
                    continue;
                }
                break;
            }
        }
        for clause in ["where", "group", "having"] {
            self.skip_trivia();
            if self.peek_kw(clause) {
                self.parse_clause(clause);
            }
        }
    }

    /// One named clause: the keyword + its expression list.
    fn parse_clause(&mut self, keyword: &str) {
        self.events.push(Event::Start {
            kind: kinds::CLAUSE,
            forward_parent: None,
        });
        self.bump_kw(keyword);
        loop {
            self.skip_trivia();
            // `GROUP`/`ORDER` continue into `BY`.
            if self.peek_kw("by") {
                self.bump_kw("by");
                self.skip_trivia();
            }
            if matches!(self.peek(), None | Some(b';')) || self.next_clause_ahead() {
                break;
            }
            self.parse_expr_bp(0);
            self.skip_trivia();
            // ORDER BY direction suffixes come after each expression.
            if self.peek_kw("asc") || self.peek_kw("desc") {
                self.bump_kw(if self.peek_kw("asc") { "asc" } else { "desc" });
                self.skip_trivia();
            }
            // `NULLS FIRST`/`NULLS LAST` ordering suffix.
            if self.peek_kw("nulls") {
                self.bump_kw("nulls");
                self.skip_trivia();
                if self.peek_kw("first") || self.peek_kw("last") {
                    self.bump_kw(if self.peek_kw("first") {
                        "first"
                    } else {
                        "last"
                    });
                    self.skip_trivia();
                }
            }
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    /// Whether a following clause keyword starts here (end of the
    /// current clause's expression list).
    fn next_clause_ahead(&self) -> bool {
        self.peek_kw("from")
            || self.peek_kw("where")
            || self.peek_kw("group")
            || self.peek_kw("having")
            || self.peek_kw("order")
            || self.peek_kw("limit")
            || self.peek_kw("offset")
            || self.peek_kw("inner")
            || self.peek_kw("left")
            || self.peek_kw("cross")
            || self.peek_kw("join")
            || self.peek_kw("union")
            || self.peek_kw("except")
            || self.peek_kw("intersect")
            || self.peek_kw("on")
    }

    /// A FROM/JOIN source: `table [AS] alias`, `(subquery) [AS] alias`.
    fn parse_source(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::SOURCE,
            forward_parent: None,
        });
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.skip_trivia();
            if self.peek_kw("select") {
                self.events.push(Event::Start {
                    kind: kinds::SUBQUERY,
                    forward_parent: None,
                });
                self.parse_select();
                self.events.push(Event::Finish);
            }
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            self.bump_word();
            self.skip_trivia();
            if self.peek() == Some(b'.') {
                self.bump(tk::DOT, 1);
                self.skip_trivia();
                self.bump_word();
                self.skip_trivia();
            }
        }
        self.skip_trivia();
        if self.peek_kw("as") {
            self.bump_kw("as");
            self.skip_trivia();
            self.bump_word();
        } else if !self.next_clause_ahead() && !self.peek_kw("on") && self.scan_word_len() > 0 {
            self.bump_word(); // bare alias
        }
        self.events.push(Event::Finish);
    }

    /// One select item: `*`, `t.*`, `expr [AS] alias`.
    fn parse_select_item(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::SELECT_ITEM,
            forward_parent: None,
        });
        self.skip_trivia();
        // `SELECT FROM t` — no items at all: one tolerated error.
        if self.next_clause_ahead() {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        if self.peek() == Some(b'*') {
            self.events.push(Event::Start {
                kind: kinds::STAR,
                forward_parent: None,
            });
            self.bump(tk::STAR, 1);
            self.events.push(Event::Finish);
        } else {
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek() == Some(b'.') {
                // `t.*`
                self.bump(tk::DOT, 1);
                self.skip_trivia();
                if self.peek() == Some(b'*') {
                    self.bump(tk::STAR, 1);
                } else {
                    self.bump_word();
                }
            }
        }
        self.skip_trivia();
        if self.peek_kw("as") {
            self.events.push(Event::Start {
                kind: kinds::ALIAS,
                forward_parent: None,
            });
            self.bump_kw("as");
            self.skip_trivia();
            self.bump_word();
            self.events.push(Event::Finish);
        } else if self.scan_word_len() > 0 && !self.next_clause_ahead() {
            self.events.push(Event::Start {
                kind: kinds::ALIAS,
                forward_parent: None,
            });
            self.bump_word();
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    /// `INSERT INTO t [(cols)] VALUES (…), …`.
    fn parse_insert(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::INSERT,
            forward_parent: None,
        });
        self.bump_kw("insert");
        self.skip_trivia();
        if self.peek_kw("into") {
            self.bump_kw("into");
        }
        self.skip_trivia();
        self.bump_word();
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b')') || self.peek().is_none() {
                    break;
                }
                self.bump_word();
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia();
        }
        if self.peek_kw("values") {
            self.events.push(Event::Start {
                kind: kinds::CLAUSE,
                forward_parent: None,
            });
            self.bump_kw("values");
            self.skip_trivia();
            if self.peek() != Some(b'(') {
                // `VALUES;` — at least one row is required.
                let start = self.pos;
                self.fail_skip(start, true);
            }
            loop {
                self.skip_trivia();
                if self.peek() == Some(b'(') {
                    self.bump(tk::LPAREN, 1);
                    loop {
                        self.skip_trivia();
                        if self.peek() == Some(b')') || self.peek().is_none() {
                            break;
                        }
                        self.parse_expr_bp(0);
                        self.skip_trivia();
                        if self.peek() == Some(b',') {
                            self.bump(tk::COMMA, 1);
                            continue;
                        }
                        break;
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b')') {
                        self.bump(tk::RPAREN, 1);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                }
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    /// `UPDATE t SET c = e, … [WHERE …]`.
    fn parse_update(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::UPDATE,
            forward_parent: None,
        });
        self.bump_kw("update");
        self.skip_trivia();
        self.bump_word();
        self.skip_trivia();
        if self.peek_kw("set") {
            self.events.push(Event::Start {
                kind: kinds::CLAUSE,
                forward_parent: None,
            });
            self.bump_kw("set");
            loop {
                self.skip_trivia();
                if self.next_clause_ahead() || self.peek().is_none() {
                    break;
                }
                self.events.push(Event::Start {
                    kind: kinds::ALIAS,
                    forward_parent: None,
                });
                self.bump_word();
                self.skip_trivia();
                if self.peek() == Some(b'=') {
                    self.bump(tk::EQUALS, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
                }
                self.events.push(Event::Finish);
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.events.push(Event::Finish);
        }
        self.skip_trivia();
        if self.peek_kw("where") {
            self.parse_clause("where");
        }
        self.events.push(Event::Finish);
    }

    /// `DELETE FROM t [WHERE …]`.
    fn parse_delete(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::DELETE,
            forward_parent: None,
        });
        self.bump_kw("delete");
        self.skip_trivia();
        if self.peek_kw("from") {
            self.parse_clause("from");
        }
        self.skip_trivia();
        if self.peek_kw("where") {
            self.parse_clause("where");
        }
        self.events.push(Event::Finish);
    }

    /// `CREATE` dispatch: `TABLE`, `[UNIQUE] INDEX`, `VIEW`.
    fn parse_create(&mut self) {
        match self.peek_next_word() {
            "unique" | "index" => self.parse_create_index(),
            "view" => self.parse_create_view(),
            _ => self.parse_create_table(),
        }
    }

    /// The case-folded word after the one at the cursor (used to peek
    /// the second keyword of `CREATE TABLE|INDEX|VIEW|UNIQUE INDEX`);
    /// `"none"` when absent.
    fn peek_next_word(&self) -> &'static str {
        let start = self.pos as usize + self.scan_word_len() as usize;
        let mut idx = start;
        while idx < self.bytes.len() && self.bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let word_start = idx;
        while idx < self.bytes.len()
            && (self.bytes[idx].is_ascii_alphanumeric() || self.bytes[idx] == b'_')
        {
            idx += 1;
        }
        let text = &self.bytes[word_start..idx];
        match String::from_utf8_lossy(text).to_ascii_lowercase().as_str() {
            "unique" => "unique",
            "index" => "index",
            "view" => "view",
            "table" => "table",
            _ => "none",
        }
    }

    /// `CREATE TABLE t (col constraints, …)`.
    #[allow(clippy::too_many_lines)]
    fn parse_create_table(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CREATE_TABLE,
            forward_parent: None,
        });
        self.bump_kw("create");
        self.skip_trivia();
        if self.peek_kw("table") {
            self.bump_kw("table");
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        self.skip_trivia();
        self.bump_word();
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            loop {
                self.skip_trivia();
                match self.peek() {
                    Some(b')') | None => break,
                    _ => {}
                }
                self.events.push(Event::Start {
                    kind: kinds::COLUMN_DEF,
                    forward_parent: None,
                });
                self.bump_word();
                self.skip_trivia();
                // Column type words + constraints ride WORD tokens.
                loop {
                    self.skip_trivia();
                    if self.scan_word_len() == 0 {
                        break;
                    }
                    self.bump_word();
                    self.skip_trivia();
                    if self.peek() == Some(b'(') {
                        self.bump(tk::LPAREN, 1);
                        loop {
                            self.skip_trivia();
                            if self.peek() == Some(b')') || self.peek().is_none() {
                                break;
                            }
                            if self.scan_number_len() > 0 {
                                let n = self.scan_number_len();
                                self.bump(tk::NUMBER, n);
                            } else {
                                self.bump_word();
                            }
                            self.skip_trivia();
                            if self.peek() == Some(b',') {
                                self.bump(tk::COMMA, 1);
                                continue;
                            }
                            break;
                        }
                        self.skip_trivia();
                        if self.peek() == Some(b')') {
                            self.bump(tk::RPAREN, 1);
                        }
                        self.skip_trivia();
                    }
                    if self.peek() == Some(b',') || self.peek() == Some(b')') {
                        break;
                    }
                }
                self.events.push(Event::Finish);
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    fn bump_word(&mut self) {
        let n = self.scan_word_len();
        if n > 0 {
            self.bump(tk::WORD, n);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// `CREATE [UNIQUE] INDEX name ON t (col, …)` — column lists ride
    /// word/number tokens inside the parens (the CREATE TABLE shape).
    fn parse_create_index(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CREATE_INDEX,
            forward_parent: None,
        });
        self.bump_kw("create");
        self.skip_trivia();
        if self.peek_kw("unique") {
            self.bump_kw("unique");
            self.skip_trivia();
        }
        if self.peek_kw("index") {
            self.bump_kw("index");
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        self.skip_trivia();
        self.bump_word(); // index name
        self.skip_trivia();
        if self.peek_kw("on") {
            self.bump_kw("on");
            self.skip_trivia();
            self.bump_word(); // table name
        }
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b')') || self.peek().is_none() {
                    break;
                }
                if self.scan_word_len() > 0 {
                    self.bump_word();
                } else if self.scan_number_len() > 0 {
                    let n = self.scan_number_len();
                    self.bump(tk::NUMBER, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// `CREATE VIEW v [AS] SELECT …` — the select rides a full
    /// [`kinds::SELECT`] node.
    fn parse_create_view(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CREATE_VIEW,
            forward_parent: None,
        });
        self.bump_kw("create");
        self.skip_trivia();
        if self.peek_kw("view") {
            self.bump_kw("view");
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        self.skip_trivia();
        self.bump_word(); // view name
        self.skip_trivia();
        if self.peek_kw("as") {
            self.bump_kw("as");
            self.skip_trivia();
        }
        if self.peek_kw("select") {
            self.parse_select();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// `ALTER TABLE t action` — `RENAME TO new`, `ADD [COLUMN] def`,
    /// `DROP [COLUMN] name`; the action words ride WORD tokens.
    fn parse_alter(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ALTER,
            forward_parent: None,
        });
        self.bump_kw("alter");
        self.skip_trivia();
        if self.peek_kw("table") {
            self.bump_kw("table");
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        self.skip_trivia();
        self.bump_word(); // table name
        self.skip_trivia();
        // The rest of the action rides word/number tokens (`ADD COLUMN
        // c INTEGER DEFAULT 0`, `RENAME TO t2`, `DROP COLUMN c`).
        loop {
            self.skip_trivia();
            let words = self.scan_word_len();
            if words > 0 {
                self.bump(tk::WORD, words);
                continue;
            }
            let nums = self.scan_number_len();
            if nums > 0 {
                self.bump(tk::NUMBER, nums);
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    /// `DROP TABLE|INDEX|VIEW [IF EXISTS] name [, name]`.
    fn parse_drop(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::DROP,
            forward_parent: None,
        });
        self.bump_kw("drop");
        self.skip_trivia();
        if self.peek_kw("table") || self.peek_kw("index") || self.peek_kw("view") {
            self.bump_kw(if self.peek_kw("table") {
                "table"
            } else if self.peek_kw("index") {
                "index"
            } else {
                "view"
            });
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
        self.skip_trivia();
        if self.peek_kw("if") {
            self.bump_kw("if");
            self.skip_trivia();
            if self.peek_kw("exists") {
                self.bump_kw("exists");
            }
            self.skip_trivia();
        }
        self.bump_word(); // first name
        loop {
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                self.bump_word();
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    /// `PRAGMA name [= value | (value)]`.
    fn parse_pragma(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PRAGMA,
            forward_parent: None,
        });
        self.bump_kw("pragma");
        self.skip_trivia();
        self.bump_word(); // pragma name
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::EQUALS, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
        } else if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        self.events.push(Event::Finish);
    }

    /// `BEGIN [TRANSACTION|DEFERRED|IMMEDIATE|EXCLUSIVE]`,
    /// `COMMIT`/`END`, `ROLLBACK` — a trailing word rides along.
    fn parse_txn(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::TXN,
            forward_parent: None,
        });
        self.bump_kw(if self.peek_kw("begin") {
            "begin"
        } else if self.peek_kw("commit") {
            "commit"
        } else {
            "rollback"
        });
        self.skip_trivia();
        if self.scan_word_len() > 0 {
            self.bump_word();
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Expressions — precedence climbing
    // ------------------------------------------------------------------

    fn parse_expr_bp(&mut self, min_bp: u8) {
        if self.depth >= MAX_DEPTH {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        }
        self.depth += 1;
        self.parse_expr_bp_guarded(min_bp);
        self.depth -= 1;
    }

    /// The guarded body of [`Self::parse_expr_bp`] (depth accounting
    /// lives in the wrapper).
    fn parse_expr_bp_guarded(&mut self, min_bp: u8) {
        let mark = self.events.len();
        if let Some((kw, rbp)) = self.peek_unary_kw() {
            self.bump_kw(kw);
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            self.wrap_node(mark, kinds::UNARY);
        } else if let Some((kind, n, rbp)) = self.peek_unary() {
            self.bump(kind, n);
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            self.wrap_node(mark, kinds::UNARY);
        } else {
            self.parse_atom();
        }
        self.skip_trivia();
        self.parse_binops(mark, min_bp);
        self.skip_trivia();
    }

    /// Keyword unary operators.
    fn peek_unary_kw(&self) -> Option<(&'static str, u8)> {
        if self.peek_kw("not") {
            Some(("not", 5))
        } else if self.peek_kw("exists") {
            // `EXISTS (subquery)` — binds tightest of the word unaries.
            Some(("exists", 8))
        } else {
            None
        }
    }

    /// Symbol unary operators.
    fn peek_unary(&self) -> Option<(u16, u32, u8)> {
        match self.peek() {
            Some(b'-') if !self.starts_with(b"--") => Some((tk::MINUS, 1, 7)),
            Some(b'+') if !self.starts_with(b"+") || self.scan_word_len() == 0 => {
                if self.peek() == Some(b'+') {
                    Some((tk::PLUS, 1, 7))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// The binary table: OR(1) AND(2) comparisons(3) IN/LIKE/BETWEEN(3)
    /// +(4) -(4) *(5) /(5) %(5) ||(5).
    fn parse_binops(&mut self, mark: usize, min_bp: u8) {
        while let Some((lbp, rbp, kind)) = self.peek_binop() {
            if lbp < min_bp {
                break;
            }
            match kind {
                BinOpKind::Word(kw) => self.bump_kw(kw),
                BinOpKind::Punct(kind16) => self.bump(kind16, self.punct_len()),
            }
            self.skip_trivia();
            // `IN (…)` / `BETWEEN a AND b` wrappers.
            if let BinOpKind::Word(kw) = kind {
                if kw.eq_ignore_ascii_case("in") && self.peek() == Some(b'(') {
                    self.events.push(Event::Start {
                        kind: kinds::IN_LIST,
                        forward_parent: None,
                    });
                    self.bump(tk::LPAREN, 1);
                    loop {
                        self.skip_trivia();
                        if self.peek_kw("select") {
                            self.events.push(Event::Start {
                                kind: kinds::SUBQUERY,
                                forward_parent: None,
                            });
                            self.parse_select();
                            self.events.push(Event::Finish);
                        } else if self.peek() == Some(b')') || self.peek().is_none() {
                            break;
                        } else {
                            self.parse_expr_bp(0);
                        }
                        self.skip_trivia();
                        if self.peek() == Some(b',') {
                            self.bump(tk::COMMA, 1);
                            continue;
                        }
                        break;
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b')') {
                        self.bump(tk::RPAREN, 1);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.events.push(Event::Finish);
                    self.wrap_node(mark, kinds::BINOP);
                    continue;
                }
                let _ = kw;
            }
            self.parse_expr_bp(rbp);
            self.wrap_node(mark, kinds::BINOP);
        }
    }

    fn peek_binop(&self) -> Option<(u8, u8, BinOpKind)> {
        // Keyword comparisons first.
        for kw in ["between", "in", "like", "is", "or", "and"] {
            if self.peek_kw(kw) {
                let bp: u8 = match kw {
                    "or" => 1,
                    "and" => 2,
                    _ => 3,
                };
                return Some((bp, bp + 1, BinOpKind::Word(kw)));
            }
        }
        let (kind, _) = self.peek_punct()?;
        let (lbp, rbp) = match kind {
            tk::EQUALS | tk::NEQ | tk::NEQ2 | tk::LT | tk::LE | tk::GT | tk::GE => (3_u8, 4_u8),
            tk::PLUS | tk::MINUS => (4, 5),
            tk::STAR | tk::SLASH | tk::PERCENT | tk::CONCAT => (5, 6),
            _ => return None,
        };
        Some((lbp, rbp, BinOpKind::Punct(kind)))
    }

    fn punct_len(&self) -> u32 {
        const TWO: &[&[u8]] = &[b"<=", b">=", b"!=", b"<>", b"||"];
        for text in TWO {
            if self.starts_with(text) {
                return u32::try_from(text.len()).unwrap_or(u32::MAX);
            }
        }
        1
    }

    fn peek_punct(&self) -> Option<(u16, u32)> {
        self.peek()?;
        let len = self.punct_len();
        let start = usize::try_from(self.pos).unwrap_or(0);
        let bytes = self.bytes.get(start..start + len as usize)?;
        let kind = match bytes {
            b"<=" => tk::LE,
            b">=" => tk::GE,
            b"!=" => tk::NEQ,
            b"<>" => tk::NEQ2,
            b"||" => tk::CONCAT,
            b"=" => tk::EQUALS,
            b"<" => tk::LT,
            b">" => tk::GT,
            b"+" => tk::PLUS,
            b"-" => tk::MINUS,
            b"/" => tk::SLASH,
            b"%" => tk::PERCENT,
            b"*" => tk::STAR,
            b"(" => tk::LPAREN,
            b")" => tk::RPAREN,
            b"," => tk::COMMA,
            b";" => tk::SEMI,
            b"." => tk::DOT,
            _ => return None,
        };
        Some((kind, len))
    }

    /// One expression atom: literals, words (with `.` qualification),
    /// function calls, CASE, subqueries, paren groups, `*`.
    #[allow(clippy::too_many_lines)]
    fn parse_atom(&mut self) {
        match self.peek() {
            Some(b'\'') => self.bump_string(),
            Some(b'0'..=b'9' | b'.') => {
                let n = self.scan_number_len();
                if n > 0 {
                    self.bump(tk::NUMBER, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(b'(') => {
                self.bump(tk::LPAREN, 1);
                self.skip_trivia();
                if self.peek_kw("select") {
                    self.events.push(Event::Start {
                        kind: kinds::SUBQUERY,
                        forward_parent: None,
                    });
                    self.parse_select();
                    self.events.push(Event::Finish);
                } else if self.peek() != Some(b')') {
                    self.parse_expr_bp(0);
                }
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark_of(&self.events), kinds::GROUP);
            }
            Some(b'*') => {
                self.events.push(Event::Start {
                    kind: kinds::STAR,
                    forward_parent: None,
                });
                self.bump(tk::STAR, 1);
                self.events.push(Event::Finish);
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                self.parse_word_expr();
            }
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }

    /// A word-led expression: CASE, function call, qualified column,
    /// NULL/TRUE/FALSE literals.
    #[allow(clippy::too_many_lines)]
    fn parse_word_expr(&mut self) {
        let mark = self.events.len();
        // CASE … END.
        if self.peek_kw("case") {
            self.events.push(Event::Start {
                kind: kinds::CASE,
                forward_parent: None,
            });
            self.bump_kw("case");
            self.skip_trivia();
            if !self.peek_kw("when") {
                self.parse_expr_bp(0); // CASE operand form
                self.skip_trivia();
            }
            while self.peek_kw("when") {
                self.events.push(Event::Start {
                    kind: kinds::WHEN,
                    forward_parent: None,
                });
                self.bump_kw("when");
                self.skip_trivia();
                self.parse_expr_bp(0);
                self.skip_trivia();
                if self.peek_kw("then") {
                    self.bump_kw("then");
                    self.skip_trivia();
                    self.parse_expr_bp(0);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.events.push(Event::Finish);
                self.skip_trivia();
            }
            if self.peek_kw("else") {
                self.bump_kw("else");
                self.skip_trivia();
                self.parse_expr_bp(0);
                self.skip_trivia();
            }
            if self.peek_kw("end") {
                self.bump_kw("end");
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.events.push(Event::Finish);
            return;
        }
        self.bump_word();
        self.skip_trivia();
        // Function call `f(args)` — but NOT keyword literals.
        let word_is_literal = self.tokens.last().is_some_and(|(k, r)| {
            *k == tk::WORD && {
                let text = &self.bytes[r.start as usize..r.end as usize];
                let folded = String::from_utf8_lossy(text).to_ascii_lowercase();
                folded == "null" || folded == "true" || folded == "false"
            }
        });
        if !word_is_literal && self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.parse_call_args();
            self.wrap_node(mark, kinds::CALL);
            return;
        }
        // Qualified column `t.c` / `t.*`.
        if self.peek() == Some(b'.') {
            self.bump(tk::DOT, 1);
            self.skip_trivia();
            if self.peek() == Some(b'*') {
                self.bump(tk::STAR, 1);
            } else {
                self.bump_word();
            }
            self.wrap_node(mark, kinds::QUALIFIED);
            return;
        }
        let _ = mark;
    }

    fn parse_call_args(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                _ => {}
            }
            if self.peek() == Some(b'*') {
                self.bump(tk::STAR, 1);
            } else {
                self.parse_expr_bp(0);
            }
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                continue;
            }
            break;
        }
        self.skip_trivia();
        if self.peek() == Some(b')') {
            self.bump(tk::RPAREN, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }
}

/// The events length at `mark` time — a helper for the paren group
/// wrap (the mark equals the current length inside `parse_atom`).
fn mark_of(events: &[Event]) -> usize {
    events.len()
}

/// Binary operator discriminator.
enum BinOpKind {
    /// A keyword operator (`AND`, `IN`, …).
    Word(&'static str),
    /// A punctuation operator.
    Punct(u16),
}

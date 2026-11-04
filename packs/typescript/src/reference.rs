//! Hand-written reference TypeScript-subset parser on the runtime-core
//! event protocol (issue 027). Brace-structured statements with ASI
//! (optional semicolons, newline-sensitive restricted productions), a
//! Pratt expression engine with assignment and arrow operators, a
//! dedicated type grammar whose generic closes split two-byte `>>`
//! tokens, and `//`/`/* … */` doc comments as trivia.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::ERROR_KIND;
use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

use crate::kinds;
use crate::token_kinds as tk;

/// Diagnostic side-table index meaning "syntax error" — the only message
/// the v0 reference parser emits (resolved by [`error_message`]).
pub const DIAG_SYNTAX: u32 = 0;

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse TypeScript-subset source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the JSON/TOML/Python packs):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; whitespace, newlines, and comments are *never* token
///   events.
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)`.
/// - **Errors**: `Error` + an ERROR node around the offending region;
///   recovery skips to the next `;` / bracket closer and keeps parsing
///   — deterministic and lossless.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse TypeScript-subset source into a green tree (one implicit
/// [`crate::ROOT_KIND`] root).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let mut parser = Parser::run(source);
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
    // NodeCache is !Sync by design (single-threaded parser core) — the
    // Arc satisfies the builder's shared-ownership signature.
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

/// Recursive-descent + Pratt driver over the source bytes.
struct Parser<'s> {
    bytes: &'s [u8],
    pos: u32,
    /// End of the last emitted clean token (gaps carry trivia).
    last_end: u32,
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    /// `(range, trivia kind, trailing)` covering every gap.
    trivia: Vec<(Range<u32>, u16, bool)>,
    errors: usize,
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
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos as usize).copied()
    }

    fn at(&self, offset: u32) -> Option<u8> {
        self.bytes.get(self.pos as usize + offset as usize).copied()
    }

    fn starts_with(&self, text: &[u8]) -> bool {
        let start = self.pos as usize;
        self.bytes.len() >= start + text.len() && &self.bytes[start..start + text.len()] == text
    }

    /// Whether a keyword (`if`, `of`, …) starts here as a standalone
    /// word (the scanner emits `NAME` for identifiers and keywords).
    fn peek_kw(&self, kw: &str) -> bool {
        if !self.starts_with(kw.as_bytes()) {
            return false;
        }
        match self.at(u32::try_from(kw.len()).unwrap_or(u32::MAX)) {
            None => true,
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'_'),
        }
    }

    fn bump_kw(&mut self, kw: &str) {
        self.bump(tk::NAME, u32::try_from(kw.len()).unwrap_or(u32::MAX));
    }

    /// Skip whitespace, newlines, and comments — all trivia, always
    /// (statement structure is brace + ASI based, not line based).
    fn skip_trivia(&mut self) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') => self.pos += 1,
                Some(b'/') if self.at(1) == Some(b'/') => {
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
                    if self.pos > start && kind != tk::COMMENT {
                        self.push_trivia(start, tk::WHITESPACE, false);
                        start = self.pos;
                    }
                    kind = tk::COMMENT;
                    self.pos += 2;
                    while let Some(b) = self.peek() {
                        self.pos += 1;
                        if b == b'*' && self.peek() == Some(b'/') {
                            self.pos += 1;
                            break;
                        }
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

    /// Emit a token event + clean range `[start .. start + n)`.
    fn bump(&mut self, kind: u16, n: u32) {
        let start = self.pos;
        self.pos += n;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, start..self.pos));
        self.last_end = self.pos;
    }

    /// Insert a `Start` event retroactively at `mark` and close the node
    /// at the current end — the precede-insert discipline.
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

    /// Error + ERROR node around the skipped region. Sync: stops before
    /// `;`, a bracket closer, or EOF; a stray non-closer byte rides the
    /// error region when `consume_stray` (progress is guaranteed by the
    /// structural loops consuming the boundary or by the loop guards).
    fn fail_skip(&mut self, start: u32, consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if matches!(b, b';' | b')' | b']' | b'}' | b'{') {
                break;
            }
            self.pos += 1;
        }
        if consume_stray && self.pos == start {
            if let Some(b) = self.peek() {
                if !matches!(b, b';' | b')' | b']' | b'}' | b'{') {
                    self.pos += 1;
                }
            }
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

    /// Consume exactly one byte as an ERROR region — the loop guard of
    /// last resort when a statement parsed nothing (stray closers).
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

    /// ASI: consume `;` when present (statements end at a newline, `}`,
    /// or EOF without one).
    fn asi(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        }
    }

    /// Restricted-production check: does a newline sit between the
    /// cursor and the next token (before any comment)?
    fn newline_before_token(&self) -> bool {
        let mut idx = self.pos as usize;
        while let Some(b) = self.bytes.get(idx) {
            match b {
                b' ' | b'\t' | b'\r' | b'\x0c' => idx += 1,
                b'\n' => return true,
                _ => return false,
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Scanners
    // ------------------------------------------------------------------

    fn scan_name_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if !self
            .bytes
            .get(idx)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
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

    /// Numeric literal length: decimal/exponent bodies, `0x`/`0o`/`0b`
    /// radix forms, and the `n` `BigInt` suffix.
    fn scan_number_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if idx + 1 < self.bytes.len() && self.bytes[idx] == b'0' {
            match self.bytes[idx + 1] {
                b'x' | b'X' | b'o' | b'O' | b'b' | b'B' => {
                    idx += 2;
                    while idx < self.bytes.len()
                        && (self.bytes[idx].is_ascii_alphanumeric() || self.bytes[idx] == b'_')
                    {
                        idx += 1;
                    }
                    return u32::try_from(idx - start).unwrap_or(1);
                }
                _ => {}
            }
        }
        while idx < self.bytes.len()
            && (self.bytes[idx].is_ascii_digit() || self.bytes[idx] == b'_')
        {
            idx += 1;
        }
        if idx < self.bytes.len() && self.bytes[idx] == b'.' {
            idx += 1;
            while idx < self.bytes.len()
                && (self.bytes[idx].is_ascii_digit() || self.bytes[idx] == b'_')
            {
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
                while idx < self.bytes.len()
                    && (self.bytes[idx].is_ascii_digit() || self.bytes[idx] == b'_')
                {
                    idx += 1;
                }
            }
        }
        if idx < self.bytes.len() && self.bytes[idx] == b'n' {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Plain string literal: quote, `\` escapes, closing quote.
    /// `None` = unterminated.
    fn scan_string_len(&self, quote: u8) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        loop {
            match self.bytes.get(idx)? {
                b'\\' => idx += 2,
                b'\n' => return None,
                b if *b == quote => return u32::try_from(idx + 1 - start).ok(),
                _ => idx += 1,
            }
        }
    }

    /// Whether the `(`-group at the cursor is arrow-function params:
    /// balance to the matching `)`, then (skipping trivia and an
    /// optional `: …` return-type annotation, bracket-balanced) look
    /// for `=>`. Bounded scan (8 KiB).
    fn paren_group_is_arrow(&self) -> bool {
        let start = self.pos as usize;
        let mut idx = start + 1;
        let mut depth = 1_u32;
        while idx < self.bytes.len() && self.bytes.len() <= idx + 8192 {
            match self.bytes[idx] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        idx += 1;
                        break;
                    }
                }
                b'"' | b'\'' => {
                    // skip a string
                    let q = self.bytes[idx];
                    idx += 1;
                    while idx < self.bytes.len() {
                        if self.bytes[idx] == b'\\' {
                            idx += 2;
                            continue;
                        }
                        if self.bytes[idx] == q || self.bytes[idx] == b'\n' {
                            break;
                        }
                        idx += 1;
                    }
                }
                b'`' => {
                    // skip a template
                    idx += 1;
                    while idx < self.bytes.len() && self.bytes[idx] != b'`' {
                        idx += if self.bytes[idx] == b'\\' { 2 } else { 1 };
                    }
                }
                _ => {}
            }
            idx += 1;
        }
        if depth != 0 {
            return false;
        }
        // Skip trivia + optional `: annotation`, then require `=>`.
        let mut guard = 0_usize;
        while idx < self.bytes.len() && guard < 8192 {
            guard += 1;
            match self.bytes[idx] {
                b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' => idx += 1,
                b'/' if self.bytes.get(idx + 1) == Some(&b'/') => {
                    while idx < self.bytes.len() && self.bytes[idx] != b'\n' {
                        idx += 1;
                    }
                }
                b'/' if self.bytes.get(idx + 1) == Some(&b'*') => {
                    idx += 2;
                    while idx + 1 < self.bytes.len()
                        && !(self.bytes[idx] == b'*' && self.bytes[idx + 1] == b'/')
                    {
                        idx += 1;
                    }
                    idx = (idx + 2).min(self.bytes.len());
                }
                b':' => {
                    // Return-type annotation: skip balanced to `=>`.
                    idx += 1;
                    let mut tdepth = 0_i64;
                    let mut tguard = 0_usize;
                    while idx < self.bytes.len() && tguard < 8192 {
                        tguard += 1;
                        match self.bytes[idx] {
                            b'(' | b'[' | b'{' | b'<' => tdepth += 1,
                            b')' | b']' | b'}' | b'>' => tdepth -= 1,
                            b';' if tdepth == 0 => return false,
                            _ => {}
                        }
                        if tdepth < 0 {
                            return false;
                        }
                        if tdepth == 0 && self.bytes[idx..].starts_with(b"=>") {
                            return true;
                        }
                        idx += 1;
                    }
                    return false;
                }
                b'=' if self.bytes.get(idx + 1) == Some(&b'>') => return true,
                _ => return false,
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // Module / statements
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        while self.peek().is_some() {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }
            if self.peek() == Some(b'}') {
                // Stray closer at module level: force past it.
                self.force_byte();
                continue;
            }
            let before = self.pos;
            self.parse_statement();
            if self.pos == before && self.peek().is_some() {
                self.force_byte();
            }
        }
    }

    /// Statements + declarations sharing one dispatch table.
    fn parse_statement(&mut self) {
        self.skip_trivia();
        if self.peek_kw("import") {
            self.parse_import();
        } else if self.peek_kw("export") {
            self.parse_export();
        } else if self.peek_kw("interface") {
            self.parse_interface();
        } else if self.peek_kw("type") && self.name_after_kw("type") {
            self.parse_type_alias();
        } else if self.peek_kw("enum")
            || (self.peek_kw("const") && self.kw_after_is("const", "enum"))
        {
            self.parse_enum();
        } else if self.peek_kw("class") {
            self.parse_class();
        } else if self.peek_kw("function") {
            self.parse_function();
        } else if self.peek_kw("const") || self.peek_kw("let") || self.peek_kw("var") {
            self.parse_var_decl();
        } else if self.peek_kw("if")
            || self.peek_kw("while")
            || self.peek_kw("for")
            || self.peek_kw("return")
            || self.peek_kw("break")
            || self.peek_kw("continue")
            || self.peek_kw("throw")
        {
            self.parse_control();
        } else if self.peek() == Some(b'{') {
            self.parse_block();
        } else {
            self.parse_expr_stmt();
        }
        self.asi();
    }

    /// Whether a declaration name follows the `type` keyword (keeps
    /// `type` usable as an ordinary identifier).
    fn name_after_kw(&self, kw: &str) -> bool {
        let len = kw.len();
        let mut idx = self.pos as usize + len;
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t')) {
            idx += 1;
        }
        self.bytes
            .get(idx)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
    }

    /// Whether the word after `first` is `second` (for `const enum`).
    fn kw_after_is(&self, first: &str, second: &str) -> bool {
        let mut idx = self.pos as usize + first.len();
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t')) {
            idx += 1;
        }
        self.starts_with_at(second.as_bytes(), idx)
            && !self
                .bytes
                .get(idx + second.len())
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
    }

    fn starts_with_at(&self, text: &[u8], idx: usize) -> bool {
        self.bytes.len() >= idx + text.len() && &self.bytes[idx..idx + text.len()] == text
    }

    fn parse_import(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IMPORT,
            forward_parent: None,
        });
        self.bump_kw("import");
        self.skip_trivia();
        if self.peek() == Some(b'"') || self.peek() == Some(b'\'') {
            self.bump_string();
        } else {
            // Default alias, namespace, or named list — comma-composed.
            if self.peek() == Some(b'*') {
                self.bump(tk::STAR, 1);
                self.skip_trivia();
                if self.peek_kw("as") {
                    self.bump_kw("as");
                    self.skip_trivia();
                    self.events.push(Event::Start {
                        kind: kinds::ALIAS,
                        forward_parent: None,
                    });
                    self.bump_name();
                    self.events.push(Event::Finish);
                }
            } else {
                let n = self.scan_name_len();
                if n > 0 {
                    self.events.push(Event::Start {
                        kind: kinds::ALIAS,
                        forward_parent: None,
                    });
                    self.bump(tk::NAME, n);
                    self.events.push(Event::Finish);
                    self.skip_trivia();
                    if self.peek() == Some(b',') {
                        self.bump(tk::COMMA, 1);
                        self.skip_trivia();
                    } else {
                        // `import x from …` — no list.
                    }
                }
            }
            self.skip_trivia();
            if self.peek() == Some(b'{') {
                self.bump(tk::LBRACE, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b'}') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    self.events.push(Event::Start {
                        kind: kinds::ALIAS,
                        forward_parent: None,
                    });
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
                    } else if matches!(self.peek(), Some(b'"' | b'\'')) {
                        self.bump_string();
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.skip_trivia();
                    if self.peek_kw("as") {
                        self.bump_kw("as");
                        self.skip_trivia();
                        self.bump_name();
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
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            self.skip_trivia();
            if self.peek_kw("from") {
                self.bump_kw("from");
                self.skip_trivia();
                self.bump_string();
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        self.events.push(Event::Finish);
    }

    fn parse_export(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::EXPORT,
            forward_parent: None,
        });
        self.bump_kw("export");
        self.skip_trivia();
        if self.peek() == Some(b'*') {
            self.bump(tk::STAR, 1);
            self.skip_trivia();
            if self.peek_kw("as") {
                self.bump_kw("as");
                self.skip_trivia();
                self.bump_name();
                self.skip_trivia();
            }
            if self.peek_kw("from") {
                self.bump_kw("from");
                self.skip_trivia();
                self.bump_string();
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    break;
                }
                if self.peek().is_none() {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                self.events.push(Event::Start {
                    kind: kinds::ALIAS,
                    forward_parent: None,
                });
                self.bump_name();
                self.skip_trivia();
                if self.peek_kw("as") {
                    self.bump_kw("as");
                    self.skip_trivia();
                    self.bump_name();
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
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia();
            if self.peek_kw("from") {
                self.bump_kw("from");
                self.skip_trivia();
                self.bump_string();
            }
        } else if self.peek_kw("default") {
            self.bump_kw("default");
            self.skip_trivia();
            self.parse_expr_bp(0);
        } else {
            // An exported declaration nests inside the EXPORT node.
            self.parse_statement();
        }
        self.events.push(Event::Finish);
    }

    /// `interface Name<T> { members }` — members end at `;`/`,`/ASI.
    fn parse_interface(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::INTERFACE,
            forward_parent: None,
        });
        self.bump_kw("interface");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_type_params();
        }
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            self.parse_interface_members();
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
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

    /// Interface body members: `name?: Type`, `name?(p): Ret`, separated
    /// by `;`/`,`/nothing (ASI-style, like the member grammar).
    fn parse_interface_members(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'}') | None => break,
                _ => {}
            }
            self.events.push(Event::Start {
                kind: kinds::MEMBER,
                forward_parent: None,
            });
            let n = self.scan_name_len();
            if n > 0 {
                self.bump(tk::NAME, n);
            } else if matches!(self.peek(), Some(b'"' | b'\'')) {
                self.bump_string();
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia();
            if self.peek() == Some(b'?') {
                self.bump(tk::QUESTION, 1);
                self.skip_trivia();
            }
            if self.peek() == Some(b'(') {
                // Method signature.
                self.bump(tk::LPAREN, 1);
                self.parse_params();
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    self.skip_trivia();
                    self.parse_type();
                }
            } else if self.peek() == Some(b'<') {
                // Generic method signature.
                self.parse_type_params();
                self.skip_trivia();
                if self.peek() == Some(b'(') {
                    self.bump(tk::LPAREN, 1);
                    self.parse_params();
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_type();
                    }
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            } else if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_type();
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.events.push(Event::Finish);
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            } else if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
            }
            if self.peek() == Some(b'}') || self.peek().is_none() {
                break;
            }
            // ASI: the next member starts directly.
        }
    }

    fn parse_type_alias(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::TYPE_ALIAS,
            forward_parent: None,
        });
        self.bump_kw("type");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_type_params();
        }
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::EQUALS, 1);
            self.skip_trivia();
            self.parse_type();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// `<T extends U = Default, …>` — with the `>>`-split close.
    fn parse_type_params(&mut self) {
        self.bump(tk::LT, 1);
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'>') {
                break;
            }
            if self.peek().is_none() {
                let start = self.pos;
                self.fail_skip(start, true);
                break;
            }
            self.events.push(Event::Start {
                kind: kinds::TYPE_PARAM,
                forward_parent: None,
            });
            if self.peek_kw("const") {
                self.bump_kw("const");
                self.skip_trivia();
            }
            self.bump_name();
            self.skip_trivia();
            if self.peek_kw("extends") {
                self.bump_kw("extends");
                self.skip_trivia();
                self.parse_type();
                self.skip_trivia();
            }
            if self.peek() == Some(b'=') {
                self.bump(tk::EQUALS, 1);
                self.skip_trivia();
                self.parse_type();
            }
            self.events.push(Event::Finish);
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                continue;
            }
            break;
        }
        self.close_generic();
    }

    /// Close a generic argument list: emit one `>` token — for `>>`/
    /// `>=`/`>>>`/`>>=` the remaining bytes re-lex naturally (the
    /// nested-generic split).
    fn close_generic(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'>') {
            self.bump(tk::GT, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// `enum` / `const enum` declaration.
    fn parse_enum(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ENUM,
            forward_parent: None,
        });
        if self.peek_kw("const") {
            self.bump_kw("const");
            self.skip_trivia();
        }
        self.bump_kw("enum");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    break;
                }
                if self.peek().is_none() {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                self.events.push(Event::Start {
                    kind: kinds::PAIR,
                    forward_parent: None,
                });
                self.bump_name();
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
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
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

    /// `class Name<T> extends B implements I, J { members }`.
    fn parse_class(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CLASS,
            forward_parent: None,
        });
        self.bump_kw("class");
        self.skip_trivia();
        let n = self.scan_name_len();
        if n > 0 {
            self.bump(tk::NAME, n);
        }
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_type_params();
        }
        self.skip_trivia();
        while self.peek_kw("extends") || self.peek_kw("implements") {
            self.bump_kw(if self.peek_kw("extends") {
                "extends"
            } else {
                "implements"
            });
            loop {
                self.skip_trivia();
                self.parse_type_ref();
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.skip_trivia();
        }
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            self.parse_class_members();
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
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

    /// Class body: `[modifiers] name [?(params): Ret] [= init]` —
    /// methods keep a METHOD node, fields a FIELD node.
    fn parse_class_members(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'}') | None => break,
                _ => {}
            }
            let before = self.pos;
            self.events.push(Event::Start {
                kind: kinds::METHOD,
                forward_parent: None,
            });
            // Modifier keywords stay as NAME tokens inside the node.
            while self.peek_kw("static")
                || self.peek_kw("readonly")
                || self.peek_kw("public")
                || self.peek_kw("private")
                || self.peek_kw("protected")
                || self.peek_kw("abstract")
                || self.peek_kw("override")
            {
                self.bump_name();
                self.skip_trivia();
            }
            let n = self.scan_name_len();
            if n > 0 {
                self.bump(tk::NAME, n);
            } else if matches!(self.peek(), Some(b'"' | b'\'')) {
                self.bump_string();
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
                self.events.push(Event::Finish);
                if self.pos == before {
                    self.force_byte();
                }
                continue;
            }
            self.skip_trivia();
            if self.peek() == Some(b'?') {
                self.bump(tk::QUESTION, 1);
                self.skip_trivia();
            }
            if self.peek() == Some(b'<') {
                self.parse_type_params();
                self.skip_trivia();
            }
            if self.peek() == Some(b'(') {
                // Method (or constructor): signature + body.
                self.bump(tk::LPAREN, 1);
                self.parse_params();
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    self.skip_trivia();
                    self.parse_type();
                    self.skip_trivia();
                }
                if self.peek() == Some(b'{') {
                    self.parse_block();
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            } else {
                // Field: optional `: T`, optional `= init`.
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    self.skip_trivia();
                    self.parse_type();
                    self.skip_trivia();
                }
                if self.peek() == Some(b'=') {
                    self.bump(tk::EQUALS, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
                }
            }
            self.events.push(Event::Finish);
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            }
        }
    }

    /// `function name<T>(params): Ret { … }`.
    fn parse_function(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FUNCTION,
            forward_parent: None,
        });
        self.bump_kw("function");
        self.skip_trivia();
        if self.peek() == Some(b'*') {
            // Generators are out of the subset; keep the token + error.
            self.bump(tk::STAR, 1);
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_type_params();
        }
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.parse_params();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_type();
        }
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// Parameter list: `name[?][: T][= default]`, `...rest: T[]`.
    fn parse_params(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PARAMS,
            forward_parent: None,
        });
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                Some(b'.') if self.starts_with(b"...") => {
                    self.bump(tk::ELLIPSIS, 3);
                    self.skip_trivia();
                    self.bump_name();
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_type();
                    }
                }
                _ => {
                    let n = self.scan_name_len();
                    if n == 0 {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    } else {
                        self.bump(tk::NAME, n);
                        self.skip_trivia();
                        if self.peek() == Some(b'?') {
                            self.bump(tk::QUESTION, 1);
                            self.skip_trivia();
                        }
                        if self.peek() == Some(b':') {
                            self.bump(tk::COLON, 1);
                            self.skip_trivia();
                            self.parse_type();
                            self.skip_trivia();
                        }
                        if self.peek() == Some(b'=') {
                            self.bump(tk::EQUALS, 1);
                            self.skip_trivia();
                            self.parse_expr_bp(1);
                        }
                    }
                }
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
    }

    /// `const`/`let`/`var` declarations (destructuring included).
    fn parse_var_decl(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::VAR_DECL,
            forward_parent: None,
        });
        self.bump_kw(if self.peek_kw("const") {
            "const"
        } else if self.peek_kw("let") {
            "let"
        } else {
            "var"
        });
        loop {
            self.skip_trivia();
            self.parse_binding_pattern();
            self.skip_trivia();
            if self.peek() == Some(b'!') && self.at(1) != Some(b'=') {
                self.bump(tk::BANG, 1);
                self.skip_trivia();
            }
            if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_type();
                self.skip_trivia();
            }
            if self.peek() == Some(b'=') {
                self.bump(tk::EQUALS, 1);
                self.skip_trivia();
                self.parse_expr_bp(1);
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

    /// A binding target: a name, a `{…}` object pattern, or a `[…]`
    /// array pattern.
    fn parse_binding_pattern(&mut self) {
        match self.peek() {
            Some(b'{') => {
                let mark = self.events.len();
                self.bump(tk::LBRACE, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b'}') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    self.events.push(Event::Start {
                        kind: kinds::PAIR,
                        forward_parent: None,
                    });
                    if self.starts_with(b"...") {
                        self.bump(tk::ELLIPSIS, 3);
                        self.skip_trivia();
                    }
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_binding_pattern();
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
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::OBJECT);
            }
            Some(b'[') => {
                let mark = self.events.len();
                self.bump(tk::LBRACKET, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    if self.starts_with(b"...") {
                        self.bump(tk::ELLIPSIS, 3);
                        self.skip_trivia();
                    }
                    self.parse_binding_pattern();
                    self.skip_trivia();
                    if self.peek() == Some(b',') {
                        self.bump(tk::COMMA, 1);
                        continue;
                    }
                    break;
                }
                self.skip_trivia();
                if self.peek() == Some(b']') {
                    self.bump(tk::RBRACKET, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::ARRAY);
            }
            _ => {
                self.bump_name();
            }
        }
    }

    /// `if` / `while` / `for` / `return` / `break` / `continue` / `throw`.
    #[allow(clippy::too_many_lines)] // one control-flow dispatcher
    fn parse_control(&mut self) {
        if self.peek_kw("if") {
            self.events.push(Event::Start {
                kind: kinds::IF,
                forward_parent: None,
            });
            self.bump_kw("if");
            self.skip_trivia();
            if self.peek() == Some(b'(') {
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
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.parse_stmt_body();
            self.skip_trivia();
            if self.peek_kw("else") {
                self.events.push(Event::Start {
                    kind: kinds::ELSE,
                    forward_parent: None,
                });
                self.bump_kw("else");
                self.parse_stmt_body();
                self.events.push(Event::Finish);
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("while") {
            self.events.push(Event::Start {
                kind: kinds::WHILE,
                forward_parent: None,
            });
            self.bump_kw("while");
            self.skip_trivia();
            if self.peek() == Some(b'(') {
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
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.parse_stmt_body();
            self.events.push(Event::Finish);
        } else if self.peek_kw("for") {
            self.parse_for();
        } else if self.peek_kw("return") {
            self.events.push(Event::Start {
                kind: kinds::RETURN,
                forward_parent: None,
            });
            self.bump_kw("return");
            self.skip_trivia();
            // Restricted production: a newline after `return` ends it.
            if !self.newline_before_token() && !matches!(self.peek(), None | Some(b';' | b'}')) {
                self.parse_expr_bp(0);
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("throw") {
            self.events.push(Event::Start {
                kind: kinds::THROW,
                forward_parent: None,
            });
            self.bump_kw("throw");
            self.skip_trivia();
            if self.newline_before_token() || matches!(self.peek(), None | Some(b';')) {
                let start = self.pos;
                self.fail_skip(start, true);
            } else {
                self.parse_expr_bp(0);
            }
            self.events.push(Event::Finish);
        } else {
            // break / continue
            let kind = if self.peek_kw("break") {
                kinds::BREAK
            } else {
                kinds::CONTINUE
            };
            self.events.push(Event::Start {
                kind,
                forward_parent: None,
            });
            self.bump_kw(if kind == kinds::BREAK {
                "break"
            } else {
                "continue"
            });
            self.events.push(Event::Finish);
        }
    }

    /// The body of `if`/`while`/`for`/`else`: a block or one statement.
    fn parse_stmt_body(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else if self.peek().is_some() && self.peek() != Some(b';') {
            self.parse_statement();
            // The inner statement's trailing ASI already ran.
        } else {
            self.asi(); // empty body: `;`
        }
    }

    /// `for (init; cond; update)` / `for (x of xs)` / `for (k in o)`.
    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FOR,
            forward_parent: None,
        });
        self.bump_kw("for");
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.skip_trivia();
            // Head: a declaration or expression, then `;` or `of`/`in`.
            if self.peek_kw("const") || self.peek_kw("let") || self.peek_kw("var") {
                self.parse_var_decl();
            } else if self.peek() != Some(b';') {
                self.parse_expr_bp(0);
            }
            self.skip_trivia();
            if self.peek_kw("of") || self.peek_kw("in") {
                let is_of = self.peek_kw("of");
                self.bump_kw(if is_of { "of" } else { "in" });
                self.skip_trivia();
                self.parse_expr_bp(1);
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.parse_stmt_body();
                self.events.push(Event::Finish);
                return;
            }
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            // Condition.
            self.skip_trivia();
            if self.peek() != Some(b';') {
                self.parse_expr_bp(0);
            }
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            // Update.
            self.skip_trivia();
            if self.peek() != Some(b')') {
                self.parse_expr_bp(0);
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
        self.parse_stmt_body();
        self.events.push(Event::Finish);
    }

    fn parse_block(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::BLOCK,
            forward_parent: None,
        });
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            loop {
                self.skip_trivia();
                match self.peek() {
                    Some(b'}') | None => break,
                    _ => {}
                }
                let before = self.pos;
                self.parse_statement();
                if self.pos == before && self.peek().is_some() {
                    self.force_byte();
                }
            }
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
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

    fn parse_expr_stmt(&mut self) {
        let mark = self.events.len();
        self.skip_trivia();
        self.parse_expr_bp(0);
        self.wrap_node(mark, kinds::EXPR_STMT);
    }

    /// One imported/exported name or any identifier at the cursor.
    fn bump_name(&mut self) {
        let n = self.scan_name_len();
        if n > 0 {
            self.bump(tk::NAME, n);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    fn bump_string(&mut self) {
        if let Some(q @ (b'"' | b'\'')) = self.peek() {
            if let Some(n) = self.scan_string_len(q) {
                self.bump(tk::STRING, n);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    /// A type: `atom (| atom)*` union.
    fn parse_type(&mut self) {
        let mark = self.events.len();
        self.parse_type_atom();
        let mut union = false;
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'|') {
                self.bump(tk::PIPE, 1);
                self.skip_trivia();
                self.parse_type_atom();
                union = true;
                continue;
            }
            break;
        }
        if union {
            self.wrap_node(mark, kinds::TYPE_UNION);
        }
    }

    /// One type atom: reference (with generics + array suffixes), tuple,
    /// parenthesized group, object literal type, or literal.
    fn parse_type_atom(&mut self) {
        match self.peek() {
            Some(b'{') => {
                // Object type literal — the member grammar shared with
                // interfaces, under an OBJECT node.
                let mark = self.events.len();
                self.bump(tk::LBRACE, 1);
                self.parse_interface_members();
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::OBJECT);
                self.parse_type_array_suffix(mark);
            }
            Some(b'(') => {
                let mark = self.events.len();
                self.bump(tk::LPAREN, 1);
                self.skip_trivia();
                if self.peek() != Some(b')') {
                    self.parse_type();
                }
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::TYPE_GROUP);
                self.parse_type_array_suffix(mark);
            }
            Some(b'[') => {
                let mark = self.events.len();
                self.bump(tk::LBRACKET, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    self.parse_type();
                    self.skip_trivia();
                    if self.peek() == Some(b',') {
                        self.bump(tk::COMMA, 1);
                        continue;
                    }
                    break;
                }
                self.skip_trivia();
                if self.peek() == Some(b']') {
                    self.bump(tk::RBRACKET, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::TYPE_TUPLE);
                self.parse_type_array_suffix(mark);
            }
            Some(b) if b == b'"' || b == b'\'' => self.bump_string(),
            Some(b'0'..=b'9') => {
                let n = self.scan_number_len();
                if n > 0 {
                    self.bump(tk::NUMBER, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(_)
                if self.peek_kw("string")
                    || self.peek_kw("number")
                    || self.peek_kw("boolean")
                    || self.peek_kw("null")
                    || self.peek_kw("undefined")
                    || self.peek_kw("void")
                    || self.peek_kw("never")
                    || self.peek_kw("unknown")
                    || self.peek_kw("any")
                    || self.peek_kw("object") =>
            {
                // Keyword types ride TYPE_REF nodes like named types.
                let mark = self.events.len();
                self.bump_name();
                self.wrap_node(mark, kinds::TYPE_REF);
                self.parse_type_array_suffix(mark);
            }
            Some(_) => {
                self.parse_type_ref();
            }
            None => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }

    /// `Name<Args>` with `T[]` array suffixes.
    fn parse_type_ref(&mut self) {
        let mark = self.events.len();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.bump(tk::LT, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b'>') {
                    break;
                }
                if self.peek().is_none() {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                self.parse_type();
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.close_generic();
        }
        self.wrap_node(mark, kinds::TYPE_REF);
        self.parse_type_array_suffix(mark);
    }

    /// `[]` array-type suffixes (repeatable: `T[][]`) — each suffix
    /// re-scopes the type node built since `mark` into a `TYPE_ARRAY`.
    fn parse_type_array_suffix(&mut self, mark: usize) {
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'[') {
                self.bump(tk::LBRACKET, 1);
                self.skip_trivia();
                if self.peek() == Some(b']') {
                    self.bump(tk::RBRACKET, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.events.insert(
                    mark,
                    Event::Start {
                        kind: kinds::TYPE_ARRAY,
                        forward_parent: None,
                    },
                );
                self.events.push(Event::Finish);
            } else {
                break;
            }
        }
    }

    // ------------------------------------------------------------------
    // Pratt expressions
    // ------------------------------------------------------------------

    /// Pratt core: prefix unary, atom (+ postfix), then binary/assign/
    /// arrow operators by binding power, then the ternary conditional.
    /// Nodes wrap retroactively via [`Self::wrap_node`] at `mark`.
    fn parse_expr_bp(&mut self, min_bp: u8) {
        let mark = self.events.len();
        if let Some((kind, n, rbp)) = self.peek_unary() {
            self.bump(kind, n);
            self.skip_trivia();
            if kind == tk::NAME {
                // `new Callee(args)` / `typeof x` — the operand is a
                // full postfix expression.
                self.parse_postfix(mark);
            } else {
                self.parse_expr_bp(rbp);
            }
            self.wrap_node(mark, kinds::UNARY);
        } else if self.peek() == Some(b'(') && self.paren_group_is_arrow() {
            self.parse_arrow_params(mark);
        } else {
            self.parse_atom();
        }
        self.skip_trivia();
        self.parse_postfix(mark);
        self.skip_trivia();
        self.parse_binops(mark, min_bp);
        self.skip_trivia();
        if min_bp == 0 && self.peek() == Some(b'?') {
            self.bump(tk::QUESTION, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_expr_bp(0);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.wrap_node(mark, kinds::CONDITIONAL);
        }
    }

    /// Prefix unary operators; `new`/`typeof` ride `NAME` tokens.
    fn peek_unary(&self) -> Option<(u16, u32, u8)> {
        match self.peek() {
            Some(b'-') if !self.starts_with(b"--") && !self.starts_with(b"-=") => {
                Some((tk::MINUS, 1, 13))
            }
            Some(b'+') if !self.starts_with(b"++") && !self.starts_with(b"+=") => {
                Some((tk::PLUS, 1, 13))
            }
            Some(b'!') if !self.starts_with(b"!=") && !self.starts_with(b"!==") => {
                Some((tk::BANG, 1, 13))
            }
            Some(b'~') => Some((tk::TILDE, 1, 13)),
            Some(b'+') if self.starts_with(b"++") => Some((tk::INCREMENT, 2, 14)),
            Some(b'-') if self.starts_with(b"--") => Some((tk::DECREMENT, 2, 14)),
            Some(_) if self.peek_kw("new") => Some((tk::NAME, 3, 14)),
            Some(_) if self.peek_kw("typeof") => Some((tk::NAME, 6, 13)),
            _ => None,
        }
    }

    /// Postfix loop: calls, index access, property access, `++`/`--` —
    /// each wraps the whole left extent via precede-insert at `mark`.
    fn parse_postfix(&mut self, mark: usize) {
        loop {
            match self.peek() {
                Some(b'(') => {
                    self.bump(tk::LPAREN, 1);
                    self.parse_call_args();
                    self.wrap_node(mark, kinds::CALL);
                }
                Some(b'[') => {
                    self.bump(tk::LBRACKET, 1);
                    self.skip_trivia();
                    if self.peek() != Some(b']') && self.peek().is_some() {
                        self.parse_expr_bp(0);
                        self.skip_trivia();
                    }
                    if self.peek() == Some(b']') {
                        self.bump(tk::RBRACKET, 1);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.wrap_node(mark, kinds::INDEX);
                }
                Some(b'.') => {
                    self.bump(tk::DOT, 1);
                    self.skip_trivia();
                    self.bump_name();
                    self.wrap_node(mark, kinds::ATTRIBUTE);
                }
                Some(b'+') if self.starts_with(b"++") => {
                    self.bump(tk::INCREMENT, 2);
                    self.wrap_node(mark, kinds::UNARY);
                }
                Some(b'-') if self.starts_with(b"--") => {
                    self.bump(tk::DECREMENT, 2);
                    self.wrap_node(mark, kinds::UNARY);
                }
                _ => break,
            }
            self.skip_trivia();
        }
    }

    /// Call/class-base arguments: comma-separated expressions, trailing
    /// comma tolerated.
    fn parse_call_args(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                _ => {}
            }
            if self.starts_with(b"...") {
                self.bump(tk::ELLIPSIS, 3);
                self.skip_trivia();
            }
            self.parse_expr_bp(1);
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

    /// Parenthesized arrow-function params (the pre-scan decided):
    /// `(a, b?: T = 1, ...rest)` `=>` body.
    fn parse_arrow_params(&mut self, mark: usize) {
        self.bump(tk::LPAREN, 1);
        self.parse_params();
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_type();
            self.skip_trivia();
        }
        if self.peek() == Some(b'=') && self.at(1) == Some(b'>') {
            self.bump(tk::ARROW_FN, 2);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_arrow_body();
        self.wrap_node(mark, kinds::ARROW);
    }

    /// Arrow body: a block or an assignment-level expression.
    fn parse_arrow_body(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else {
            self.parse_expr_bp(1);
        }
    }

    /// Binary/assignment/arrow operators by binding power. Assignment
    /// forms are right-associative; `=>` chains the same way.
    fn parse_binops(&mut self, mark: usize, min_bp: u8) {
        while let Some((lbp, rbp, kind)) = self.peek_binop() {
            if lbp < min_bp {
                break;
            }
            if kind == tk::NAME {
                // Keyword operators: `in` / `instanceof`.
                self.bump_kw(if self.peek_kw("instanceof") {
                    "instanceof"
                } else {
                    "in"
                });
            } else {
                self.bump(kind, self.punct_len());
            }
            self.skip_trivia();
            if kind == tk::ARROW_FN {
                self.parse_arrow_body();
            } else {
                self.parse_expr_bp(rbp);
            }
            self.wrap_node(
                mark,
                if kind == tk::ARROW_FN {
                    kinds::ARROW
                } else if Self::is_assign_op(kind) {
                    kinds::ASSIGN
                } else {
                    kinds::BINOP
                },
            );
        }
    }

    fn is_assign_op(kind: u16) -> bool {
        matches!(
            kind,
            tk::EQUALS
                | tk::PLUSEQ
                | tk::MINUSEQ
                | tk::STAREQ
                | tk::SLASHEQ
                | tk::PERCENTEQ
                | tk::POWEREQ
                | tk::AND_EQ
                | tk::OR_EQ
                | tk::AMP_EQ
                | tk::PIPE_EQ
                | tk::CARET_EQ
                | tk::LSHIFT_EQ
                | tk::SHIFT_EQ
                | tk::USHIFT_EQ
        )
    }

    /// The byte length of the operator at the cursor.
    fn punct_len(&self) -> u32 {
        const THREE: &[&[u8]] = &[
            b">>>", b"===", b"!==", b"**=", b"&&=", b"||=", b"<<=", b">>=", b"...",
        ];
        const TWO: &[&[u8]] = &[
            b"**", b"==", b"!=", b"<=", b">=", b"&&", b"||", b"<<", b">>", b"++", b"--", b"+=",
            b"-=", b"*=", b"/=", b"%=", b"&=", b"|=", b"^=", b"=>",
        ];
        for text in THREE {
            if self.starts_with(text) {
                return u32::try_from(text.len()).unwrap_or(u32::MAX);
            }
        }
        for text in TWO {
            if self.starts_with(text) {
                return u32::try_from(text.len()).unwrap_or(u32::MAX);
            }
        }
        1
    }

    fn peek_binop(&self) -> Option<(u8, u8, u16)> {
        if self.peek_kw("in") || self.peek_kw("instanceof") {
            return Some((7, 8, tk::NAME));
        }
        let (kind, _) = self.peek_punct()?;
        let (lbp, rbp) = match kind {
            tk::ARROW_FN
            | tk::EQUALS
            | tk::PLUSEQ
            | tk::MINUSEQ
            | tk::STAREQ
            | tk::SLASHEQ
            | tk::PERCENTEQ
            | tk::POWEREQ
            | tk::AND_EQ
            | tk::OR_EQ
            | tk::AMP_EQ
            | tk::PIPE_EQ
            | tk::CARET_EQ
            | tk::LSHIFT_EQ
            | tk::SHIFT_EQ
            | tk::USHIFT_EQ => (2, 2),
            tk::OR => (3, 4),
            tk::AND => (4, 5),
            tk::PIPE => (5, 6),
            tk::CARET => (6, 7),
            tk::AMP => (7, 8),
            tk::EQ | tk::NEQ | tk::SEQ | tk::SNEQ => (8, 9),
            tk::LT | tk::GT | tk::LE | tk::GE => (9, 10),
            tk::LSHIFT | tk::SHIFT | tk::USHIFT => (10, 11),
            tk::PLUS | tk::MINUS => (11, 12),
            tk::STAR | tk::SLASH | tk::PERCENT => (12, 13),
            tk::POWER => (14, 14), // right-associative
            _ => return None,
        };
        Some((lbp, rbp, kind))
    }

    /// The punctuation token at the cursor (longest match wins).
    fn peek_punct(&self) -> Option<(u16, u32)> {
        self.peek()?;
        let len = self.punct_len();
        let bytes = &self.bytes[self.pos as usize..(self.pos + len) as usize];
        let kind = match bytes {
            b">>>" => tk::USHIFT,
            b"===" => tk::SEQ,
            b"!==" => tk::SNEQ,
            b"**=" => tk::POWEREQ,
            b"&&=" => tk::AND_EQ,
            b"||=" => tk::OR_EQ,
            b"<<=" => tk::LSHIFT_EQ,
            b">>=" => tk::SHIFT_EQ,
            b"..." => tk::ELLIPSIS,
            b"**" => tk::POWER,
            b"==" => tk::EQ,
            b"!=" => tk::NEQ,
            b"<=" => tk::LE,
            b">=" => tk::GE,
            b"&&" => tk::AND,
            b"||" => tk::OR,
            b"<<" => tk::LSHIFT,
            b">>" => tk::SHIFT,
            b"++" => tk::INCREMENT,
            b"--" => tk::DECREMENT,
            b"+=" => tk::PLUSEQ,
            b"-=" => tk::MINUSEQ,
            b"*=" => tk::STAREQ,
            b"/=" => tk::SLASHEQ,
            b"%=" => tk::PERCENTEQ,
            b"&=" => tk::AMP_EQ,
            b"|=" => tk::PIPE_EQ,
            b"^=" => tk::CARET_EQ,
            b"=>" => tk::ARROW_FN,
            b"{" => tk::LBRACE,
            b"}" => tk::RBRACE,
            b"(" => tk::LPAREN,
            b")" => tk::RPAREN,
            b"[" => tk::LBRACKET,
            b"]" => tk::RBRACKET,
            b"<" => tk::LT,
            b">" => tk::GT,
            b"," => tk::COMMA,
            b";" => tk::SEMI,
            b":" => tk::COLON,
            b"." => tk::DOT,
            b"?" => tk::QUESTION,
            b"!" => tk::BANG,
            b"+" => tk::PLUS,
            b"-" => tk::MINUS,
            b"*" => tk::STAR,
            b"/" => tk::SLASH,
            b"%" => tk::PERCENT,
            b"&" => tk::AMP,
            b"|" => tk::PIPE,
            b"^" => tk::CARET,
            b"~" => tk::TILDE,
            b"=" => tk::EQUALS,
            _ => return None,
        };
        Some((kind, len))
    }

    /// One atom: names/keywords-as-atoms, numbers, strings, template
    /// literals, parenthesized forms (incl. detected arrows), array and
    /// object literals.
    #[allow(clippy::too_many_lines)] // one atom dispatcher
    fn parse_atom(&mut self) {
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' || b == b'$' => {
                self.bump_name();
            }
            Some(b'0'..=b'9' | b'.') => {
                let n = self.scan_number_len();
                if n > 0 {
                    self.bump(tk::NUMBER, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(b'"' | b'\'') => self.bump_string(),
            Some(b'`') => self.parse_template(),
            Some(b'(') => {
                self.bump(tk::LPAREN, 1);
                self.skip_trivia();
                if self.peek() != Some(b')') {
                    self.parse_expr_bp(0);
                }
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(b'[') => {
                let mark = self.events.len();
                self.bump(tk::LBRACKET, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    if self.starts_with(b"...") {
                        self.bump(tk::ELLIPSIS, 3);
                        self.skip_trivia();
                    }
                    self.parse_expr_bp(1);
                    self.skip_trivia();
                    if self.peek() == Some(b',') {
                        self.bump(tk::COMMA, 1);
                        continue;
                    }
                    break;
                }
                self.skip_trivia();
                if self.peek() == Some(b']') {
                    self.bump(tk::RBRACKET, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::ARRAY);
            }
            Some(b'{') => {
                let mark = self.events.len();
                self.bump(tk::LBRACE, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b'}') {
                        break;
                    }
                    if self.peek().is_none() {
                        let start = self.pos;
                        self.fail_skip(start, true);
                        break;
                    }
                    if self.starts_with(b"...") {
                        // Spread: flat inside the OBJECT node.
                        self.bump(tk::ELLIPSIS, 3);
                        self.skip_trivia();
                        self.parse_expr_bp(1);
                        self.skip_trivia();
                        if self.peek() == Some(b',') {
                            self.bump(tk::COMMA, 1);
                        }
                        continue;
                    }
                    let key_mark = self.events.len();
                    if matches!(self.peek(), Some(b'"' | b'\'')) {
                        self.bump_string();
                    } else if self.peek() == Some(b'[') {
                        // Computed key: group it as an expression.
                        self.bump(tk::LBRACKET, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(0);
                        self.skip_trivia();
                        if self.peek() == Some(b']') {
                            self.bump(tk::RBRACKET, 1);
                        } else {
                            let start = self.pos;
                            self.fail_skip(start, true);
                        }
                    } else {
                        self.bump_name();
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(1);
                    }
                    self.wrap_node(key_mark, kinds::PAIR);
                    self.skip_trivia();
                    if self.peek() == Some(b',') {
                        self.bump(tk::COMMA, 1);
                        continue;
                    }
                    break;
                }
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::OBJECT);
            }
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }

    /// Template literal: TEMPLATE node with start/middle chunks and
    /// `${expr}` interpolations (full Pratt grammar inside).
    fn parse_template(&mut self) {
        let node_mark = self.events.len();
        self.bump(tk::TEMPLATE_START, 1);
        self.events.insert(
            node_mark,
            Event::Start {
                kind: kinds::TEMPLATE,
                forward_parent: None,
            },
        );
        loop {
            // Literal chunk (escapes stay inside the middle text).
            let chunk = self.pos;
            loop {
                match self.peek() {
                    None | Some(b'`') => break,
                    Some(b'\\') => self.pos += 2,
                    Some(b'$') if self.at(1) == Some(b'{') => break,
                    Some(_) => self.pos += 1,
                }
            }
            if self.pos > chunk {
                // Rewind discipline: the token covers the scanned chunk.
                let scanned = self.pos;
                self.pos = chunk;
                self.bump(tk::TEMPLATE_MIDDLE, scanned - chunk);
            }
            // Closing backtick?
            if self.peek() == Some(b'`') {
                self.bump(tk::TEMPLATE_END, 1);
                self.events.push(Event::Finish);
                return;
            }
            // Interpolation?
            if self.peek() == Some(b'$') && self.at(1) == Some(b'{') {
                self.bump(tk::LBRACE, 2);
                self.skip_trivia();
                self.parse_expr_bp(0);
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                continue;
            }
            // Unterminated: one tolerated error.
            let start = self.pos;
            self.fail_skip(start, false);
            self.events.push(Event::Finish);
            return;
        }
    }
}

//! Hand-written reference Rust-subset parser on the runtime-core event
//! protocol (issue 033). Brace-structured with `;` separators — newlines
//! are pure trivia — with a Pratt expression engine, generic-parameter
//! and where-clause grammars, lifetime/char disambiguation in the
//! scanner, and macros as opaque balanced token trees.

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

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse Rust-subset source into the raw event stream:
/// `(events, token_ranges, error_count)`.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_module();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse Rust-subset source into a green tree (one implicit
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
    /// Struct-literal `{` allowed after a path? Cleared inside
    /// control-flow scrutinees (`match x {`, `if c {`, `for … in it {`).
    allow_struct: bool,
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
            allow_struct: true,
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

    fn peek_kw(&self, kw: &str) -> bool {
        if !self.starts_with(kw.as_bytes()) {
            return false;
        }
        match self.at(u32::try_from(kw.len()).unwrap_or(u32::MAX)) {
            None => true,
            // `r` in `r#"…"` still starts a name; keywords must not be
            // glued to other name characters.
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'_'),
        }
    }

    fn bump_kw(&mut self, kw: &str) {
        self.bump(tk::NAME, u32::try_from(kw.len()).unwrap_or(u32::MAX));
    }

    /// Whitespace + comments (`//`, `///`, `//!`, `/* … */`) are trivia;
    /// doc comments are NOT special to the grammar (v0).
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
    // Scanner: names, lifetimes vs chars, numbers, strings
    // ------------------------------------------------------------------

    fn scan_name_len(&self) -> u32 {
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
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// The `'` disambiguation (TDD): `'a'` is a char literal,
    /// `'a`/`'static` is a lifetime, `'lbl:` a label. Returns
    /// `(kind, length)` or `None` when no `'` sits here.
    fn scan_quote_form(&self) -> Option<(u16, u32)> {
        if self.peek() != Some(b'\'') {
            return None;
        }
        match self.at(1) {
            // Escape start: always a char literal, scan to the close.
            Some(b'\\') => {
                let mut idx = self.pos as usize + 2;
                while idx < self.bytes.len() {
                    if self.bytes[idx] == b'\'' {
                        return Some((tk::CHAR, u32::try_from(idx + 1 - self.pos as usize).ok()?));
                    }
                    idx += 1;
                }
                Some((tk::CHAR, 1)) // unterminated: one-byte error fodder
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                let mut idx = self.pos as usize + 1;
                while self
                    .bytes
                    .get(idx)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    idx += 1;
                }
                // `'a'`: a one-letter char literal (a closing quote).
                if self.bytes.get(idx) == Some(&b'\'') && idx == self.pos as usize + 2 {
                    return Some((tk::CHAR, 3));
                }
                // `'a` / `'static` / `'lbl:` — a lifetime/label token.
                Some((
                    tk::LIFETIME,
                    u32::try_from(idx - self.pos as usize).unwrap_or(1),
                ))
            }
            _ => Some((tk::CHAR, 1)), // lone `'`: error fodder
        }
    }

    /// String literal length with raw/byte prefixes: `"…"`, `b"…"`,
    /// `r"…"`, `r#"…"#`, `br"…"`.
    fn scan_string_len(&self, hash_count: usize) -> Option<u32> {
        let start = self.pos as usize;
        let mut i = start;
        // find the opening quote
        while i < self.bytes.len() && self.bytes[i] != b'"' {
            i += 1;
        }
        if i >= self.bytes.len() {
            return None;
        }
        i += 1;
        loop {
            match self.bytes.get(i)? {
                b'\\' if hash_count == 0 => i += 2,
                b'"' => {
                    if hash_count == 0 {
                        return u32::try_from(i + 1 - start).ok();
                    }
                    // raw string: closing `"` + `hash_count` hashes
                    let hashes = &self.bytes[i + 1..(i + 1 + hash_count).min(self.bytes.len())];
                    if hashes.len() == hash_count && hashes.iter().all(|b| *b == b'#') {
                        return u32::try_from(i + 1 + hash_count - start).ok();
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
    }

    /// Numeric literal length (suffixes included).
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
            // `1.foo` is a method call, not a float — a digit must follow.
            if self.bytes.get(idx + 1).is_some_and(u8::is_ascii_digit) {
                idx += 1;
                while idx < self.bytes.len()
                    && (self.bytes[idx].is_ascii_digit() || self.bytes[idx] == b'_')
                {
                    idx += 1;
                }
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
        // Suffixes: u8..u128, i8..i128, f32/f64.
        let suffixes: [&[u8]; 12] = [
            b"u8", b"u16", b"u32", b"u64", b"u128", b"usize", b"i8", b"i16", b"i32", b"i64",
            b"i128", b"isize",
        ];
        for suffix in suffixes {
            if self.bytes[idx..].starts_with(suffix)
                && !self
                    .bytes
                    .get(idx + suffix.len())
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                idx += suffix.len();
                break;
            }
        }
        if self.bytes[idx..].starts_with(b"f32") || self.bytes[idx..].starts_with(b"f64") {
            idx += 3;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Bump one name or handle string prefixes (`r`, `b`, `br`) glued to
    /// a quote.
    fn bump_atom_token(&mut self) {
        let n = self.scan_name_len();
        let text_start = self.pos as usize;
        if n > 0 {
            let next = self.at(n);
            let prefix = &self.bytes[text_start..text_start + n as usize];
            // Raw strings put their hashes BEFORE the quote: r#…"#.
            let raw_hashes = if matches!(prefix, b"r" | b"br" | b"rb") {
                let mut h = text_start + n as usize;
                let mut count = 0_usize;
                while self.bytes.get(h) == Some(&b'#') {
                    count += 1;
                    h += 1;
                }
                if self.bytes.get(h) == Some(&b'"') && count > 0 {
                    Some(count)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(hashes) = raw_hashes {
                if let Some(len) = self.scan_string_len(hashes) {
                    self.bump(tk::STRING, len);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                return;
            }
            let hashy = self.at(n + 1) == Some(b'#') && matches!(self.at(n), Some(b'"'));
            if matches!(next, Some(b'"')) && matches!(prefix, b"r" | b"b" | b"br" | b"rb") {
                // Raw/byte string: prefix + hashed body.
                let hashes = {
                    let mut i = text_start + n as usize;
                    let mut count = 0;
                    while self.bytes.get(i) == Some(&b'#') {
                        count += 1;
                        i += 1;
                    }
                    count
                };
                let _ = hashy;
                let count = if matches!(prefix, b"r" | b"br" | b"rb") {
                    hashes
                } else {
                    0
                };
                if let Some(len) = self.scan_string_len(count) {
                    self.bump(tk::STRING, len);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                return;
            }
            if matches!(next, Some(b'\'')) && matches!(prefix, b"b") {
                // byte char b'x'
                if let Some((_, len)) = self.scan_quote_form_at(text_start + n as usize) {
                    self.bump(tk::CHAR, n + len);
                    return;
                }
            }
        }
        if n > 0 {
            self.bump(tk::NAME, n);
        }
    }

    /// `scan_quote_form` at an arbitrary offset (for `b'x'`).
    fn scan_quote_form_at(&self, offset: usize) -> Option<(u16, u32)> {
        let saved = self.pos;
        // A small immutable re-scan: build a sub-probe.
        let sub = Parser {
            bytes: self.bytes,
            pos: u32::try_from(offset).unwrap_or(u32::MAX),
            last_end: 0,
            events: Vec::new(),
            tokens: Vec::new(),
            trivia: Vec::new(),
            errors: 0,
            allow_struct: true,
        };
        let result = sub.scan_quote_form();
        let _ = saved;
        result
    }

    // ------------------------------------------------------------------
    // Module / items
    // ------------------------------------------------------------------

    fn parse_module(&mut self) {
        while self.peek().is_some() {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }
            if self.peek() == Some(b'}') {
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

    /// Attributes then the item/statement dispatch.
    fn parse_statement(&mut self) {
        self.skip_trivia();
        // A labeled statement: `'lbl: item/expr`.
        if self.peek() == Some(b'\'') {
            if let Some((kind, len)) = self.scan_quote_form() {
                if kind == tk::LIFETIME {
                    self.bump(kind, len);
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_statement();
                        return;
                    }
                }
            }
        }
        // Attributes: `#[…]` / `#![…]` — one ATTR node each.
        while self.peek() == Some(b'#')
            && (self.at(1) == Some(b'[') || (self.at(1) == Some(b'!') && self.at(2) == Some(b'[')))
        {
            self.parse_attribute();
            self.skip_trivia();
        }
        if self.peek_kw("pub")
            || self.peek_kw("unsafe")
            || self.peek_kw("async")
            || (self.peek_kw("const")
                && (self.kw_follows("const", "fn")
                    || self.kw_follows("const", "unsafe")
                    || self.kw_follows("const", "async")))
        {
            // Modifiers: ride NAME tokens; dispatch behind them.
            self.parse_modified();
        } else if self.peek_kw("fn")
            || self.peek_kw("struct")
            || self.peek_kw("enum")
            || self.peek_kw("impl")
            || self.peek_kw("trait")
            || self.peek_kw("mod")
            || self.peek_kw("use")
            || self.peek_kw("type")
        {
            self.parse_item();
        } else if self.peek_kw("const") || self.peek_kw("static") {
            self.parse_const();
        } else if self.peek_kw("let") {
            self.parse_let();
        } else if self.peek_kw("if")
            || self.peek_kw("while")
            || self.peek_kw("for")
            || self.peek_kw("loop")
            || self.peek_kw("match")
            || self.peek_kw("return")
            || self.peek_kw("break")
            || self.peek_kw("continue")
        {
            self.parse_control_expr();
            self.terminator();
        } else {
            self.parse_expr_stmt();
        }
    }

    /// Whether `kw` is immediately followed by `next` (word-bounded).
    fn kw_follows(&self, kw: &str, next: &str) -> bool {
        let mut idx = self.pos as usize + kw.len();
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t')) {
            idx += 1;
        }
        self.starts_with_at(next.as_bytes(), idx)
    }

    fn starts_with_at(&self, text: &[u8], idx: usize) -> bool {
        self.bytes.len() >= idx + text.len() && &self.bytes[idx..idx + text.len()] == text
    }

    /// `pub`/`unsafe`/`async`/`const` modifiers then the real item.
    fn parse_modified(&mut self) {
        while self.peek_kw("pub")
            || self.peek_kw("unsafe")
            || self.peek_kw("async")
            || (self.peek_kw("const")
                && (self.kw_follows("const", "fn")
                    || self.kw_follows("const", "unsafe")
                    || self.kw_follows("const", "async")))
        {
            self.bump_kw(if self.peek_kw("pub") {
                "pub"
            } else if self.peek_kw("unsafe") {
                "unsafe"
            } else if self.peek_kw("async") {
                "async"
            } else {
                "const"
            });
            // `pub(crate)` / `pub(in …)` — a paren group rides.
            self.skip_trivia();
            if self.peek() == Some(b'(') {
                self.bump(tk::LPAREN, 1);
                self.skip_balanced_to_close(tk::RPAREN, tk::LPAREN);
            }
            self.skip_trivia();
        }
        if self.peek_kw("fn")
            || self.peek_kw("struct")
            || self.peek_kw("enum")
            || self.peek_kw("impl")
            || self.peek_kw("trait")
            || self.peek_kw("mod")
            || self.peek_kw("use")
            || self.peek_kw("type")
        {
            self.parse_item();
        } else if self.peek_kw("const") || self.peek_kw("static") {
            self.parse_const();
        } else if self.peek_kw("fn") {
            self.parse_fn();
        } else {
            // A modified expression (`unsafe { … }`): parse as an expression.
            self.parse_expr_stmt();
        }
    }

    /// Skip to and consume the matching closer of `open` (balanced).
    fn skip_balanced_to_close(&mut self, close: u16, open: u16) {
        let mut depth = 1_u32;
        while let Some(b) = self.peek() {
            match b {
                b if b == delimiter_byte(close) => {
                    self.bump(close, 1);
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                b if b == delimiter_byte(open) => {
                    self.bump(open, 1);
                    depth += 1;
                }
                b'"' | b'\'' => {
                    self.bump_string_or_char();
                }
                _ => {
                    self.bump_misc();
                }
            }
        }
    }

    fn bump_string_or_char(&mut self) {
        if let Some((kind, len)) = self.scan_quote_form() {
            self.bump(kind, len);
            return;
        }
        // A string (possibly prefixed): reuse the atom scanner.
        let before = self.events.len();
        let _ = before;
        if matches!(self.peek(), Some(b'"')) {
            if let Some(len) = self.scan_string_len(0) {
                self.bump(tk::STRING, len);
            } else {
                self.pos += 1;
            }
            return;
        }
        self.bump_misc();
    }

    /// Bump one "misc" token: the longest punct match or a single byte.
    fn bump_misc(&mut self) {
        self.skip_trivia();
        let (kind, len) = self.peek_punct().unwrap_or((tk::NAME, 1));
        // Names inside token trees: scan as identifiers.
        if kind == tk::NAME {
            let n = self.scan_name_len();
            if n > 0 {
                self.bump(tk::NAME, n);
                return;
            }
        }
        self.bump(kind, len);
    }

    fn parse_item(&mut self) {
        if self.peek_kw("fn") {
            self.parse_fn();
        } else if self.peek_kw("struct") {
            self.parse_struct();
        } else if self.peek_kw("enum") {
            self.parse_enum();
        } else if self.peek_kw("impl") {
            self.parse_impl();
        } else if self.peek_kw("trait") {
            self.parse_trait();
        } else if self.peek_kw("mod") {
            self.parse_mod();
        } else if self.peek_kw("use") {
            self.parse_use();
        } else {
            self.parse_type_alias();
        }
    }

    /// `fn name<Generic>(params) -> Ret where … { body }` or `;`.
    #[allow(clippy::too_many_lines)]
    fn parse_fn(&mut self) {
        let mark = self.events.len();
        self.events.push(Event::Start {
            kind: kinds::FN,
            forward_parent: None,
        });
        self.bump_kw("fn");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
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
        if self.peek() == Some(b'-') && self.at(1) == Some(b'>') {
            self.bump(tk::RARROW, 2);
            self.skip_trivia();
            self.parse_type();
            self.skip_trivia();
        }
        if self.peek_kw("where") {
            self.parse_where();
        }
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
        let _ = mark;
    }

    /// `(params)` — `pat: Type`, `&'a mut pat`, `...`, defaults.
    fn parse_params(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PARAMS,
            forward_parent: None,
        });
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                _ => {}
            }
            if self.starts_with(b"...") {
                self.bump(tk::DOT2EQ, 0); // placeholder never taken
                break;
            }
            self.parse_pattern();
            self.skip_trivia();
            if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_type();
                self.skip_trivia();
            }
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

    /// `<T: Bound<'a>, const N: usize>` — closed with the `>>`-safe
    /// single-`>` bump.
    fn parse_generic_params(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::GENERIC_PARAMS,
            forward_parent: None,
        });
        self.bump(tk::LT, 1);
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'>') => break,
                None => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                Some(b'\'') => {
                    if let Some((kind, len)) = self.scan_quote_form() {
                        self.bump(kind, len);
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_type_union();
                    }
                }
                _ => {
                    if self.peek_kw("const") {
                        self.bump_kw("const");
                        self.skip_trivia();
                    }
                    self.bump_name();
                    self.skip_trivia();
                    // A qualified type as a "param" (Vec<u8> in
                    // Vec<Vec<u8>>): recurse into its own arguments.
                    if self.peek() == Some(b'<') {
                        self.parse_generic_params();
                        self.skip_trivia();
                    }
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_type_union();
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b'=') {
                        self.bump(tk::ASSIGN, 1);
                        self.skip_trivia();
                        self.parse_type();
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
        self.close_generic();
    }

    /// `>` close: one byte, so nested `>>` splits naturally. Closes
    /// the `GENERIC_PARAMS` node opened by the caller.
    fn close_generic(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'>') {
            self.bump(tk::GT, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// `where T: Into<U>, 'a: 'b` — predicates split on top-level commas.
    fn parse_where(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::WHERE,
            forward_parent: None,
        });
        self.bump_kw("where");
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'{') || self.peek() == Some(b';') || self.peek().is_none() {
                break;
            }
            // One predicate: a type (or lifetime) + bounds.
            if self.peek() == Some(b'\'') {
                if let Some((kind, len)) = self.scan_quote_form() {
                    self.bump(kind, len);
                }
            } else {
                self.parse_type();
            }
            self.skip_trivia();
            if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_type_union();
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

    /// `struct` — unit `;`, tuple `(…);`, or named `{ … }`.
    fn parse_struct(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::STRUCT,
            forward_parent: None,
        });
        self.bump_kw("struct");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
        }
        self.skip_trivia();
        match self.peek() {
            Some(b'(') => {
                self.bump(tk::LPAREN, 1);
                self.parse_params();
                self.skip_trivia();
                if self.peek_kw("where") {
                    self.parse_where();
                }
                self.terminator();
            }
            Some(b'{') => {
                if self.peek_kw("where") {
                    self.parse_where();
                }
                self.skip_trivia();
                self.parse_body_members();
            }
            _ => {
                if self.peek_kw("where") {
                    self.parse_where();
                }
                self.terminator();
            }
        }
        self.events.push(Event::Finish);
    }

    /// `{ field: Type, … }` — the BODY node with MEMBER children.
    fn parse_body_members(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::BODY,
            forward_parent: None,
        });
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            self.events.push(Event::Finish);
            return;
        }
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
            // Attributes + visibility ride NAME/hash tokens.
            while self.peek() == Some(b'#') && self.at(1) == Some(b'[') {
                self.parse_attribute();
                self.skip_trivia();
            }
            if self.peek_kw("pub") {
                self.bump_kw("pub");
                self.skip_trivia();
                if self.peek() == Some(b'(') {
                    self.bump(tk::LPAREN, 1);
                    self.skip_balanced_to_close(tk::RPAREN, tk::LPAREN);
                    self.skip_trivia();
                }
            }
            self.bump_name();
            self.skip_trivia();
            // Enum variants: tuple `B(u8)` or struct `C { x: i32 }`.
            if self.peek() == Some(b'(') {
                self.bump(tk::LPAREN, 1);
                self.parse_params();
                self.skip_trivia();
            } else if self.peek() == Some(b'{') {
                self.parse_body_members();
                self.skip_trivia();
            } else if self.peek() == Some(b':') {
                self.bump(tk::COLON, 1);
                self.skip_trivia();
                self.parse_type();
            }
            self.skip_trivia();
            // Enum discriminants: `= 3`.
            if self.peek() == Some(b'=') {
                self.bump(tk::ASSIGN, 1);
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
        self.events.push(Event::Finish);
    }

    /// `enum E { Variant, Data { x: u8 } = 3, … }`.
    fn parse_enum(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ENUM,
            forward_parent: None,
        });
        self.bump_kw("enum");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
        }
        self.skip_trivia();
        if self.peek_kw("where") {
            self.parse_where();
        }
        self.skip_trivia();
        self.parse_body_members();
        self.events.push(Event::Finish);
    }

    /// `impl<T> Trait for Type where … { methods }`.
    fn parse_impl(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IMPL,
            forward_parent: None,
        });
        self.bump_kw("impl");
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
        }
        self.skip_trivia();
        // Head type (optionally `Trait for Type`).
        self.parse_type();
        self.skip_trivia();
        if self.peek_kw("for") {
            self.bump_kw("for");
            self.skip_trivia();
            self.parse_type();
            self.skip_trivia();
        }
        if self.peek_kw("where") {
            self.parse_where();
        }
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            self.parse_impl_members();
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

    /// Impl/trait bodies: associated items (fn/const/type) — the same
    /// statement dispatch, one level in.
    fn parse_impl_members(&mut self) {
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
    }

    /// `trait Name<T>: Bounds where … { … }`.
    fn parse_trait(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::TRAIT,
            forward_parent: None,
        });
        self.bump_kw("trait");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
        }
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_type_union();
        }
        self.skip_trivia();
        if self.peek_kw("where") {
            self.parse_where();
        }
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            self.parse_impl_members();
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            self.terminator();
        }
        self.events.push(Event::Finish);
    }

    fn parse_mod(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::MOD,
            forward_parent: None,
        });
        self.bump_kw("mod");
        self.skip_trivia();
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            self.parse_impl_members();
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.bump(tk::RBRACE, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        } else {
            self.terminator();
        }
        self.events.push(Event::Finish);
    }

    /// `use a::b::{c, d as e};` / `use a::*;`.
    fn parse_use(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::USE,
            forward_parent: None,
        });
        self.bump_kw("use");
        self.skip_trivia();
        self.parse_use_tree();
        self.skip_trivia();
        self.terminator();
        self.events.push(Event::Finish);
    }

    fn parse_use_tree(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::USE_TREE,
            forward_parent: None,
        });
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'*') => self.bump(tk::STAR, 1),
                Some(b'{') => {
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
                        self.parse_use_tree();
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
                _ => {
                    self.bump_name();
                    self.skip_trivia();
                    if self.peek_kw("as") {
                        self.bump_kw("as");
                        self.skip_trivia();
                        if self.peek() == Some(b'_') {
                            self.bump(tk::UNDERSCORE, 1);
                        } else {
                            self.bump_name();
                        }
                    }
                }
            }
            self.skip_trivia();
            if self.peek() == Some(b':') && self.at(1) == Some(b':') {
                self.bump(tk::COLON2, 2);
                continue;
            }
            break;
        }
        self.events.push(Event::Finish);
    }

    /// `const`/`static` items: `const NAME: T = init;`.
    fn parse_const(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CONST,
            forward_parent: None,
        });
        self.bump_kw(if self.peek_kw("static") {
            "static"
        } else {
            "const"
        });
        self.skip_trivia();
        if self.peek_kw("mut") {
            self.bump_kw("mut");
            self.skip_trivia();
        }
        if self.peek() == Some(b'_') {
            self.bump(tk::UNDERSCORE, 1);
        } else {
            self.bump_name();
        }
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_type();
        }
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(1);
        }
        self.skip_trivia();
        self.terminator();
        self.events.push(Event::Finish);
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
            self.parse_generic_params();
        }
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_type();
        }
        self.skip_trivia();
        if self.peek_kw("where") {
            self.parse_where();
        }
        self.skip_trivia();
        self.terminator();
        self.events.push(Event::Finish);
    }

    /// `#[…]` / `#![…]` — one `ATTR` node with a balanced `TOKEN_TREE`.
    fn parse_attribute(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ATTR,
            forward_parent: None,
        });
        self.bump(tk::HASH, 1);
        self.skip_trivia();
        if self.peek() == Some(b'!') {
            self.bump(tk::BANG, 1);
            self.skip_trivia();
        }
        if self.peek() == Some(b'[') {
            self.bump(tk::LBRACKET, 1);
            self.parse_token_tree(tk::RBRACKET, tk::LBRACKET);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// Balanced token-tree scan to the matching `close` — nested
    /// delimiters recurse; strings/lifetimes ride their own tokens.
    fn parse_token_tree(&mut self, close: u16, open: u16) {
        self.events.push(Event::Start {
            kind: kinds::TOKEN_TREE,
            forward_parent: None,
        });
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                Some(b) if b == delimiter_byte(close) => {
                    self.bump(close, 1);
                    break;
                }
                Some(b'[') => {
                    self.bump(tk::LBRACKET, 1);
                    self.parse_token_tree(tk::RBRACKET, tk::LBRACKET);
                }
                Some(b']') => {
                    self.bump(tk::RBRACKET, 1);
                }
                Some(b'{') => {
                    self.bump(tk::LBRACE, 1);
                    self.parse_token_tree(tk::RBRACE, tk::LBRACE);
                }
                Some(b'}') => {
                    self.bump(tk::RBRACE, 1);
                }
                Some(b'(') => {
                    self.bump(tk::LPAREN, 1);
                    self.parse_token_tree(tk::RPAREN, tk::LPAREN);
                }
                Some(b')') => {
                    self.bump(tk::RPAREN, 1);
                }
                Some(b'\'') => {
                    if let Some((kind, len)) = self.scan_quote_form() {
                        self.bump(kind, len);
                    } else {
                        self.bump_misc();
                    }
                }
                Some(b'"') => {
                    self.bump_string_or_char();
                }
                Some(b'r') if self.raw_string_ahead() => {
                    self.bump_atom_token();
                }
                _ => {
                    self.bump_misc();
                }
            }
        }
        self.events.push(Event::Finish);
        let _ = open;
    }

    fn raw_string_ahead(&self) -> bool {
        let n = self.scan_name_len();
        n == 1 && self.at(n) == Some(b'"') || (n == 2 && self.starts_with(b"r#"))
    }

    /// `let pat: T = init;`.
    fn parse_let(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::LET,
            forward_parent: None,
        });
        self.bump_kw("let");
        self.skip_trivia();
        self.parse_pattern();
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_type();
            self.skip_trivia();
        }
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(1);
        }
        self.skip_trivia();
        self.terminator();
        self.events.push(Event::Finish);
    }

    /// `;` when present (statements end at `;` or a block).
    fn terminator(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        }
    }

    /// `if` / `while` / `for` / `loop` / `match` / `return` / `break` /
    /// `continue` — all are expressions in Rust; v0 parses them at
    /// statement level with the same node shapes.
    #[allow(clippy::too_many_lines)]
    fn parse_control_expr(&mut self) {
        if self.peek_kw("if") {
            self.parse_if();
        } else if self.peek_kw("while") {
            self.events.push(Event::Start {
                kind: kinds::WHILE,
                forward_parent: None,
            });
            self.bump_kw("while");
            self.skip_trivia();
            if self.peek_kw("let") {
                self.parse_let_head();
            } else {
                self.parse_expr_no_struct(0);
            }
            self.expect_block();
            self.events.push(Event::Finish);
        } else if self.peek_kw("for") {
            self.events.push(Event::Start {
                kind: kinds::FOR,
                forward_parent: None,
            });
            self.bump_kw("for");
            self.skip_trivia();
            self.parse_pattern();
            self.skip_trivia();
            if self.peek_kw("in") {
                self.bump_kw("in");
                self.skip_trivia();
                self.parse_expr_no_struct(1);
            }
            self.expect_block();
            self.events.push(Event::Finish);
        } else if self.peek_kw("loop") {
            self.events.push(Event::Start {
                kind: kinds::LOOP,
                forward_parent: None,
            });
            self.bump_kw("loop");
            self.expect_block();
            self.events.push(Event::Finish);
        } else if self.peek_kw("match") {
            self.parse_match();
        } else if self.peek_kw("return") {
            self.events.push(Event::Start {
                kind: kinds::RETURN,
                forward_parent: None,
            });
            self.bump_kw("return");
            self.skip_trivia();
            if !self.at_stmt_end() {
                self.parse_expr_bp(1);
            }
            self.events.push(Event::Finish);
        } else {
            // break / continue
            let is_break = self.peek_kw("break");
            self.events.push(Event::Start {
                kind: if is_break {
                    kinds::BREAK
                } else {
                    kinds::CONTINUE
                },
                forward_parent: None,
            });
            self.bump_kw(if is_break { "break" } else { "continue" });
            self.skip_trivia();
            // Optional label.
            if self.peek() == Some(b'\'') {
                if let Some((kind, len)) = self.scan_quote_form() {
                    self.bump(kind, len);
                }
            }
            if is_break && !self.at_stmt_end() {
                self.bump_value();
            }
            self.events.push(Event::Finish);
        }
    }

    /// `if` / `if let` with optional `else`.
    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IF,
            forward_parent: None,
        });
        self.bump_kw("if");
        self.skip_trivia();
        if self.peek_kw("let") {
            self.parse_let_head();
        } else {
            self.parse_expr_no_struct(0);
        }
        self.expect_block();
        self.skip_trivia();
        if self.peek_kw("else") {
            self.events.push(Event::Start {
                kind: kinds::ELSE,
                forward_parent: None,
            });
            self.bump_kw("else");
            self.skip_trivia();
            if self.peek_kw("if") {
                self.parse_if();
            } else {
                self.expect_block();
            }
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    /// `let pat = expr` inside if/while heads (no trailing `;`).
    fn parse_let_head(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::LET,
            forward_parent: None,
        });
        self.bump_kw("let");
        self.skip_trivia();
        self.parse_pattern();
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(1);
        }
        self.events.push(Event::Finish);
    }

    fn expect_block(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// `match expr { arm, arm, … }`.
    fn parse_match(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::MATCH,
            forward_parent: None,
        });
        self.bump_kw("match");
        self.skip_trivia();
        self.parse_expr_no_struct(0);
        self.skip_trivia();
        if self.peek() == Some(b'{') {
            self.bump(tk::LBRACE, 1);
            loop {
                self.skip_trivia();
                match self.peek() {
                    Some(b'}') | None => break,
                    _ => {}
                }
                self.events.push(Event::Start {
                    kind: kinds::ARM,
                    forward_parent: None,
                });
                // Patterns: `p1 | p2 | p3 =>`.
                self.parse_pattern();
                self.skip_trivia();
                while self.peek() == Some(b'|') {
                    self.bump(tk::PIPE, 1);
                    self.skip_trivia();
                    self.parse_pattern();
                    self.skip_trivia();
                }
                if self.peek_kw("if") {
                    // Guard.
                    self.bump_kw("if");
                    self.skip_trivia();
                    self.parse_expr_bp(0);
                    self.skip_trivia();
                }
                if self.peek() == Some(b'=') && self.at(1) == Some(b'>') {
                    self.bump(tk::FATARROW, 2);
                    self.skip_trivia();
                    if self.peek() == Some(b'{') {
                        self.parse_block();
                    } else {
                        self.parse_expr_bp(1);
                    }
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.events.push(Event::Finish);
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                if self.peek() == Some(b'}') {
                    break;
                }
                // A block arm needs no comma; anything else errors once.
                if self.peek().is_some() && self.peek() != Some(b'}') {
                    let start = self.pos;
                    self.fail_skip(start, true);
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

    fn at_stmt_end(&self) -> bool {
        matches!(self.peek(), None | Some(b';' | b'}'))
    }

    /// `break VALUE` — a value only when the line keeps going (Rust has
    /// no restricted productions, but a `;`/`}` ends it).
    fn bump_value(&mut self) {
        self.parse_expr_bp(1);
    }

    fn parse_expr_stmt(&mut self) {
        let mark = self.events.len();
        self.skip_trivia();
        self.parse_expr_bp(0);
        self.wrap_node(mark, kinds::EXPR_STMT);
        self.terminator();
    }

    fn bump_name(&mut self) {
        let n = self.scan_name_len();
        if n > 0 {
            self.bump(tk::NAME, n);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }
}

/// The byte behind a delimiter token kind.
fn delimiter_byte(kind: u16) -> u8 {
    match kind {
        tk::RPAREN => b')',
        tk::LPAREN => b'(',
        tk::RBRACKET => b']',
        tk::LBRACKET => b'[',
        tk::RBRACE => b'}',
        _ => b'{',
    }
}

impl Parser<'_> {
    // ------------------------------------------------------------------
    // Blocks
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    /// A type: `atom (+ atom)*` — the `+` unions of traits and the
    /// `&'a mut T` references all funnel through here.
    fn parse_type(&mut self) {
        let mark = self.events.len();
        self.parse_type_atom();
        self.skip_trivia();
        // `+` unions (bounds): `A + B<'x>` — stop at `,`/`>`/`>`-ish.
        while self.peek() == Some(b'+') {
            self.bump(tk::PLUS, 1);
            self.skip_trivia();
            self.parse_type_atom();
            self.skip_trivia();
        }
        if self.events[mark..]
            .iter()
            .filter(|e| matches!(e, Event::Start { .. }))
            .count()
            > 1
        {
            self.wrap_node(mark, kinds::BINOP); // the union grouping
        }
    }

    /// The union entry point for bounds lists.
    fn parse_type_union(&mut self) {
        self.parse_type();
    }

    /// One type atom.
    fn parse_type_atom(&mut self) {
        match self.peek() {
            Some(b'&') => {
                let mark = self.events.len();
                self.bump(tk::AMP, 1);
                self.skip_trivia();
                if self.peek() == Some(b'\'') {
                    if let Some((kind, len)) = self.scan_quote_form() {
                        self.bump(kind, len);
                    }
                    self.skip_trivia();
                }
                if self.peek_kw("mut") {
                    self.bump_kw("mut");
                    self.skip_trivia();
                }
                self.parse_type_atom();
                self.wrap_node(mark, kinds::UNARY);
            }
            Some(b'*') => {
                // Raw pointer types: `*const T` / `*mut T`.
                let mark = self.events.len();
                self.bump(tk::STAR, 1);
                self.skip_trivia();
                if self.peek_kw("const") || self.peek_kw("mut") {
                    self.bump_kw(if self.peek_kw("mut") { "mut" } else { "const" });
                    self.skip_trivia();
                }
                self.parse_type_atom();
                self.wrap_node(mark, kinds::UNARY);
            }
            Some(b'[') => {
                let mark = self.events.len();
                self.bump(tk::LBRACKET, 1);
                self.skip_trivia();
                self.parse_type();
                self.skip_trivia();
                if self.peek() == Some(b';') {
                    self.bump(tk::SEMI, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
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
            Some(b'(') => {
                let mark = self.events.len();
                self.bump(tk::LPAREN, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b')') || self.peek().is_none() {
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
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::TUPLE);
            }
            Some(b'_') => {
                self.bump(tk::UNDERSCORE, 1);
            }
            Some(b) if matches!(b, b'"' | b'\'' | b'0'..=b'9') => {
                // Literal types in bounds (`'static`, `1`?) — rare; take tokens.
                if b == b'\'' {
                    if let Some((kind, len)) = self.scan_quote_form() {
                        self.bump(kind, len);
                    }
                } else {
                    let n = self.scan_number_len();
                    self.bump(tk::NUMBER, n);
                }
            }
            _ => {
                self.parse_type_path();
            }
        }
    }

    /// A type path: `Name<Args>`, `a::b::C<T>`, `dyn Trait`, `impl T`.
    fn parse_type_path(&mut self) {
        let mark = self.events.len();
        if self.peek_kw("dyn") || self.peek_kw("impl") {
            self.bump_kw(if self.peek_kw("dyn") { "dyn" } else { "impl" });
            self.skip_trivia();
        }
        self.bump_name();
        self.skip_trivia();
        if self.peek() == Some(b':') && self.at(1) == Some(b':') {
            self.bump(tk::COLON2, 2);
            self.skip_trivia();
            self.bump_name();
            self.skip_trivia();
        }
        if self.peek() == Some(b'<') {
            self.parse_generic_params();
            self.skip_trivia();
        }
        self.wrap_node(mark, kinds::FIELD); // a type path rides FIELD nodes
    }

    // ------------------------------------------------------------------
    // Patterns
    // ------------------------------------------------------------------

    /// A loose pattern grammar: literals, paths, tuples, structs, refs,
    /// slices, ranges, `_`, `mut x @ sub`.
    #[allow(clippy::too_many_lines)]
    fn parse_pattern(&mut self) {
        self.parse_pattern_bp(0);
    }

    #[allow(clippy::too_many_lines)] // one pattern grammar
    fn parse_pattern_bp(&mut self, min_bp: u8) {
        let mark = self.events.len();
        match self.peek() {
            Some(b'&') => {
                self.bump(tk::AMP, 1);
                self.skip_trivia();
                if self.peek_kw("mut") {
                    self.bump_kw("mut");
                    self.skip_trivia();
                }
                self.parse_pattern_bp(1);
                self.wrap_node(mark, kinds::UNARY);
            }
            Some(b'(') => {
                self.bump(tk::LPAREN, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b')') || self.peek().is_none() {
                        break;
                    }
                    self.parse_pattern_bp(0);
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
                self.wrap_node(mark, kinds::TUPLE);
            }
            Some(b'[') => {
                self.bump(tk::LBRACKET, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') || self.peek().is_none() {
                        break;
                    }
                    self.parse_pattern_bp(0);
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
            Some(b'_') => {
                self.bump(tk::UNDERSCORE, 1);
            }
            _ => {
                // Literal / path / binding.
                if matches!(self.peek(), Some(b'"')) {
                    if let Some(len) = self.scan_string_len(0) {
                        self.bump(tk::STRING, len);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                } else if self.peek() == Some(b'\'') {
                    if let Some((kind, len)) = self.scan_quote_form() {
                        self.bump(kind, len);
                    }
                } else if matches!(self.peek(), Some(b'0'..=b'9' | b'-')) {
                    let n = self.scan_number_len();
                    if n > 0 {
                        self.bump(tk::NUMBER, n);
                    }
                } else if self.peek() == Some(b'.') && self.at(1) == Some(b'.') {
                    // leading range
                    self.parse_range_tail(mark, None);
                    return;
                } else {
                    self.bump_name();
                    self.skip_trivia();
                    // `mut x` / `x @ pat`
                    if self.peek_kw("mut") {
                        self.bump_kw("mut");
                        self.skip_trivia();
                    }
                    // struct pattern `Name { … }`
                    if self.peek() == Some(b'{') {
                        self.bump(tk::LBRACE, 1);
                        loop {
                            self.skip_trivia();
                            if self.peek() == Some(b'}') || self.peek().is_none() {
                                break;
                            }
                            if self.starts_with(b"..") {
                                self.bump(tk::DOT2, 2);
                            } else {
                                self.bump_name();
                                self.skip_trivia();
                                if self.peek() == Some(b':') {
                                    self.bump(tk::COLON, 1);
                                    self.skip_trivia();
                                    self.parse_pattern_bp(0);
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
                        if self.peek() == Some(b'}') {
                            self.bump(tk::RBRACE, 1);
                        } else {
                            let start = self.pos;
                            self.fail_skip(start, true);
                        }
                        self.wrap_node(mark, kinds::STRUCT_LIT);
                        return;
                    }
                    // path continuation `::Variant`
                    if self.peek() == Some(b':') && self.at(1) == Some(b':') {
                        self.bump(tk::COLON2, 2);
                        self.skip_trivia();
                        self.bump_name();
                        self.skip_trivia();
                    }
                    // Tuple-struct pattern: `Some(v)` / `E::A(x)`.
                    if self.peek() == Some(b'(') {
                        self.bump(tk::LPAREN, 1);
                        loop {
                            self.skip_trivia();
                            if self.peek() == Some(b')') || self.peek().is_none() {
                                break;
                            }
                            self.parse_pattern_bp(0);
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
                        self.wrap_node(mark, kinds::TUPLE);
                        return;
                    }
                    if self.peek() == Some(b'@') {
                        self.bump(tk::AT, 1);
                        self.skip_trivia();
                        self.parse_pattern_bp(1);
                    }
                }
            }
        }
        // Range patterns: `1..=5` / `a..b` — after a literal or path.
        self.skip_trivia();
        if self.peek() == Some(b'.') && (self.at(1) == Some(b'.')) {
            self.parse_range_tail(mark, None);
            return;
        }
        if min_bp > 0 {
            return;
        }
        let _ = min_bp;
    }

    /// `..x` / `x..y` / `x..=y` tail — wraps everything since `mark`.
    fn parse_range_tail(&mut self, mark: usize, _lhs: Option<()>) {
        if self.peek() == Some(b'.') && self.at(1) == Some(b'.') {
            if self.at(2) == Some(b'=') {
                self.bump(tk::DOT2EQ, 3);
            } else {
                self.bump(tk::DOT2, 2);
            }
            self.skip_trivia();
            if !self.at_range_end() {
                self.parse_pattern_bp(1);
            }
            self.wrap_node(mark, kinds::RANGE);
        }
    }

    fn at_range_end(&self) -> bool {
        matches!(
            self.peek(),
            None | Some(b',' | b')' | b']' | b'}' | b';' | b'{')
        )
    }

    // ------------------------------------------------------------------
    // Pratt expressions
    // ------------------------------------------------------------------

    fn parse_expr_bp(&mut self, min_bp: u8) {
        let mark = self.events.len();
        if let Some((kind, n, rbp)) = self.peek_unary() {
            self.bump(kind, n);
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            self.wrap_node(mark, kinds::UNARY);
        } else if self.peek() == Some(b'|') {
            // Closure: `|…| body` / `|| body`.
            self.parse_closure();
        } else if self.peek() == Some(b'.') && self.at(1) == Some(b'.') {
            // Leading range: `..x` / `..`.
            self.parse_range_tail(mark, None);
        } else if self.peek_kw("move") {
            self.bump_kw("move");
            self.skip_trivia();
            self.parse_closure();
        } else {
            self.parse_atom(self.allow_struct);
        }
        self.skip_trivia();
        self.parse_postfix(mark);
        self.skip_trivia();
        self.parse_binops(mark, min_bp);
        self.skip_trivia();
    }

    /// The scrutinee variant: struct-literal `{` is disabled after a
    /// control-flow head expression (`match x {`, `if c {`, …) — the
    /// flag rides the parser state so every recursive call inherits it.
    fn parse_expr_no_struct(&mut self, min_bp: u8) {
        let saved = self.allow_struct;
        self.allow_struct = false;
        self.parse_expr_bp(min_bp);
        self.allow_struct = saved;
    }

    /// `|params| body` — body: a block or an expression.
    fn parse_closure(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CLOSURE,
            forward_parent: None,
        });
        self.events.push(Event::Start {
            kind: kinds::PARAMS,
            forward_parent: None,
        });
        if self.peek() == Some(b'|') && self.at(1) == Some(b'|') {
            self.bump(tk::OR, 2);
        } else {
            self.bump(tk::PIPE, 1);
            loop {
                self.skip_trivia();
                if self.peek() == Some(b'|') || self.peek().is_none() {
                    break;
                }
                self.parse_pattern();
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    self.skip_trivia();
                    self.parse_type();
                    self.skip_trivia();
                }
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.skip_trivia();
            if self.peek() == Some(b'|') {
                self.bump(tk::PIPE, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        self.events.push(Event::Finish);
        // Optional `-> Ret`.
        self.skip_trivia();
        if self.peek() == Some(b'-') && self.at(1) == Some(b'>') {
            self.bump(tk::RARROW, 2);
            self.skip_trivia();
            self.parse_type();
            self.skip_trivia();
        }
        if self.peek() == Some(b'{') {
            self.parse_block();
        } else {
            self.parse_expr_bp(1);
        }
        self.events.push(Event::Finish);
    }

    /// Prefix unary: `-`/`!`/`*`/`&`/`&mut`.
    fn peek_unary(&self) -> Option<(u16, u32, u8)> {
        match self.peek() {
            Some(b'-') if !self.starts_with(b"--") && !self.starts_with(b"-=") => {
                Some((tk::MINUS, 1, 11))
            }
            Some(b'!') if !self.starts_with(b"!=") => Some((tk::BANG, 1, 11)),
            Some(b'*') if !self.starts_with(b"*=") => Some((tk::STAR, 1, 11)),
            Some(b'&') if !self.starts_with(b"&&") && !self.starts_with(b"&=") => {
                Some((tk::AMP, 1, 11))
            }
            _ => None,
        }
    }

    /// Postfix: calls, method calls, field access, index, `?`,
    /// turbofish generics.
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
                Some(b'.') if self.at(1) != Some(b'.') => {
                    self.bump(tk::DOT, 1);
                    self.skip_trivia();
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
                        self.skip_trivia();
                        // Method call: `(` follows the name.
                        if self.peek() == Some(b'(') {
                            self.bump(tk::LPAREN, 1);
                            self.parse_call_args();
                            self.wrap_node(mark, kinds::METHOD);
                        } else {
                            self.wrap_node(mark, kinds::FIELD);
                        }
                    } else if matches!(self.peek(), Some(b'0'..=b'9')) {
                        // Tuple index `.0`.
                        let n = self.scan_number_len();
                        self.bump(tk::NUMBER, n);
                        self.wrap_node(mark, kinds::FIELD);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                }
                Some(b':') if self.at(1) == Some(b':') => {
                    // Path continuation + turbofish: `collect::<Vec<_>>()`.
                    self.bump(tk::COLON2, 2);
                    self.skip_trivia();
                    let turbofished = if self.peek() == Some(b'<') {
                        self.parse_generic_params();
                        self.skip_trivia();
                        true
                    } else {
                        false
                    };
                    // `::<T>()` calls with no trailing segment.
                    if turbofished && self.peek() == Some(b'(') {
                        self.bump(tk::LPAREN, 1);
                        self.parse_call_args();
                        self.wrap_node(mark, kinds::METHOD);
                        self.skip_trivia();
                        continue;
                    }
                    self.bump_name();
                    self.skip_trivia();
                    if self.peek() == Some(b'(') {
                        self.bump(tk::LPAREN, 1);
                        self.parse_call_args();
                        self.wrap_node(mark, kinds::METHOD);
                    } else {
                        self.wrap_node(mark, kinds::FIELD);
                    }
                }
                Some(b'?') => {
                    self.bump(tk::QUESTION, 1);
                    self.wrap_node(mark, kinds::TRY);
                }
                Some(b'!') if self.macro_ahead() => {
                    // `name!(…)` after an atom path.
                    self.parse_macro(mark);
                }
                _ => break,
            }
            self.skip_trivia();
        }
    }

    /// `name ! ( delims )` — one `MACRO` node + a balanced `TOKEN_TREE`.
    fn parse_macro(&mut self, mark: usize) {
        self.bump(tk::BANG, 1);
        self.skip_trivia();
        match self.peek() {
            Some(b'(') => {
                self.bump(tk::LPAREN, 1);
                self.parse_token_tree(tk::RPAREN, tk::LPAREN);
            }
            Some(b'[') => {
                self.bump(tk::LBRACKET, 1);
                self.parse_token_tree(tk::RBRACKET, tk::LBRACKET);
            }
            Some(b'{') => {
                self.bump(tk::LBRACE, 1);
                self.parse_token_tree(tk::RBRACE, tk::LBRACE);
            }
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        self.wrap_node(mark, kinds::MACRO);
    }

    /// Whether `!` + a delimiter sits at the cursor (a macro site).
    fn macro_ahead(&self) -> bool {
        self.peek() == Some(b'!') && matches!(self.at(1), Some(b'(' | b'[' | b'{'))
    }

    /// Call arguments: comma-separated expressions, trailing comma ok.
    fn parse_call_args(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                _ => {}
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

    /// Binary operators by binding power (Rust table), assignment and
    /// ranges right-associative.
    fn parse_binops(&mut self, mark: usize, min_bp: u8) {
        while let Some((lbp, rbp, kind)) = self.peek_binop() {
            if lbp < min_bp {
                break;
            }
            if kind == tk::NAME {
                self.bump_kw(if self.peek_kw("as") { "as" } else { "in" });
            } else {
                self.bump(kind, self.punct_len());
            }
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            let node = if Self::is_assign_op(kind) {
                kinds::ASSIGN
            } else if kind == tk::DOT2 || kind == tk::DOT2EQ {
                kinds::RANGE
            } else {
                kinds::BINOP
            };
            self.wrap_node(mark, node);
        }
    }

    fn is_assign_op(kind: u16) -> bool {
        matches!(
            kind,
            tk::ASSIGN
                | tk::PLUSEQ
                | tk::MINUSEQ
                | tk::STAREQ
                | tk::SLASHEQ
                | tk::PERCENTEQ
                | tk::CARETEQ
                | tk::AMP_EQ
                | tk::PIPE_EQ
                | tk::LSHIFT_EQ
                | tk::SHIFT_EQ
        )
    }

    fn peek_binop(&self) -> Option<(u8, u8, u16)> {
        if self.peek_kw("as") {
            return Some((8, 9, tk::NAME));
        }
        if self.peek_kw("in") {
            return Some((4, 5, tk::NAME));
        }
        let (kind, _) = self.peek_punct()?;
        let (lbp, rbp) = match kind {
            tk::OR => (3_u8, 4_u8),
            tk::AND => (4, 5),
            tk::PIPE => (5, 6),
            tk::CARET => (6, 7),
            tk::AMP => (7, 8),
            tk::EQ | tk::NEQ => (8, 9),
            tk::LT | tk::GT | tk::LE | tk::GE => (9, 10),
            tk::LSHIFT | tk::SHIFT => (10, 11),
            tk::PLUS | tk::MINUS => (11, 12),
            tk::STAR | tk::SLASH | tk::PERCENT => (12, 13),
            tk::DOT2 | tk::DOT2EQ => (2, 3), // ranges: loose, right-assoc
            tk::ASSIGN
            | tk::PLUSEQ
            | tk::MINUSEQ
            | tk::STAREQ
            | tk::SLASHEQ
            | tk::PERCENTEQ
            | tk::CARETEQ
            | tk::AMP_EQ
            | tk::PIPE_EQ
            | tk::LSHIFT_EQ
            | tk::SHIFT_EQ => (1, 1),
            _ => return None,
        };
        Some((lbp, rbp, kind))
    }

    /// The longest punct match at the cursor.
    fn punct_len(&self) -> u32 {
        const THREE: &[&[u8]] = &[b"..=", b"<<=", b">>=", b"..."];
        const TWO: &[&[u8]] = &[
            b"::", b"..", b"->", b"=>", b"==", b"!=", b"<=", b">=", b"&&", b"||", b"<<", b">>",
            b"+=", b"-=", b"*=", b"/=", b"%=", b"^=", b"&=", b"|=",
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

    fn peek_punct(&self) -> Option<(u16, u32)> {
        self.peek()?;
        let len = self.punct_len();
        let end = usize::try_from(self.pos).unwrap_or(0) + len as usize;
        let bytes = self
            .bytes
            .get(usize::try_from(self.pos).unwrap_or(0)..end)?;
        let kind = match bytes {
            b"..=" => tk::DOT2EQ,
            b"<<" => tk::LSHIFT,
            b">>" => tk::SHIFT,
            b"::" => tk::COLON2,
            b".." => tk::DOT2,
            b"->" => tk::RARROW,
            b"=>" => tk::FATARROW,
            b"==" => tk::EQ,
            b"!=" => tk::NEQ,
            b"<=" => tk::LE,
            b">=" => tk::GE,
            b"&&" => tk::AND,
            b"||" => tk::OR,
            b"+=" => tk::PLUSEQ,
            b"-=" => tk::MINUSEQ,
            b"*=" => tk::STAREQ,
            b"/=" => tk::SLASHEQ,
            b"%=" => tk::PERCENTEQ,
            b"^=" => tk::CARETEQ,
            b"&=" => tk::AMP_EQ,
            b"|=" => tk::PIPE_EQ,
            b"<<=" => tk::LSHIFT_EQ,
            b">>=" => tk::SHIFT_EQ,
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
            b"#" => tk::HASH,
            b"!" => tk::BANG,
            b"&" => tk::AMP,
            b"|" => tk::PIPE,
            b"+" => tk::PLUS,
            b"-" => tk::MINUS,
            b"*" => tk::STAR,
            b"/" => tk::SLASH,
            b"%" => tk::PERCENT,
            b"^" => tk::CARET,
            b"@" => tk::AT,
            b"_" => tk::UNDERSCORE,
            b"$" => tk::DOLLAR,
            b"?" => tk::QUESTION,
            b"=" => tk::ASSIGN,
            _ => return None,
        };
        Some((kind, len))
    }

    /// One expression atom: literals, paths (+ generic heuristic),
    /// parens/tuples, arrays, struct literals, blocks-as-exprs,
    /// control expressions, macros.
    #[allow(clippy::too_many_lines)]
    fn parse_atom(&mut self, _allow_struct: bool) {
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                if self.peek_kw("if")
                    || self.peek_kw("match")
                    || self.peek_kw("loop")
                    || self.peek_kw("while")
                    || self.peek_kw("for")
                {
                    self.parse_control_expr();
                    return;
                }
                if self.peek_kw("return") || self.peek_kw("break") || self.peek_kw("continue") {
                    self.parse_control_expr();
                    return;
                }
                let before = self.pos;
                self.bump_atom_token();
                // String/number atoms are done.
                if self.pos == before {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    return;
                }
                if self
                    .tokens
                    .last()
                    .is_some_and(|(k, _)| matches!(*k, tk::STRING | tk::NUMBER | tk::CHAR))
                {
                    return;
                }
                self.parse_path_and_generics(self.allow_struct);
            }
            Some(b'"') => {
                if let Some(len) = self.scan_string_len(0) {
                    self.bump(tk::STRING, len);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(b'\'') => {
                if let Some((kind, len)) = self.scan_quote_form() {
                    self.bump(kind, len);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
            Some(b'0'..=b'9') => {
                let n = self.scan_number_len();
                self.bump(tk::NUMBER, n);
            }
            Some(b'(') => {
                let mark = self.events.len();
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
                self.wrap_node(mark, kinds::TUPLE);
            }
            Some(b'[') => {
                let mark = self.events.len();
                self.bump(tk::LBRACKET, 1);
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') || self.peek().is_none() {
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
                if self.peek() == Some(b']') {
                    self.bump(tk::RBRACKET, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::ARRAY);
            }
            Some(b'{') => {
                self.parse_block();
            }
            Some(b'|') => {
                // Closure at atom position.
                self.parse_closure();
            }
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }

    /// Path continuation (`::Seg`) + the generic-args heuristic + the
    /// struct-literal `{ … }`.
    fn parse_path_and_generics(&mut self, allow_struct: bool) {
        loop {
            self.skip_trivia();
            if self.peek() == Some(b':') && self.at(1) == Some(b':') {
                // Turbofish `::<T>` or a path segment.
                self.bump(tk::COLON2, 2);
                self.skip_trivia();
                if self.peek() == Some(b'<') {
                    self.parse_generic_params();
                    self.skip_trivia();
                }
                self.bump_name();
                self.skip_trivia();
                continue;
            }
            break;
        }
        // The heuristic: a `<` glued to the name opens generic args.
        if self.peek() == Some(b'<') && self.glued_lt() && self.type_ahead() {
            self.parse_generic_params();
            self.skip_trivia();
        }
        // Struct literal: a `{` right after a path (disabled at
        // control-flow scrutinee positions).
        self.skip_trivia();
        if allow_struct && self.peek() == Some(b'{') {
            self.parse_struct_lit_body();
        }
    }

    /// Whether the `<` is glued to the previous name byte.
    fn glued_lt(&self) -> bool {
        self.peek() == Some(b'<')
            && self
                .bytes
                .get(self.pos as usize - 1)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    }

    /// A cheap type-start probe after `<`: a name, `&`, `[`, `(`, `-`,
    /// `'`, or `>`-close.
    fn type_ahead(&self) -> bool {
        match self.at(1) {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => true,
            Some(b'&' | b'[' | b'(' | b'-' | b'\'') => true,
            _ => false,
        }
    }

    /// `Path { field: value, … }` — the `STRUCT_LIT` body.
    fn parse_struct_lit_body(&mut self) {
        let mark = self.events.len();
        self.bump(tk::LBRACE, 1);
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'}') | None => break,
                _ => {}
            }
            self.events.push(Event::Start {
                kind: kinds::PAIR,
                forward_parent: None,
            });
            if self.starts_with(b"..") {
                self.bump(tk::DOT2, 2);
            } else {
                let n = self.scan_name_len();
                if n > 0 {
                    self.bump(tk::NAME, n);
                }
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
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
        if self.peek() == Some(b'}') {
            self.bump(tk::RBRACE, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.wrap_node(mark, kinds::STRUCT_LIT);
    }
}

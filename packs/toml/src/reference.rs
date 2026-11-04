//! Hand-written reference TOML parser on the runtime-core event protocol
//! (issue 025). The ceiling the codegen chases for the TOML pack: a clean
//! recursive-descent parser emitting 8-byte events, byte-faithful with
//! deterministic recovery (skip to the next line / closer, always
//! progress).

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

/// Parse TOML source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the JSON pack, issue 019):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; whitespace and comments are *never* token events.
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)`.
/// - **Errors**: `Error` + an ERROR node around the offending region;
///   recovery skips to the next line start / `]` / `}` / EOF and keeps
///   parsing — deterministic and lossless.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse TOML source into a green tree (one implicit [`crate::ROOT_KIND`]
/// root; sibling top-level entries from recovery live inside it).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let mut parser = Parser::run(source);
    parser.parse_document();
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

/// Recursive-descent driver over the source bytes.
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

    /// Skip whitespace and comments, recording them as trivia. `trailing`
    /// = the run follows a token on the same line (v0 heuristic: any
    /// post-token run before a newline).
    fn skip_trivia(&mut self, trailing: bool) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') => {
                    if self.pos > start && kind != tk::COMMENT {
                        self.push_trivia(start, tk::WHITESPACE, trailing);
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
                _ => break,
            }
        }
        if self.pos > start {
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

    /// Error + ERROR node around the skipped region. Sync: stops before
    /// `]`, `}`, `,`, a comment or a newline (line-structured grammar);
    /// consuming one stray char guarantees progress.
    fn fail_skip(&mut self, start: u32, consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if matches!(b, b']' | b'}' | b',' | b'\n' | b'#') {
                break;
            }
            self.pos += 1;
        }
        if consume_stray && self.pos == start && self.peek().is_some() {
            self.pos += 1; // the stray char rides the error region
        }
        if self.pos > start {
            self.events.push(Event::Token {
                kind: ERROR_KIND,
                n_raw: 1,
            });
            self.tokens.push((ERROR_KIND, start..self.pos));
        }
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Length scanners (None = malformed at this position)
    // ------------------------------------------------------------------

    /// Basic string `"…"` with `\` escapes; unterminated → `None`.
    fn scan_basic_string_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'\n' => return None, // basic strings are single-line
                b'"' => return u32::try_from(idx + 1 - start).ok(),
                b'\\' => idx += 2,
                _ => idx += 1,
            }
        }
        None
    }

    /// Literal string `'…'`; unterminated → `None`.
    fn scan_literal_string_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'\n' => return None,
                b'\'' => return u32::try_from(idx + 1 - start).ok(),
                _ => idx += 1,
            }
        }
        None
    }

    /// Multi-line basic string `"""…"""` (spans lines, `\` escapes).
    fn scan_ml_basic_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 3;
        loop {
            if idx >= self.bytes.len() {
                return None;
            }
            if self.bytes[idx..].starts_with(b"\"\"\"") {
                return u32::try_from(idx + 3 - start).ok();
            }
            idx += if self.bytes[idx] == b'\\' { 2 } else { 1 };
        }
    }

    /// Multi-line literal string `'''…'''`.
    fn scan_ml_literal_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 3;
        loop {
            if idx >= self.bytes.len() {
                return None;
            }
            if self.bytes[idx..].starts_with(b"'''") {
                return u32::try_from(idx + 3 - start).ok();
            }
            idx += 1;
        }
    }

    /// Numeric literal length (int/float body; inf/nan handled by caller).
    /// Covers `0x`/`0o`/`0b` digits, decimal digits, `_`, `.`, `e`/`E`
    /// exponents with signs.
    fn scan_number_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if matches!(self.bytes.get(idx), Some(b'+' | b'-')) {
            idx += 1;
        }
        if idx + 1 < self.bytes.len() && self.bytes[idx] == b'0' {
            match self.bytes[idx + 1] {
                b'x' | b'o' | b'b' => {
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
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Date-time length: offset/local date-time, local date, local time.
    /// Scans the RFC 3339 charset plus one space between date and time.
    fn scan_datetime_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        let mut digits = 0;
        while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
            idx += 1;
            digits += 1;
        }
        // Local time HH:MM:SS…
        if digits >= 2 && self.bytes.get(idx) == Some(&b':') {
            idx += 1;
            while idx < self.bytes.len()
                && (self.bytes[idx].is_ascii_digit()
                    || matches!(self.bytes[idx], b':' | b'.' | b'+' | b'-' | b'Z' | b'z'))
            {
                idx += 1;
            }
            return u32::try_from(idx - start).unwrap_or(1);
        }
        // Date YYYY-MM-DD (optionally followed by time).
        while idx < self.bytes.len()
            && (self.bytes[idx].is_ascii_digit() || matches!(self.bytes[idx], b'-'))
        {
            idx += 1;
        }
        if idx < self.bytes.len()
            && matches!(self.bytes[idx], b'T' | b't' | b' ')
            && self.bytes.get(idx + 1).is_some_and(u8::is_ascii_digit)
        {
            let sep = self.bytes[idx];
            idx += 1;
            while idx < self.bytes.len()
                && (self.bytes[idx].is_ascii_digit()
                    || matches!(self.bytes[idx], b':' | b'.' | b'+' | b'-' | b'Z' | b'z'))
            {
                idx += 1;
            }
            let _ = sep;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Bare key segment length (`[A-Za-z0-9_-]+`).
    fn scan_bare_key_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        while idx < self.bytes.len()
            && (self.bytes[idx].is_ascii_alphanumeric() || matches!(self.bytes[idx], b'_' | b'-'))
        {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Grammar
    // ------------------------------------------------------------------

    fn parse_document(&mut self) {
        loop {
            let before = self.pos;
            self.skip_trivia(false);
            let Some(b) = self.peek() else {
                break;
            };
            match b {
                b'[' => self.parse_header(),
                _ => self.parse_key_value(),
            }
            // Global progress guard: any iteration that consumed no bytes
            // (a recovery path that stopped on a sync char) force-feeds
            // one byte into an error region — the loop cannot spin.
            if self.pos == before {
                let start = self.pos;
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
        }
    }

    /// `[table]` or `[[array of tables]]` header.
    fn parse_header(&mut self) {
        let array = self.at(1) == Some(b'[');
        if array {
            self.bump(tk::LDOUBLE, 2);
        } else {
            self.bump(tk::LBRACKET, 1);
        }
        let node = if array {
            kinds::ARRAY_TABLE
        } else {
            kinds::TABLE
        };
        self.events.push(Event::Start {
            kind: node,
            forward_parent: None,
        });
        self.skip_trivia(false);
        if !self.parse_key_path() {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia(false);
        match (self.peek(), array) {
            (Some(b']'), false) => self.bump(tk::RBRACKET, 1),
            (Some(b']'), true) if self.at(1) == Some(b']') => self.bump(tk::RDOUBLE, 2),
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
        self.events.push(Event::Finish);
    }

    /// `key = value` at document level (also inline-table entries via
    /// [`Self::parse_inline_entry`]).
    fn parse_key_value(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::KEY_VALUE,
            forward_parent: None,
        });
        self.skip_trivia(false);
        let key_ok = self.parse_key_path();
        self.skip_trivia(false);
        let eq_ok = self.peek() == Some(b'=');
        if eq_ok {
            self.bump(tk::EQUALS, 1);
        }
        self.skip_trivia(false);
        let value_ok = if key_ok && eq_ok {
            self.parse_value()
        } else {
            false
        };
        if !(key_ok && eq_ok && value_ok) {
            let start = self.pos;
            self.fail_skip(start, !value_ok && key_ok && eq_ok);
        }
        self.events.push(Event::Finish);
    }

    /// A dotted key path: parts (bare or quoted) joined by `.`. Wrapped
    /// in a [`kinds::KEY`] node.
    fn parse_key_path(&mut self) -> bool {
        self.events.push(Event::Start {
            kind: kinds::KEY,
            forward_parent: None,
        });
        let mut ok = self.parse_key_part();
        while ok {
            self.skip_trivia(false);
            if self.peek() == Some(b'.') {
                self.bump(tk::DOT, 1);
                self.skip_trivia(false);
                ok = self.parse_key_part();
            } else {
                break;
            }
        }
        self.events.push(Event::Finish);
        ok
    }

    /// One key part: bare, basic-quoted or literal-quoted.
    fn parse_key_part(&mut self) -> bool {
        match self.peek() {
            Some(b'"') => {
                let Some(n) = self.scan_basic_string_len() else {
                    return false;
                };
                self.bump(tk::STRING, n);
                true
            }
            Some(b'\'') => {
                let Some(n) = self.scan_literal_string_len() else {
                    return false;
                };
                self.bump(tk::STRING, n);
                true
            }
            Some(_) => {
                let n = self.scan_bare_key_len();
                if n == 0 {
                    return false;
                }
                self.bump(tk::BARE_KEY, n);
                true
            }
            None => false,
        }
    }

    /// Value dispatch: string family, number family, booleans,
    /// date-times, arrays, inline tables.
    fn parse_value(&mut self) -> bool {
        match self.peek() {
            Some(b'"') => {
                let n = if self.starts_with(b"\"\"\"") {
                    self.scan_ml_basic_len()
                } else {
                    self.scan_basic_string_len()
                };
                match n {
                    Some(len) => {
                        self.bump(tk::STRING, len);
                        true
                    }
                    None => false,
                }
            }
            Some(b'\'') => {
                let n = if self.starts_with(b"'''") {
                    self.scan_ml_literal_len()
                } else {
                    self.scan_literal_string_len()
                };
                match n {
                    Some(len) => {
                        self.bump(tk::STRING, len);
                        true
                    }
                    None => false,
                }
            }
            Some(b'[') => {
                self.parse_array();
                true
            }
            Some(b'{') => {
                self.parse_inline_table();
                true
            }
            Some(b't') if self.starts_with(b"true") => {
                self.bump(tk::BOOLEAN, 4);
                true
            }
            Some(b'f') if self.starts_with(b"false") => {
                self.bump(tk::BOOLEAN, 5);
                true
            }
            Some(b'+') if self.starts_with(b"+inf") || self.starts_with(b"+nan") => {
                self.bump(tk::FLOAT, 4);
                true
            }
            Some(b'-') if self.starts_with(b"-inf") || self.starts_with(b"-nan") => {
                self.bump(tk::FLOAT, 4);
                true
            }
            Some(b'-' | b'+') => {
                // Signed numeric (inf/nan handled above).
                let n = self.scan_number_len();
                if n <= 1 {
                    return false; // sign alone — no digits
                }
                let text = &self.bytes[self.pos as usize..(self.pos + n) as usize];
                let float = text.contains(&b'.') || text.contains(&b'e') || text.contains(&b'E');
                self.bump(if float { tk::FLOAT } else { tk::INTEGER }, n);
                true
            }
            Some(b'i') if self.starts_with(b"inf") => {
                self.bump(tk::FLOAT, 3);
                true
            }
            Some(b'n') if self.starts_with(b"nan") => {
                self.bump(tk::FLOAT, 3);
                true
            }
            Some(b'0'..=b'9') => {
                let n = self.scan_datetime_len();
                if n >= 8 {
                    // A date (YYYY-MM-DD ≥ 10 chars) or time (HH:MM:SS = 8).
                    self.bump(tk::DATETIME, n);
                    return true;
                }
                let n = self.scan_number_len();
                let text = &self.bytes[self.pos as usize..(self.pos + n) as usize];
                let float = text.contains(&b'.') || text.contains(&b'e') || text.contains(&b'E');
                self.bump(if float { tk::FLOAT } else { tk::INTEGER }, n);
                true
            }
            _ => false,
        }
    }

    /// `[ value, … ]` — newlines and comments are trivia, trailing comma
    /// tolerated (recovery keeps parsing after the closing bracket).
    fn parse_array(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ARRAY,
            forward_parent: None,
        });
        self.bump(tk::LBRACKET, 1);
        loop {
            self.skip_trivia(false);
            match self.peek() {
                Some(b']') => {
                    self.bump(tk::RBRACKET, 1);
                    break;
                }
                None => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                _ => {}
            }
            if !self.parse_value() {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia(false);
            match self.peek() {
                Some(b',') => {
                    self.bump(tk::COMMA, 1);
                }
                Some(b']') => {
                    self.bump(tk::RBRACKET, 1);
                    break;
                }
                None => {
                    // Unterminated array: one error at EOF, then stop.
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                _ => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    self.skip_trivia(false);
                    if self.peek() == Some(b']') {
                        self.bump(tk::RBRACKET, 1);
                        break;
                    }
                }
            }
        }
        self.events.push(Event::Finish);
    }

    /// `{ k = v, … }` — single line; trailing comma tolerated.
    fn parse_inline_table(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::INLINE_TABLE,
            forward_parent: None,
        });
        self.bump(tk::LBRACE, 1);
        loop {
            self.skip_trivia(false);
            match self.peek() {
                Some(b'}') => {
                    self.bump(tk::RBRACE, 1);
                    break;
                }
                None => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                _ => {}
            }
            // Inline entry: key = value (no nested key-value node — the
            // entry IS the pair).
            self.events.push(Event::Start {
                kind: kinds::KEY_VALUE,
                forward_parent: None,
            });
            let key_ok = self.parse_key_path();
            self.skip_trivia(false);
            let eq_ok = self.peek() == Some(b'=');
            if eq_ok {
                self.bump(tk::EQUALS, 1);
            }
            self.skip_trivia(false);
            let value_ok = if key_ok && eq_ok {
                self.parse_value()
            } else {
                false
            };
            if !(key_ok && eq_ok && value_ok) {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.events.push(Event::Finish);
            self.skip_trivia(false);
            match self.peek() {
                Some(b',') => {
                    self.bump(tk::COMMA, 1);
                }
                Some(b'}') => {
                    self.bump(tk::RBRACE, 1);
                    break;
                }
                _ => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    self.skip_trivia(false);
                    if self.peek() == Some(b'}') {
                        self.bump(tk::RBRACE, 1);
                        break;
                    }
                }
            }
        }
        self.events.push(Event::Finish);
    }
}

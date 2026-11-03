//! Hand-written reference JSON parser on the runtime-core event protocol.
//!
//! This is the ceiling the codegen chases (roadmap §12.4): a clean
//! recursive-descent JSON parser that emits 8-byte events and stays
//! byte-faithful.
//!
//! Kind numbering (issue 019, documented mapping):
//! - node kinds: [`crate::kinds`] (`OBJECT`=0, `ARRAY`=1, `PAIR`=2),
//! - leaf value tokens reuse the same numbering (`string`=3, `number`=4,
//!   `true`=5, `false`=6, `null`=7),
//! - structural punctuation tokens: [`token_kinds`] (dense continuation),
//! - offending regions: [`runtime_core::ERROR_KIND`] nodes and tokens.
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace is never a token event. The parser emits CLEAN token ranges
//! and [`TriviaRecord`]s covering every gap; the tree builder interleaves
//! the record leaves, keeping `render(parse(x)) == x`.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::ERROR_KIND;
use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

use crate::kinds;

/// Token kinds for structural punctuation — dense continuation of
/// [`crate::kinds`], mirroring `tokens.kdl` declaration order.
pub mod token_kinds {
    /// `{` — object open.
    pub const LBRACE: u16 = 8;
    /// `}` — object close.
    pub const RBRACE: u16 = 9;
    /// `[` — array open.
    pub const LBRACK: u16 = 10;
    /// `]` — array close.
    pub const RBRACK: u16 = 11;
    /// `:` — pair separator.
    pub const COLON: u16 = 12;
    /// `,` — list separator.
    pub const COMMA: u16 = 13;
    /// Whitespace trivia: never a token event (ADR-008) — flows through
    /// [`TriviaRecord`]s at tree-build time.
    pub const WHITESPACE: u16 = 14;
}

/// Implicit document-root kind for the green tree — pack-local sentinel.
///
/// The event stream has no root (recovery can leave sibling top-level
/// entries and scalar documents are bare tokens), so the tree integration
/// wraps every document in exactly one root node. Numbered just below the
/// runtime-core sentinels ([`runtime_core::MISSING_KIND`] = 65533) so the
/// pack's dense `0..=14` ids and the sentinels stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 3;

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

/// Parse JSON source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (issue 019):
/// - **Events**: [`Event::Start`] / [`Event::Finish`] bracket nodes
///   (object/array/pair plus recovery nodes); [`Event::Token`] carries leaf
///   tokens (`n_raw` is always 1 — the reference parser lexes raw tokens
///   itself). Whitespace is *never* a token event.
/// - **Ranges**: one `(kind, byte range)` pair per token event, parallel in
///   DFS order (directly consumable by [`runtime_core::sink::drive_sink`]).
///   Ranges are CLEAN (trivia excluded) and may have gaps — the trivia
///   between them is exactly what [`Parser::trivia`] covers, so the union
///   of ranges + trivia records partitions `[0, len)`.
/// - **Errors**: one [`Event::Error`] per syntax error; `diag` indexes the
///   side table ([`DIAG_SYNTAX`] = "syntax error" via [`error_message`]).
///   Every error is immediately followed by `Start(ERROR_KIND)`, a token
///   covering the offending region (omitted only at EOF), and `Finish`.
///   Recovery skips to the next sync point (`,`, `]`, `}` or EOF) and keeps
///   parsing — deterministic and still lossless.
/// - **Shape**: there is no implicit root node; error recovery can leave
///   sibling top-level entries. The [`parse`] tree integration wraps them
///   when building the green tree.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse JSON source into a green tree.
///
/// Tree integration (issues 019/021): `parse_events` →
/// [`events::process`] → [`drive_sink`] into a source-attached
/// [`TreeBuilder`] → `finish()`. The stream has no implicit root, so the
/// tree wraps all top-level entries in one implicit [`ROOT_KIND`] node.
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *JSON* is recovered, not an error.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let mut parser = Parser::run(source);
    parser.parse_document();
    let mut wrapped = Vec::with_capacity(parser.events.len() + 2);
    wrapped.push(Event::Start {
        kind: ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(parser.events.iter().copied());
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped)?;
    let input = Source::from_str_unchecked(source);
    // NodeCache is !Sync by design (single-threaded parser core) — the Arc
    // satisfies the builder's shared-ownership signature, never a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &input);
    let records: Vec<TriviaRecord> = parser
        .trivia
        .iter()
        .map(|(range, trailing)| TriviaRecord {
            kind: token_kinds::WHITESPACE,
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
    /// End of the last emitted clean token (ranges are clean; gaps carry
    /// trivia).
    last_end: u32,
    events: Vec<Event>,
    /// Clean token ranges parallel to token events.
    tokens: Vec<(u16, Range<u32>)>,
    /// `(range, trailing)` trivia records covering every gap.
    trivia: Vec<(Range<u32>, bool)>,
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

    /// Skip whitespace, recording it as trivia. `trailing` = the trivia
    /// follows a token on the same line (v0 heuristic: any post-token run
    /// before a newline is trailing; the flag only shapes attachment).
    fn skip_ws(&mut self, trailing: bool) {
        let start = self.pos;
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
        if self.pos > start {
            self.trivia.push((start..self.pos, trailing));
        }
    }

    /// Emit a token event + clean range `[self.pos .. self.pos + n)`.
    fn bump(&mut self, kind: u16, n: u32) {
        let start = self.pos;
        self.pos += n;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, start..self.pos));
        self.last_end = self.pos;
    }

    /// Emit an error diagnostic + an ERROR node, then skip the offending
    /// region: consume bytes up to (not including) the next `]`/`}`/EOF —
    /// a `,` is consumed (element separators belong to the garbage) — and
    /// ride the skipped bytes on the ERROR token so clean ranges + trivia
    /// + error regions keep covering the whole source (lossless).
    ///
    /// When the skip consumed nothing and a byte remains (a stray closer
    /// sitting right at the failure point), that byte is consumed into the
    /// error region — this is what keeps every recovery loop strictly
    /// byte-progressing (an error node alone cannot be the loop's only
    /// progress, or a container's garbage arm would spin on a closer it
    /// has no arm for).
    ///
    /// Guarantees progress: at least one byte consumed per failure (or
    /// EOF reached).
    fn fail_skip(&mut self, start: u32, consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if matches!(b, b']' | b'}') {
                break;
            }
            self.pos += 1;
            if b == b',' {
                break;
            }
        }
        if consume_stray && self.pos == start && self.peek().is_some() {
            self.pos += 1; // the stray closer rides the error region
        }
        if self.pos > start {
            let kind = ERROR_KIND;
            self.events.push(Event::Token { kind, n_raw: 1 });
            self.tokens.push((kind, start..self.pos));
        }
        self.events.push(Event::Finish);
    }

    /// A value token: string / number / true / false / null, lexed from
    /// `self.pos`. Returns the token kind and length, or `None` when the
    /// bytes do not start any value.
    fn scan_value_token(&self) -> Option<(u16, u32)> {
        match self.peek()? {
            b'"' => Some((kinds::STRING, self.scan_string_len()?)),
            b't' if self.bytes[self.pos as usize..].starts_with(b"true") => Some((kinds::TRUE, 4)),
            b'f' if self.bytes[self.pos as usize..].starts_with(b"false") => {
                Some((kinds::FALSE, 5))
            }
            b'n' if self.bytes[self.pos as usize..].starts_with(b"null") => Some((kinds::NULL, 4)),
            b'-' | b'0'..=b'9' => Some((kinds::NUMBER, self.scan_number_len())),
            _ => None,
        }
    }

    /// Length of a JSON string starting at `self.pos` (opening quote
    /// included). `None` on an unterminated string.
    fn scan_string_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1; // past the opening quote
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'"' => return u32::try_from(idx + 1 - start).ok(),
                b'\\' => idx += 2, // skip the escape pair
                _ => idx += 1,
            }
        }
        None
    }

    /// Length of a JSON number starting at `self.pos` (longest match over
    /// `-? digits .? digits eE?`).
    fn scan_number_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if self.bytes.get(idx) == Some(&b'-') {
            idx += 1;
        }
        while matches!(self.bytes.get(idx), Some(b'0'..=b'9')) {
            idx += 1;
        }
        if self.bytes.get(idx) == Some(&b'.') {
            idx += 1;
            while matches!(self.bytes.get(idx), Some(b'0'..=b'9')) {
                idx += 1;
            }
        }
        if matches!(self.bytes.get(idx), Some(b'e' | b'E')) {
            idx += 1;
            if matches!(self.bytes.get(idx), Some(b'+' | b'-')) {
                idx += 1;
            }
            while matches!(self.bytes.get(idx), Some(b'0'..=b'9')) {
                idx += 1;
            }
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Parse one value (object/array/string/number/true/false/null).
    fn parse_value(&mut self) {
        self.skip_ws(false);
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            _ => {
                if let Some((kind, n)) = self.scan_value_token() {
                    self.bump(kind, n);
                } else {
                    // No value here: error node covering the garbage up to (not
                    // including) the enclosing closer, then keep parsing.
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
        }
    }

    /// `pair : STRING COLON value`.
    fn parse_pair(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PAIR,
            forward_parent: None,
        });
        self.parse_value(); // the key (a STRING value token)
        self.skip_ws(false);
        if self.peek() == Some(b':') {
            self.bump(token_kinds::COLON, 1);
        } else {
            self.fail_skip(self.pos, true);
        }
        self.parse_value();
        self.events.push(Event::Finish);
    }

    /// Parse a `{ ... }` object.
    fn parse_object(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::OBJECT,
            forward_parent: None,
        });
        self.bump(token_kinds::LBRACE, 1);
        loop {
            self.skip_ws(self.last_end > 0);
            match self.peek() {
                Some(b'}') => {
                    self.bump(token_kinds::RBRACE, 1);
                    break;
                }
                Some(b',') => {
                    self.bump(token_kinds::COMMA, 1);
                }
                Some(b'"') => self.parse_pair(),
                Some(_) => {
                    // Garbage inside an object: error + resync (closers are
                    // left for the loop's `}` arm).
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                None => {
                    self.fail_skip(self.pos, true);
                    break;
                }
            }
        }
        self.events.push(Event::Finish);
    }

    /// Parse a `[ ... ]` array.
    fn parse_array(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ARRAY,
            forward_parent: None,
        });
        self.bump(token_kinds::LBRACK, 1);
        loop {
            self.skip_ws(self.last_end > 0);
            match self.peek() {
                Some(b']') => {
                    self.bump(token_kinds::RBRACK, 1);
                    break;
                }
                Some(b',') => {
                    self.bump(token_kinds::COMMA, 1);
                }
                Some(_) => self.parse_value(),
                None => {
                    self.fail_skip(self.pos, true);
                    break;
                }
            }
        }
        self.events.push(Event::Finish);
    }

    /// Top level: values separated by whitespace until EOF; non-value
    /// garbage becomes an error + resync; trailing content after a complete
    /// document yields sibling roots.
    fn parse_document(&mut self) {
        loop {
            self.skip_ws(false);
            if self.peek().is_none() {
                break;
            }
            match self.peek() {
                Some(b'{') => self.parse_object(),
                Some(b'[') => self.parse_array(),
                Some(_) if self.scan_value_token().is_some() => self.parse_value(),
                _ => {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
        }
    }
}

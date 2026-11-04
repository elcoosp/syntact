//! Hand-written reference CSV parser on the runtime-core event protocol
//! (issue 040). Row-structured recursive descent emitting 8-byte events,
//! byte-faithful with deterministic recovery: an unterminated quoted
//! field becomes one `ERROR_KIND` region that consumes to EOF (a quoted
//! field may span lines, so no earlier sync point exists), and the
//! parser always makes progress.

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

/// Parse CSV source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the other packs):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; stray whitespace between tokens is never a token
///   event (newlines ARE tokens — they terminate rows).
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)`.
/// - **Errors**: `Error` + an `ERROR_KIND` node around the offending
///   region; recovery consumes to EOF (unterminated quotes may span
///   lines) and keeps parsing — deterministic and lossless.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse CSV source into a green tree (the full integration).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *CSV* is recovered, not an error.
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
            events: Vec::new(),
            tokens: Vec::new(),
            trivia: Vec::new(),
            errors: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos as usize).copied()
    }

    /// Skip stray spaces/tabs between tokens (newlines are tokens; a
    /// `\r` only ever precedes a `\n`, where it belongs to the
    /// terminator token).
    fn skip_trivia(&mut self, trailing: bool) {
        let start = self.pos;
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
        if self.pos > start {
            self.trivia
                .push((start..self.pos, tk::WHITESPACE, trailing));
        }
    }

    /// Emit a token event + clean range `[start .. start + n)`.
    fn bump(&mut self, kind: u16, n: u32) {
        let start = self.pos;
        self.pos += n;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, start..self.pos));
    }

    /// Length of a bare field: the raw run to the next `,`, `\n`, or
    /// `"` (an embedded quote outside a quoted field ends the bare run;
    /// re-scanning from there drives the error path).
    fn bare_len(&self) -> u32 {
        let mut idx = self.pos as usize;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b',' | b'\n' | b'"' => break,
                // A CRLF terminates the row; a lone `\r` stays in the run.
                b'\r' if self.bytes.get(idx + 1) == Some(&b'\n') => break,
                _ => idx += 1,
            }
        }
        let end = u32::try_from(idx).unwrap_or(u32::MAX);
        end.saturating_sub(self.pos)
    }

    /// Length of a quoted field `"…"` with `""` escapes; unterminated →
    /// `None` (quotes may span lines, matching RFC 4180).
    fn quoted_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'"' => {
                    if self.bytes.get(idx + 1) == Some(&b'"') {
                        idx += 2; // escaped quote
                    } else {
                        return u32::try_from(idx + 1 - start).ok();
                    }
                }
                _ => idx += 1,
            }
        }
        None
    }

    /// Error + `ERROR_KIND` node around the unterminated quote: the
    /// region runs to EOF and the stray `"` rides it, guaranteeing
    /// progress (this is the pack's mandatory `consume_stray`).
    fn fail_unterminated(&mut self, start: u32) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        self.pos = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX);
        self.events.push(Event::Token {
            kind: ERROR_KIND,
            n_raw: 1,
        });
        self.tokens.push((ERROR_KIND, start..self.pos));
        self.events.push(Event::Finish);
    }

    fn parse_document(&mut self) {
        // A document is rows until EOF; a trailing newline does not
        // fabricate an empty final row after it.
        while self.peek().is_some() {
            self.parse_row();
        }
    }

    /// One row: `field (, field)* (newline | EOF)`. A blank line is a
    /// row of one empty field.
    fn parse_row(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::ROW,
            forward_parent: None,
        });
        loop {
            self.parse_field();
            self.skip_trivia(true);
            match self.peek() {
                Some(b',') => {
                    self.bump(tk::COMMA, 1);
                    self.skip_trivia(false);
                }
                // Row end: newline, CRLF, or EOF.
                Some(b'\n' | b'\r') | None => break,
                // Junk directly after a field (no comma): one error
                // region stopping at the next comma or the line end, so
                // the intact tail still parses.
                _ => {
                    let start = self.pos;
                    self.errors += 1;
                    self.events.push(Event::Error { diag: DIAG_SYNTAX });
                    self.events.push(Event::Start {
                        kind: ERROR_KIND,
                        forward_parent: None,
                    });
                    while let Some(b) = self.peek() {
                        if b == b',' || b == b'\n' || b == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                    self.events.push(Event::Token {
                        kind: ERROR_KIND,
                        n_raw: 1,
                    });
                    self.tokens.push((ERROR_KIND, start..self.pos));
                    self.events.push(Event::Finish);
                    match self.peek() {
                        Some(b',') => {
                            self.bump(tk::COMMA, 1);
                            self.skip_trivia(false);
                        }
                        _ => break,
                    }
                }
            }
        }
        match self.peek() {
            // `\r\n` and `\n` are both one NEWLINE token; a bare `\r`
            // (never followed by `\n`) rides the bare-field run instead.
            Some(b'\r') if self.bytes.get(self.pos as usize + 1) == Some(&b'\n') => {
                self.bump(tk::NEWLINE, 2);
            }
            Some(b'\n') => self.bump(tk::NEWLINE, 1),
            _ => {}
        }
        self.events.push(Event::Finish);
    }

    /// One field: quoted (with `""` escapes) or bare; empty fields are
    /// zero-width bare tokens. Unterminated quotes become error regions.
    fn parse_field(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FIELD,
            forward_parent: None,
        });
        if self.peek() == Some(b'"') {
            if let Some(n) = self.quoted_len() {
                self.bump(tk::QUOTED, n);
            } else {
                let start = self.pos;
                self.fail_unterminated(start);
            }
        } else {
            let n = self.bare_len();
            self.bump(tk::BARE, n); // zero-width for empty fields
        }
        self.events.push(Event::Finish);
    }
}

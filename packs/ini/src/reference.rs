//! Hand-written reference INI parser on the runtime-core event protocol
//! (issue 040). Line-structured recursive descent emitting 8-byte
//! events, byte-faithful with deterministic recovery: every malformed
//! construct becomes one `ERROR_KIND` node that consumes to the end of
//! its line (always progress, never a loop).

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

/// Parse INI source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the other packs):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; whitespace and comments are *never* token events.
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)`.
/// - **Errors**: `Error` + an `ERROR_KIND` node around the offending
///   region; recovery consumes to the end of the line (or one stray
///   character when already at the line end) and keeps parsing.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse INI source into a green tree (the full integration).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *INI* is recovered, not an error.
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

    /// The byte at `pos + offset` (does not advance).
    fn at(&self, offset: u32) -> Option<u8> {
        self.bytes.get(self.pos as usize + offset as usize).copied()
    }

    /// Skip whitespace (spaces, tabs, and the newlines that terminate
    /// lines) plus `;`/`#` comments, recording trivia.
    fn skip_trivia(&mut self, trailing: bool) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b';' | b'#') => {
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
    }

    /// Emit a token event + clean range `[start .. start + n)`.
    fn bump(&mut self, kind: u16, n: u32) {
        let start = self.pos;
        self.pos += n;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, start..self.pos));
    }

    /// Error + `ERROR_KIND` node around the skipped region. Sync: the
    /// end of the current line (line-structured grammar). `consume_stray`
    /// is mandatory here — when the region is empty the stray byte rides
    /// the error region so the parser always makes progress.
    fn fail_skip_line(&mut self, start: u32) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start && self.peek().is_some() {
            self.pos += 1; // the stray byte (a newline) rides the error region
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

    /// Length of the raw run at `self.pos` stopping before any of
    /// `stops` or a newline (bytes, never crossing a line).
    fn run_len(&self, stops: &[u8]) -> u32 {
        let mut idx = self.pos as usize;
        while idx < self.bytes.len() {
            let b = self.bytes[idx];
            if b == b'\n' || stops.contains(&b) {
                break;
            }
            idx += 1;
        }
        let end = u32::try_from(idx).unwrap_or(u32::MAX);
        end.saturating_sub(self.pos)
    }

    fn parse_document(&mut self) {
        loop {
            self.skip_trivia(false);
            let Some(b) = self.peek() else {
                break;
            };
            if b == b'[' {
                self.parse_section();
            } else {
                self.parse_setting();
            }
        }
    }

    /// `[name]` — the name is a raw run to the first `]` or newline.
    /// An unterminated header keeps its node and nests the error region
    /// covering the rest of the line (toml-pack discipline).
    fn parse_section(&mut self) {
        self.bump(tk::LBRACKET, 1);
        self.events.push(Event::Start {
            kind: kinds::SECTION,
            forward_parent: None,
        });
        let name_len = self.run_len(b"]");
        if name_len > 0 {
            self.bump(tk::BARE, name_len);
        }
        if self.peek() == Some(b']') {
            self.bump(tk::RBRACKET, 1);
        } else {
            let start = self.pos;
            self.fail_skip_line(start);
        }
        self.events.push(Event::Finish);
    }

    /// `key = value` — key is a raw run to `=`/newline; value is the raw
    /// run to the line end. A missing `=` nests the error region inside
    /// the setting node, covering the rest of the line.
    fn parse_setting(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::SETTING,
            forward_parent: None,
        });
        let key_len = self.run_len(b"=");
        if key_len == 0 || self.at(key_len) != Some(b'=') {
            let start = self.pos;
            self.fail_skip_line(start);
            self.events.push(Event::Finish);
            return;
        }
        self.bump(tk::BARE, key_len);
        self.skip_trivia(true);
        if self.peek() == Some(b'=') {
            self.bump(tk::EQUALS, 1);
        }
        self.skip_trivia(false);
        let value_len = self.run_len(&[]);
        if value_len > 0 {
            self.bump(tk::VALUE, value_len);
        }
        self.events.push(Event::Finish);
    }
}

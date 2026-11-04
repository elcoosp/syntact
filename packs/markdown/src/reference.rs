//! Hand-written reference Markdown-subset parser on the runtime-core
//! event protocol (issue 040). Line-structured recursive descent
//! emitting 8-byte events, byte-faithful with deterministic recovery:
//! any line the grammar rejects becomes one `ERROR_KIND` region
//! consuming to the line end (always progress, never a loop).

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

/// Parse Markdown source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the other packs):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; leading whitespace and blank lines are never token
///   events (line-terminating newlines ARE tokens).
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)`.
/// - **Errors**: `Error` + an `ERROR_KIND` node around the offending
///   region; recovery consumes to the line end (or one stray newline)
///   and keeps parsing.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse Markdown source into a green tree (the full integration).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *Markdown* is recovered, not an error.
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

    fn at(&self, offset: u32) -> Option<u8> {
        self.bytes.get(self.pos as usize + offset as usize).copied()
    }

    /// Skip leading spaces/tabs of a line plus whole blank lines —
    /// everything up to the first content byte of the next line.
    /// `trailing` (mid-line, after a heading marker) never consumes
    /// newlines: those are structural tokens. Line-terminating newlines
    /// after content are tokens, not trivia.
    fn skip_trivia(&mut self, trailing: bool) {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(b' ' | b'\t') => self.pos += 1,
                Some(b'\n') if !trailing => self.pos += 1,
                Some(b'\r') if !trailing && self.at(1) == Some(b'\n') => {
                    self.pos += 2;
                }
                _ => break,
            }
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

    /// Raw run to the end of the line (excluding the terminator). A
    /// `\r` always ends the run: followed by `\n` it is the CRLF
    /// terminator, alone it is the error point of the line.
    fn line_len(&self) -> u32 {
        let mut idx = self.pos as usize;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'\n' | b'\r' => break,
                _ => idx += 1,
            }
        }
        let end = u32::try_from(idx).unwrap_or(u32::MAX);
        end.saturating_sub(self.pos)
    }

    /// The line terminator: `\r\n` or `\n` as one token; absent at EOF.
    fn bump_newline(&mut self) {
        match self.peek() {
            Some(b'\r') if self.at(1) == Some(b'\n') => self.bump(tk::NEWLINE, 2),
            Some(b'\n') => self.bump(tk::NEWLINE, 1),
            _ => {}
        }
    }

    /// Error + `ERROR_KIND` node covering the rest of the line (or the
    /// stray newline when the line is already consumed) — the pack's
    /// mandatory consume-stray progress guarantee.
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
            self.pos += 1;
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

    /// Is the line at `self.pos` an ATX heading opening? 1–6 `#`s
    /// followed by a space, tab, or the line end.
    fn atx_len(&self) -> Option<u32> {
        if self.peek() != Some(b'#') {
            return None;
        }
        let mut hashes = 1u32;
        while self.at(hashes) == Some(b'#') {
            hashes += 1;
        }
        if hashes > 6 {
            return None; // 7+ hashes are plain text in this subset
        }
        match self.at(hashes) {
            None | Some(b' ' | b'\t' | b'\r' | b'\n') => Some(hashes),
            _ => None, // `#tag` is text, not a heading
        }
    }

    fn parse_document(&mut self) {
        loop {
            self.skip_trivia(false);
            let Some(b) = self.peek() else {
                break;
            };
            if b == b'#' && self.atx_len().is_some() {
                self.parse_heading();
            } else {
                self.parse_paragraph();
            }
        }
    }

    /// `# text` — the marker, the inline spaces (trivia), the text, the
    /// terminator.
    fn parse_heading(&mut self) {
        let hashes = self.atx_len().unwrap_or(1);
        self.bump(tk::ATX, hashes);
        self.events.push(Event::Start {
            kind: kinds::HEADING,
            forward_parent: None,
        });
        self.skip_trivia(true);
        let text_len = self.line_len();
        if text_len > 0 {
            self.bump(tk::TEXT, text_len);
        }
        if self.peek() == Some(b'\r') && self.at(1) != Some(b'\n') {
            // A stray CR: the error region rides it, inside the node.
            let start = self.pos;
            self.fail_skip_line(start);
        } else {
            self.bump_newline();
        }
        self.events.push(Event::Finish);
    }

    /// One paragraph line: text to the line end plus the terminator.
    /// An empty paragraph line cannot occur (blank lines are trivia);
    /// a stray CR becomes the line's error region.
    fn parse_paragraph(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PARAGRAPH,
            forward_parent: None,
        });
        let text_len = self.line_len();
        if text_len > 0 {
            self.bump(tk::TEXT, text_len);
        }
        if self.peek() == Some(b'\r') && self.at(1) != Some(b'\n') {
            let start = self.pos;
            self.fail_skip_line(start);
        } else {
            self.bump_newline();
        }
        self.events.push(Event::Finish);
    }
}

//! Hand-written reference Python-subset parser on the runtime-core event
//! protocol (issue 028). A CPython-flavoured scanner suite (`INDENT` /
//! `DEDENT` column stack, `NEWLINE` logical-line terminators, bracket
//! continuation) fused with a recursive-descent statement grammar and a
//! Pratt expression engine — byte-faithful with deterministic recovery
//! (skip to the next line / closer, always progress).

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

/// Maximum nesting depth the recursive-descent parser will follow
/// (paren/list cycles `parse_expr_bp → atom → parse_paren/parse_list →
/// parse_expr_bp`, suites cycle `parse_suite → statement → parse_suite`
/// through ever-deeper indentation). Deeper nesting fails cleanly: the
/// opener rides an ERROR region (lossless), the parse resynchronizes and
/// the process stays alive. Nesting bombs (`((((…`, `[[[[…`, indent
/// pyramids) are a classic stack-smash `DoS` and MUST NOT reach the stack.
pub const MAX_DEPTH: usize = 512;

/// Resolve a diagnostic side-table index to its message text.
#[must_use]
pub fn error_message(diag: u32) -> &'static str {
    match diag {
        DIAG_SYNTAX => "syntax error",
        _ => "unknown diagnostic",
    }
}

/// Parse Python-subset source into the raw event stream:
/// `(events, token_ranges, error_count)`.
///
/// Contract (mirrors the JSON/TOML packs):
/// - **Events**: `Start`/`Finish` bracket nodes; `Token` leaves with
///   `n_raw == 1`; whitespace and comments are *never* token events.
/// - **Ranges**: clean `(kind, range)` pairs parallel to token events;
///   ranges + trivia records partition `[0, len)` (`INDENT` tokens own
///   their indent run; `DEDENT` tokens are zero-width).
/// - **Errors**: `Error` + an ERROR node around the offending region;
///   recovery skips to the next newline / bracket closer and keeps
///   parsing — deterministic and lossless. EOF closes all open suites.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_document();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse Python-subset source into a green tree (one implicit
/// [`crate::ROOT_KIND`] root).
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
    /// Current expr/container/suite nesting depth, bounded by
    /// [`MAX_DEPTH`].
    nest_depth: usize,
    /// CPython-style indent column stack (`0` is the bottom sentinel).
    indents: Vec<u32>,
    /// Bracket nesting depth: > 0 turns newlines into trivia.
    depth: usize,
    /// Kind of the last emitted token.
    last_token: Option<u16>,
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
            nest_depth: 0,
            indents: vec![0],
            depth: 0,
            last_token: None,
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

    /// Whether a keyword (`if`, `in`, …) starts here as a standalone
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

    fn kw_len(kw: &str) -> u32 {
        u32::try_from(kw.len()).unwrap_or(u32::MAX)
    }

    /// Skip whitespace, `\`-continuations, and comments, recording them
    /// as trivia. Newlines are trivia only inside brackets (the logical
    /// line continues); otherwise the caller emits the `NEWLINE` token.
    fn skip_trivia(&mut self) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\x0c') => self.pos += 1,
                Some(b'\\')
                    if matches!(self.at(1), Some(b'\r' | b'\n'))
                        || (self.at(1) == Some(b'\r') && self.at(2) == Some(b'\n')) =>
                {
                    self.pos += 1;
                    if self.peek() == Some(b'\r') {
                        self.pos += 1;
                    }
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                }
                Some(b'\n') if self.depth > 0 => self.pos += 1,
                Some(b'#') => {
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
        self.last_token = Some(kind);
    }

    /// Zero-width token (the `DEDENT` markers).
    fn bump_zero(&mut self, kind: u16) {
        let at = self.pos;
        self.events.push(Event::Token { kind, n_raw: 1 });
        self.tokens.push((kind, at..at));
        self.last_token = Some(kind);
    }

    /// Bracket opener/closer: emits the token and tracks the depth (the
    /// trivia layer's newline handling keys off it).
    fn bump_bracket(&mut self, kind: u16, n: u32, open: bool) {
        self.bump(kind, n);
        if open {
            self.depth += 1;
        } else {
            self.depth = self.depth.saturating_sub(1);
        }
    }

    /// Error + ERROR node around the skipped region. Sync: stops before
    /// a newline, a bracket closer, or a comment; those boundaries stay
    /// for the structural loops to consume (progress is guaranteed by
    /// the caller consuming the boundary or by the loop guards).
    fn fail_skip(&mut self, start: u32, consume_stray: bool) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        while let Some(b) = self.peek() {
            if matches!(b, b'\n' | b')' | b']' | b'}' | b'#') {
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
            self.last_token = Some(ERROR_KIND);
        }
        self.events.push(Event::Finish);
    }

    /// Zero-width tolerated error (inconsistent dedent, empty suite).
    fn error_zero_width(&mut self) {
        self.errors += 1;
        self.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
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
        self.last_token = Some(ERROR_KIND);
        self.events.push(Event::Finish);
    }

    /// Insert a `Start` event retroactively at `mark` and close the node
    /// at the current end — the precede-insert discipline: the node
    /// spans everything parsed since `mark`.
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

    /// Wrap an implicit comma list into a TUPLE node spanning from
    /// `mark` (before the first element).
    fn wrap_tuple(&mut self, mark: usize) {
        self.wrap_node(mark, kinds::TUPLE);
    }

    // ------------------------------------------------------------------
    // Scanner: lines, INDENT/DEDENT, names, numbers, strings
    // ------------------------------------------------------------------

    /// Process a line start: skip blank / comment-only lines, then run
    /// the INDENT/DEDENT column stack against the content line's indent
    /// run. Called at document start and after every logical `NEWLINE`.
    fn advance_line_start(&mut self) {
        loop {
            let ws_start = self.pos;
            let mut col: u32 = 0;
            while let Some(b) = self.peek() {
                match b {
                    b' ' => {
                        self.pos += 1;
                        col += 1;
                    }
                    b'\t' => {
                        self.pos += 1;
                        col = col / 8 * 8 + 8; // tab = next multiple of 8
                    }
                    b'\r' | b'\x0c' => self.pos += 1,
                    _ => break,
                }
            }
            match self.peek() {
                None => {
                    if self.pos > ws_start {
                        self.push_trivia(ws_start, tk::WHITESPACE, false);
                    }
                    self.flush_dedents();
                    return;
                }
                Some(b'\n') => {
                    // Blank line: the run including the newline is trivia.
                    self.pos += 1;
                    self.push_trivia(ws_start, tk::WHITESPACE, false);
                }
                Some(b'#') => {
                    // Comment-only line: the comment trivia record owns
                    // its terminating newline (no NEWLINE token).
                    if self.pos > ws_start {
                        self.push_trivia(ws_start, tk::WHITESPACE, false);
                    }
                    let cstart = self.pos;
                    while let Some(b) = self.peek() {
                        self.pos += 1;
                        if b == b'\n' {
                            break;
                        }
                    }
                    self.push_trivia(cstart, tk::COMMENT, false);
                }
                Some(_) => {
                    let ws_end = self.pos;
                    if self.depth > 0 {
                        // Inside brackets: no indent processing.
                        if ws_end > ws_start {
                            self.push_trivia(ws_start, tk::WHITESPACE, true);
                        }
                        return;
                    }
                    let top = *self.indents.last().unwrap_or(&0);
                    if col > top {
                        // The indent run IS the INDENT token: rewind to
                        // the run's start so the range covers it.
                        self.pos = ws_start;
                        self.bump(tk::INDENT, ws_end - ws_start);
                        self.indents.push(col);
                    } else if col < top {
                        if ws_end > ws_start {
                            self.push_trivia(ws_start, tk::WHITESPACE, false);
                        }
                        while self.indents.len() > 1 && *self.indents.last().unwrap_or(&0) > col {
                            self.indents.pop();
                            self.bump_zero(tk::DEDENT);
                        }
                        if *self.indents.last().unwrap_or(&0) < col {
                            self.error_zero_width(); // inconsistent dedent
                        }
                    } else if ws_end > ws_start {
                        self.push_trivia(ws_start, tk::WHITESPACE, false);
                    }
                    return;
                }
            }
        }
    }

    /// Pop every remaining indent level at EOF (zero-width `DEDENT`s).
    fn flush_dedents(&mut self) {
        while self.indents.len() > 1 {
            self.indents.pop();
            self.bump_zero(tk::DEDENT);
        }
    }

    /// End a logical line: emit the `NEWLINE` token (outside brackets)
    /// or trivia (inside), then process the next line start.
    fn bump_newline(&mut self) {
        if self.depth > 0 {
            let start = self.pos;
            if self.peek() == Some(b'\r') {
                self.pos += 1;
            }
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
            if self.pos > start {
                let trailing = start == self.last_end && self.last_end > 0;
                self.push_trivia(start, tk::WHITESPACE, trailing);
            }
            return;
        }
        let start = self.pos;
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
            self.events.push(Event::Token {
                kind: tk::NEWLINE,
                n_raw: 1,
            });
            self.tokens.push((tk::NEWLINE, start..self.pos));
            self.last_end = self.pos;
            self.last_token = Some(tk::NEWLINE);
        }
        self.advance_line_start();
    }

    /// Whether the logical line ends here (newline, comment, or EOF).
    fn at_line_end(&self) -> bool {
        matches!(self.peek(), None | Some(b'\n' | b'#'))
    }

    /// Identifier length (`[A-Za-z_][A-Za-z0-9_]*`).
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

    /// Bump a keyword/name of `kw` (the caller validated `peek_kw`).
    fn bump_kw(&mut self, kw: &str) {
        self.bump(tk::NAME, Self::kw_len(kw));
    }

    /// Numeric literal length: `0x`/`0o`/`0b` radix bodies, decimal
    /// digits with `_`, a fractional part, an exponent, and an optional
    /// imaginary `j` suffix.
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
        if idx < self.bytes.len() && matches!(self.bytes[idx], b'j' | b'J') {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// Whether a name is a string prefix (`r`, `b`, `u`, `rb`, `br`,
    /// `f`, `fr`, `rf`, case-insensitive).
    fn is_string_prefix(name: &[u8]) -> bool {
        if name.is_empty() || name.len() > 2 {
            return false;
        }
        name.iter()
            .all(|b| matches!(b, b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F'))
    }

    fn prefix_has_f(name: &[u8]) -> bool {
        name.iter().any(|b| *b == b'f' || *b == b'F')
    }

    /// Plain (non-f) string literal body: quote, escapes, closing
    /// quote; triple-quoted spans lines. `None` = unterminated.
    fn scan_plain_string_len(&self, quote: u8, triple: bool) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start;
        idx += if triple { 3 } else { 1 };
        loop {
            match self.bytes.get(idx)? {
                b'\\' => idx += 2,
                b if *b == quote => {
                    if triple {
                        if self.bytes.get(idx + 1) == Some(&quote)
                            && self.bytes.get(idx + 2) == Some(&quote)
                        {
                            return u32::try_from(idx + 3 - start).ok();
                        }
                        idx += 1;
                    } else {
                        return u32::try_from(idx + 1 - start).ok();
                    }
                }
                b'\n' if !triple => return None,
                _ => idx += 1,
            }
        }
    }

    // ------------------------------------------------------------------
    // Document / statements
    // ------------------------------------------------------------------

    fn parse_document(&mut self) {
        self.advance_line_start();
        while self.peek().is_some() {
            self.parse_statement_with_progress();
        }
        // Mid-line EOF (no trailing newline): flush remaining levels.
        self.flush_dedents();
    }

    /// One module-level statement with the progress guard: a statement
    /// that consumed nothing (stray closers) gets one byte eaten.
    fn parse_statement_with_progress(&mut self) {
        let before = self.pos;
        self.parse_statement();
        if self.pos == before && self.peek().is_some() {
            self.force_byte();
        }
    }

    fn parse_statement(&mut self) {
        if self.peek() == Some(b'@') {
            self.parse_decorated();
        } else if self.peek_kw("if")
            || self.peek_kw("while")
            || self.peek_kw("for")
            || self.peek_kw("def")
            || self.peek_kw("class")
        {
            self.parse_compound();
        } else {
            self.parse_simple_line();
        }
    }

    /// `@` decorator lines followed by the decorated `def`/`class`.
    fn parse_decorated(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::DECORATED,
            forward_parent: None,
        });
        loop {
            if self.peek() != Some(b'@') {
                break;
            }
            self.events.push(Event::Start {
                kind: kinds::DECORATOR,
                forward_parent: None,
            });
            self.bump(tk::AT_SIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
            self.bump_newline();
            self.events.push(Event::Finish);
        }
        if self.peek_kw("def") || self.peek_kw("class") {
            self.parse_compound();
        } else {
            // A decorator with no def/class behind it: tolerated error.
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    fn parse_compound(&mut self) {
        if self.peek_kw("if") {
            self.parse_if();
        } else if self.peek_kw("while") {
            self.parse_while();
        } else if self.peek_kw("for") {
            self.parse_for();
        } else if self.peek_kw("def") {
            self.parse_def();
        } else {
            self.parse_class();
        }
    }

    /// `:` suite — either `NEWLINE INDENT … (dedent)` or a single line.
    /// The block loop runs while the current line's column (the indent
    /// stack top) stays strictly deeper than the enclosing block: this
    /// is what makes batched `DEDENT`s close exactly the right number
    /// of nested suites.
    fn parse_suite(&mut self) {
        if self.nest_depth >= MAX_DEPTH {
            // Indentation pyramid: no opener to consume — emit a
            // zero-width error and return without recursing (the outer
            // statement loop keeps making byte progress).
            self.error_zero_width();
            return;
        }
        self.nest_depth += 1;
        self.parse_suite_guarded();
        self.nest_depth -= 1;
    }

    /// The guarded body of [`Self::parse_suite`] (depth accounting lives
    /// in the wrapper).
    fn parse_suite_guarded(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'\n') {
            self.bump_newline();
            if self.last_token == Some(tk::INDENT) {
                let body_col = *self.indents.last().unwrap_or(&0);
                // Re-scope the already-emitted INDENT token into the
                // SUITE node (it is the last event).
                let mark = self.events.len() - 1;
                self.events.insert(
                    mark,
                    Event::Start {
                        kind: kinds::SUITE,
                        forward_parent: None,
                    },
                );
                while self.peek().is_some() {
                    let top = *self.indents.last().unwrap_or(&0);
                    if top < body_col {
                        break;
                    }
                    self.parse_statement_with_progress();
                }
                self.events.push(Event::Finish);
            } else {
                // No INDENT after the header: empty suite, tolerated.
                self.error_zero_width();
                self.events.push(Event::Start {
                    kind: kinds::SUITE,
                    forward_parent: None,
                });
                self.events.push(Event::Finish);
            }
        } else {
            self.events.push(Event::Start {
                kind: kinds::SUITE,
                forward_parent: None,
            });
            self.parse_simple_line();
            self.events.push(Event::Finish);
        }
    }

    /// `if` / `elif` chains and `else` (nested as sibling nodes).
    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IF,
            forward_parent: None,
        });
        self.parse_if_head();
        self.parse_if_tail();
        self.events.push(Event::Finish);
    }

    /// The `if`/`elif` keyword through the suite (node already opened).
    fn parse_if_head(&mut self) {
        self.bump_kw("if");
        self.skip_trivia();
        self.parse_expr_bp(0);
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_suite();
    }

    /// `elif`/`else` clauses following an `if`/`while`/`for` suite.
    fn parse_if_tail(&mut self) {
        loop {
            if self.peek_kw("elif") {
                self.events.push(Event::Start {
                    kind: kinds::IF,
                    forward_parent: None,
                });
                self.bump_kw("elif");
                self.skip_trivia();
                self.parse_expr_bp(0);
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.parse_suite();
                self.events.push(Event::Finish);
                continue;
            }
            if self.peek_kw("else") {
                self.events.push(Event::Start {
                    kind: kinds::ELSE,
                    forward_parent: None,
                });
                self.bump_kw("else");
                self.skip_trivia();
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.parse_suite();
                self.events.push(Event::Finish);
            }
            break;
        }
    }

    /// The `keyword [expr] : suite` shape shared by `while`/`for` heads.
    fn parse_head_expr_colon(&mut self) {
        self.skip_trivia();
        self.parse_expr_bp(0);
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_suite();
    }

    fn parse_while(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::WHILE,
            forward_parent: None,
        });
        self.bump_kw("while");
        self.parse_head_expr_colon();
        self.parse_if_tail(); // while … else
        self.events.push(Event::Finish);
    }

    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FOR,
            forward_parent: None,
        });
        self.bump_kw("for");
        self.skip_trivia();
        self.parse_target();
        self.skip_trivia();
        if self.peek_kw("in") {
            self.bump_kw("in");
            self.skip_trivia();
            let mark = self.events.len();
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek() == Some(b',') {
                // `for x in a, b:` — implicit tuple iterable.
                while self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    self.skip_trivia();
                    if self.peek() == Some(b':') || self.at_line_end() {
                        break;
                    }
                    self.parse_expr_bp(0);
                    self.skip_trivia();
                }
                self.wrap_tuple(mark);
            }
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_suite();
        self.parse_if_tail(); // for … else
        self.events.push(Event::Finish);
    }

    fn parse_def(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FUNC_DEF,
            forward_parent: None,
        });
        self.bump_kw("def");
        self.skip_trivia();
        let name_len = self.scan_name_len();
        if name_len > 0 {
            self.bump(tk::NAME, name_len);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump_bracket(tk::LPAREN, 1, true);
            self.parse_params();
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b'-') && self.at(1) == Some(b'>') {
            self.bump(tk::RARROW, 2);
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
        }
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_suite();
        self.events.push(Event::Finish);
    }

    /// Parameter list: `name[: ann [= default]]`, `*args`, `**kwargs`.
    fn parse_params(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PARAMS,
            forward_parent: None,
        });
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                Some(b'*') => {
                    if self.at(1) == Some(b'*') {
                        self.bump(tk::POWER, 2);
                    } else {
                        self.bump(tk::STAR, 1);
                    }
                    self.skip_trivia();
                    let name_len = self.scan_name_len();
                    if name_len > 0 {
                        self.bump(tk::NAME, name_len);
                        self.parse_param_annotation();
                    }
                }
                _ => {
                    let name_len = self.scan_name_len();
                    if name_len == 0 {
                        if self.at_line_end() || self.peek() == Some(b')') {
                            // Broken header: stop eating params; the
                            // caller's closer/colon checks report once.
                            break;
                        }
                        let start = self.pos;
                        self.fail_skip(start, true);
                    } else {
                        self.bump(tk::NAME, name_len);
                        self.parse_param_annotation();
                        self.skip_trivia();
                        if self.peek() == Some(b'=') {
                            self.bump(tk::EQUALS, 1);
                            self.skip_trivia();
                            self.parse_expr_bp(0);
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
            self.bump_bracket(tk::RPAREN, 1, false);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// Optional `: annotation` after a parameter name.
    fn parse_param_annotation(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
        }
    }

    fn parse_class(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::CLASS_DEF,
            forward_parent: None,
        });
        self.bump_kw("class");
        self.skip_trivia();
        let name_len = self.scan_name_len();
        if name_len > 0 {
            self.bump(tk::NAME, name_len);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump_bracket(tk::LPAREN, 1, true);
            self.parse_call_args_inner();
        }
        self.skip_trivia();
        if self.peek() == Some(b':') {
            self.bump(tk::COLON, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.parse_suite();
        self.events.push(Event::Finish);
    }

    /// One logical line of simple statements (`;`-separated).
    fn parse_simple_line(&mut self) {
        loop {
            self.parse_simple_stmt();
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
                self.skip_trivia();
                if self.at_line_end() {
                    break; // trailing `;`
                }
                continue;
            }
            break;
        }
        if self.peek() == Some(b'\n') {
            self.bump_newline();
        } else if !self.at_line_end() {
            // Garbage before the newline: one error, then the line ends.
            let start = self.pos;
            self.fail_skip(start, true);
            self.skip_trivia();
            if self.peek() == Some(b'\n') {
                self.bump_newline();
            }
        }
    }

    fn parse_simple_stmt(&mut self) {
        #![allow(clippy::too_many_lines)] // one dispatch table reads best whole
        if self.peek_kw("return") {
            self.events.push(Event::Start {
                kind: kinds::RETURN,
                forward_parent: None,
            });
            self.bump_kw("return");
            self.skip_trivia();
            if !self.at_line_end() && self.peek() != Some(b';') {
                let mark = self.events.len();
                self.parse_expr_bp(0);
                self.maybe_value_tuple(mark);
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("pass") {
            self.events.push(Event::Start {
                kind: kinds::PASS,
                forward_parent: None,
            });
            self.bump_kw("pass");
            self.events.push(Event::Finish);
        } else if self.peek_kw("break") {
            self.events.push(Event::Start {
                kind: kinds::BREAK,
                forward_parent: None,
            });
            self.bump_kw("break");
            self.events.push(Event::Finish);
        } else if self.peek_kw("continue") {
            self.events.push(Event::Start {
                kind: kinds::CONTINUE,
                forward_parent: None,
            });
            self.bump_kw("continue");
            self.events.push(Event::Finish);
        } else if self.peek_kw("import") {
            self.parse_import();
        } else if self.peek_kw("from") {
            self.parse_from_import();
        } else if self.peek_kw("del") {
            self.events.push(Event::Start {
                kind: kinds::DEL,
                forward_parent: None,
            });
            self.bump_kw("del");
            self.skip_trivia();
            self.parse_expr_bp(11);
            self.skip_trivia();
            while self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                if self.at_line_end() || self.peek() == Some(b';') {
                    break;
                }
                self.parse_expr_bp(11);
                self.skip_trivia();
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("global") {
            self.events.push(Event::Start {
                kind: kinds::GLOBAL,
                forward_parent: None,
            });
            self.bump_kw("global");
            loop {
                self.skip_trivia();
                let n = self.scan_name_len();
                if n == 0 {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
                self.bump(tk::NAME, n);
                self.skip_trivia();
                if self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    continue;
                }
                break;
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("assert") {
            self.events.push(Event::Start {
                kind: kinds::ASSERT,
                forward_parent: None,
            });
            self.bump_kw("assert");
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                self.parse_expr_bp(0);
            }
            self.events.push(Event::Finish);
        } else if self.peek_kw("raise") {
            self.events.push(Event::Start {
                kind: kinds::RAISE,
                forward_parent: None,
            });
            self.bump_kw("raise");
            self.skip_trivia();
            if !self.at_line_end() && self.peek() != Some(b';') {
                self.parse_expr_bp(0);
                self.skip_trivia();
                if self.peek_kw("from") {
                    self.bump_kw("from");
                    self.skip_trivia();
                    self.parse_expr_bp(0);
                }
            }
            self.events.push(Event::Finish);
        } else {
            self.parse_expr_or_assign();
        }
    }

    fn parse_import(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IMPORT,
            forward_parent: None,
        });
        self.bump_kw("import");
        loop {
            self.skip_trivia();
            self.events.push(Event::Start {
                kind: kinds::ALIAS,
                forward_parent: None,
            });
            self.parse_dotted_name();
            self.skip_trivia();
            if self.peek_kw("as") {
                self.bump_kw("as");
                self.skip_trivia();
                let n = self.scan_name_len();
                if n > 0 {
                    self.bump(tk::NAME, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
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
        self.events.push(Event::Finish);
    }

    /// `name(.name)*` — the dotted head of an import alias.
    /// The module path of `from`: relative leading dots (`from .`,
    /// `from ..pkg`) then an optional dotted name. The `import`
    /// keyword terminates the path (a bare relative import is legal).
    fn parse_from_module(&mut self) {
        let mut dots = 0_u32;
        while self.peek() == Some(b'.') {
            self.bump(tk::DOT, 1);
            self.skip_trivia();
            dots += 1;
        }
        let n = if self.peek_kw("import") {
            0
        } else {
            self.scan_name_len()
        };
        if n > 0 {
            self.bump(tk::NAME, n);
            while self.peek() == Some(b'.') {
                self.bump(tk::DOT, 1);
                self.skip_trivia();
                let n = self.scan_name_len();
                if n > 0 {
                    self.bump(tk::NAME, n);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                    break;
                }
            }
        } else if dots == 0 {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    fn parse_dotted_name(&mut self) {
        let n = self.scan_name_len();
        if n > 0 {
            self.bump(tk::NAME, n);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        }
        while self.peek() == Some(b'.') {
            self.bump(tk::DOT, 1);
            self.skip_trivia();
            let n = self.scan_name_len();
            if n > 0 {
                self.bump(tk::NAME, n);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
                break;
            }
        }
    }

    fn parse_from_import(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FROM_IMPORT,
            forward_parent: None,
        });
        self.bump_kw("from");
        self.skip_trivia();
        self.parse_from_module();
        self.skip_trivia();
        if self.peek_kw("import") {
            self.bump_kw("import");
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        if self.peek() == Some(b'*') {
            self.bump(tk::STAR, 1);
        } else {
            let paren = self.peek() == Some(b'(');
            if paren {
                self.bump_bracket(tk::LPAREN, 1, true);
            }
            loop {
                self.skip_trivia();
                if self.peek() == Some(b')') {
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
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.skip_trivia();
                if self.peek_kw("as") {
                    self.bump_kw("as");
                    self.skip_trivia();
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
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
            if paren {
                if self.peek() == Some(b')') {
                    self.bump_bracket(tk::RPAREN, 1, false);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
        }
        self.events.push(Event::Finish);
    }

    /// Assignment / augmented assignment / bare expression statement.
    fn parse_expr_or_assign(&mut self) {
        let mark = self.events.len();
        self.parse_expr_bp(0);
        let mut tuple = false;
        self.skip_trivia();
        if self.peek() == Some(b',') {
            // Target list: `a, b = …` or a bare tuple statement.
            while self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                if self.at_line_end()
                    || self.peek() == Some(b';')
                    || self.peek() == Some(b'=')
                    || self.peek_aug().is_some()
                {
                    break;
                }
                self.parse_expr_bp(0);
                self.skip_trivia();
            }
            self.wrap_tuple(mark);
            tuple = true;
        }
        let aug = self.peek_aug();
        if self.peek() == Some(b'=') || aug.is_some() {
            let kind = if aug.is_some() {
                kinds::AUG_ASSIGN
            } else {
                kinds::ASSIGN
            };
            self.events.insert(
                mark,
                Event::Start {
                    kind,
                    forward_parent: None,
                },
            );
            if let Some(op) = aug {
                self.bump(op, aug_len(op));
                self.skip_trivia();
                let vmark = self.events.len();
                self.parse_expr_bp(0);
                self.maybe_value_tuple(vmark);
            } else {
                loop {
                    self.bump(tk::EQUALS, 1);
                    self.skip_trivia();
                    let vmark = self.events.len();
                    self.parse_expr_bp(0);
                    self.maybe_value_tuple(vmark);
                    self.skip_trivia();
                    if self.peek() == Some(b'=') {
                        continue; // chained assignment
                    }
                    break;
                }
            }
            self.events.push(Event::Finish);
        } else if !tuple {
            self.wrap_node(mark, kinds::EXPR_STMT);
        }
    }

    /// Wrap a trailing `, …` value list into an implicit TUPLE spanning
    /// from `mark` (before the first element).
    fn maybe_value_tuple(&mut self, mark: usize) {
        self.skip_trivia();
        if self.peek() != Some(b',') {
            return;
        }
        while self.peek() == Some(b',') {
            self.bump(tk::COMMA, 1);
            self.skip_trivia();
            if self.at_line_end()
                || self.peek() == Some(b';')
                || self.peek() == Some(b')')
                || self.peek() == Some(b'}')
                || self.peek() == Some(b']')
            {
                break;
            }
            self.parse_expr_bp(0);
            self.skip_trivia();
        }
        self.wrap_tuple(mark);
    }

    fn peek_aug(&self) -> Option<u16> {
        if self.starts_with(b"**=") {
            Some(tk::POWEREQ)
        } else if self.starts_with(b"//=") {
            Some(tk::DOUBLE_SLASHEQ)
        } else if self.starts_with(b"<<=") {
            Some(tk::LSHIFT_EQ)
        } else if self.starts_with(b">>=") {
            Some(tk::RSHIFT_EQ)
        } else if self.starts_with(b"@=") {
            Some(tk::AT_SIGN_EQ)
        } else if self.starts_with(b"+=") {
            Some(tk::PLUSEQ)
        } else if self.starts_with(b"-=") {
            Some(tk::MINUSEQ)
        } else if self.starts_with(b"*=") {
            Some(tk::STAREQ)
        } else if self.starts_with(b"/=") {
            Some(tk::SLASHEQ)
        } else if self.starts_with(b"%=") {
            Some(tk::PERCENTEQ)
        } else if self.starts_with(b"&=") {
            Some(tk::AMP_EQ)
        } else if self.starts_with(b"|=") {
            Some(tk::PIPE_EQ)
        } else if self.starts_with(b"^=") {
            Some(tk::CARET_EQ)
        } else {
            None
        }
    }

    // ------------------------------------------------------------------
    // Pratt expressions
    // ------------------------------------------------------------------

    /// Pratt core: prefix unary, atom + postfix, then binary ops by
    /// binding power, then the ternary conditional at `min_bp == 0`.
    /// Nodes wrap retroactively via [`Self::wrap_node`] at `mark`.
    fn parse_expr_bp(&mut self, min_bp: u8) {
        if self.nest_depth >= MAX_DEPTH {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        }
        self.nest_depth += 1;
        self.parse_expr_bp_guarded(min_bp);
        self.nest_depth -= 1;
    }

    /// The guarded body of [`Self::parse_expr_bp`] (depth accounting
    /// lives in the wrapper).
    fn parse_expr_bp_guarded(&mut self, min_bp: u8) {
        let mark = self.events.len();
        if let Some((kind, n, rbp)) = self.peek_unary() {
            self.bump(kind, n);
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            self.wrap_node(mark, kinds::UNARY);
        } else {
            self.parse_atom();
        }
        self.skip_trivia();
        self.parse_postfix(mark);
        self.skip_trivia();
        self.parse_binops(mark, min_bp);
        self.skip_trivia();
        if min_bp == 0 && self.peek_kw("if") {
            self.bump_kw("if");
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek_kw("else") {
                self.bump_kw("else");
                self.skip_trivia();
                self.parse_expr_bp(0);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.wrap_node(mark, kinds::CONDITIONAL);
        }
    }

    /// Prefix unary operators: `-`/`+`/`~` (bind looser than `**`) and
    /// `not` (binds looser than comparisons, tighter than `and`).
    fn peek_unary(&self) -> Option<(u16, u32, u8)> {
        match self.peek() {
            Some(b'-') if !self.starts_with(b"-=") && !self.starts_with(b"->") => {
                Some((tk::MINUS, 1, 11))
            }
            Some(b'+') if !self.starts_with(b"+=") => Some((tk::PLUS, 1, 11)),
            Some(b'~') => Some((tk::TILDE, 1, 11)),
            Some(_) if self.peek_kw("not") => Some((tk::NAME, Self::kw_len("not"), 4)),
            _ => None,
        }
    }

    /// Postfix loop: calls, subscripts, attribute access — each wraps
    /// the whole left extent via precede-insert at `mark`.
    fn parse_postfix(&mut self, mark: usize) {
        loop {
            match self.peek() {
                Some(b'(') => {
                    self.bump_bracket(tk::LPAREN, 1, true);
                    self.parse_call_args_inner();
                    self.wrap_node(mark, kinds::CALL);
                }
                Some(b'[') => {
                    self.bump_bracket(tk::LBRACKET, 1, true);
                    self.skip_trivia();
                    if self.peek() != Some(b']') && self.peek().is_some() {
                        self.parse_expr_bp(0);
                        self.skip_trivia();
                    }
                    if self.peek() == Some(b']') {
                        self.bump_bracket(tk::RBRACKET, 1, false);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.wrap_node(mark, kinds::SUBSCRIPT);
                }
                Some(b'.') => {
                    self.bump(tk::DOT, 1);
                    self.skip_trivia();
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.wrap_node(mark, kinds::ATTRIBUTE);
                }
                _ => break,
            }
            self.skip_trivia();
        }
    }

    /// Call/class-base arguments: `expr`, `name = expr`, `*expr`,
    /// `**expr`, comma-separated, trailing comma tolerated.
    fn parse_call_args_inner(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b')') | None => break,
                Some(b'*') => {
                    if self.at(1) == Some(b'*') {
                        self.bump(tk::POWER, 2);
                    } else {
                        self.bump(tk::STAR, 1);
                    }
                    self.skip_trivia();
                    self.parse_expr_bp(0);
                }
                _ => {
                    // keyword argument? NAME `=` (not `==`)
                    let n = self.scan_name_len();
                    if n > 0 && self.at(n) == Some(b'=') && self.at(n + 1) != Some(b'=') {
                        self.bump(tk::NAME, n);
                        self.skip_trivia();
                        self.bump(tk::EQUALS, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(0);
                    } else {
                        self.parse_expr_bp(0);
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
            self.bump_bracket(tk::RPAREN, 1, false);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// Binary operators by binding power. Keyword ops (`or`/`and`,
    /// comparisons incl. `not in` / `is not`) ride the same table.
    /// Comparisons chain inside one `COMPARE` node; `**` is
    /// right-associative; everything else is left-associative.
    fn parse_binops(&mut self, mark: usize, min_bp: u8) {
        let mut open_compare = false;
        while let Some((lbp, rbp, is_cmp)) = self.peek_binop() {
            if lbp < min_bp {
                break;
            }
            if is_cmp && open_compare {
                // Chain into the open COMPARE node.
                self.bump_binop();
                self.skip_trivia();
                self.parse_expr_bp(rbp);
                continue;
            }
            if open_compare {
                self.events.push(Event::Finish);
                open_compare = false;
            }
            self.bump_binop();
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            if is_cmp {
                self.events.insert(
                    mark,
                    Event::Start {
                        kind: kinds::COMPARE,
                        forward_parent: None,
                    },
                );
                open_compare = true;
            } else {
                self.wrap_node(mark, kinds::BINOP);
            }
        }
        if open_compare {
            self.events.push(Event::Finish);
        }
    }

    /// Bump the operator at the cursor (multi-token keyword ops emit
    /// one token per word).
    fn bump_binop(&mut self) {
        if self.starts_with(b"not in") {
            self.bump(tk::NAME, 3);
            self.skip_trivia();
            self.bump(tk::NAME, 2);
        } else if self.starts_with(b"is not") {
            self.bump(tk::NAME, 2);
            self.skip_trivia();
            self.bump(tk::NAME, 3);
        } else if self.peek_kw("or") {
            self.bump_kw("or");
        } else if self.peek_kw("and") {
            self.bump_kw("and");
        } else if self.peek_kw("in") {
            self.bump_kw("in");
        } else if self.peek_kw("is") {
            self.bump_kw("is");
        } else {
            let (kind, n) = self.peek_punct().unwrap_or((tk::LT, 1));
            self.bump(kind, n);
        }
    }

    fn peek_binop(&self) -> Option<(u8, u8, bool)> {
        let two_word =
            |a: &str, b: &str| self.peek_kw(a) && self.starts_with(format!("{a} {b}").as_bytes());
        if two_word("not", "in") || two_word("is", "not") {
            return Some((4, 5, true));
        }
        if self.peek_kw("in") || self.peek_kw("is") {
            return Some((4, 5, true));
        }
        if self.peek_kw("or") {
            return Some((1, 2, false));
        }
        if self.peek_kw("and") {
            return Some((2, 3, false));
        }
        let (kind, _) = self.peek_punct()?;
        let (lbp, rbp, is_cmp) = match kind {
            tk::PIPE => (5_u8, 6_u8, false),
            tk::CARET => (6, 7, false),
            tk::AMP => (7, 8, false),
            tk::LSHIFT | tk::RSHIFT => (8, 9, false),
            tk::PLUS | tk::MINUS => (9, 10, false),
            tk::STAR | tk::SLASH | tk::DOUBLE_SLASH | tk::PERCENT | tk::AT_SIGN => (10, 11, false),
            tk::POWER => (12, 12, false), // right-associative
            tk::EQ | tk::NEQ | tk::LE | tk::GE | tk::LT | tk::GT => (4, 5, true),
            _ => return None,
        };
        Some((lbp, rbp, is_cmp))
    }

    /// The punctuation token at the cursor: `(kind, byte length)`.
    /// Augmented-assignment tokens sort before their bare prefixes so
    /// `<<=` never lexes as `<<` followed by `=`.
    fn peek_punct(&self) -> Option<(u16, u32)> {
        const THREE: &[(&[u8], u16)] = &[
            (b"**=", tk::POWEREQ),
            (b"//=", tk::DOUBLE_SLASHEQ),
            (b"<<=", tk::LSHIFT_EQ),
            (b">>=", tk::RSHIFT_EQ),
        ];
        const TWO: &[(&[u8], u16)] = &[
            (b"**", tk::POWER),
            (b"//", tk::DOUBLE_SLASH),
            (b"==", tk::EQ),
            (b"!=", tk::NEQ),
            (b"<=", tk::LE),
            (b">=", tk::GE),
            (b"<<", tk::LSHIFT),
            (b">>", tk::RSHIFT),
            (b"+=", tk::PLUSEQ),
            (b"-=", tk::MINUSEQ),
            (b"*=", tk::STAREQ),
            (b"/=", tk::SLASHEQ),
            (b"%=", tk::PERCENTEQ),
            (b"@=", tk::AT_SIGN_EQ),
            (b"&=", tk::AMP_EQ),
            (b"|=", tk::PIPE_EQ),
            (b"^=", tk::CARET_EQ),
            (b"->", tk::RARROW),
        ];
        for (text, kind) in THREE {
            if self.starts_with(text) {
                return Some((*kind, u32::try_from(text.len()).unwrap_or(u32::MAX)));
            }
        }
        for (text, kind) in TWO {
            if self.starts_with(text) {
                return Some((*kind, u32::try_from(text.len()).unwrap_or(u32::MAX)));
            }
        }
        let one = match self.peek()? {
            b'(' => tk::LPAREN,
            b')' => tk::RPAREN,
            b'[' => tk::LBRACKET,
            b']' => tk::RBRACKET,
            b'{' => tk::LBRACE,
            b'}' => tk::RBRACE,
            b':' => tk::COLON,
            b';' => tk::SEMI,
            b',' => tk::COMMA,
            b'.' => tk::DOT,
            b'!' => tk::BANG,
            b'+' => tk::PLUS,
            b'-' => tk::MINUS,
            b'*' => tk::STAR,
            b'/' => tk::SLASH,
            b'%' => tk::PERCENT,
            b'@' => tk::AT_SIGN,
            b'<' => tk::LT,
            b'>' => tk::GT,
            b'&' => tk::AMP,
            b'|' => tk::PIPE,
            b'^' => tk::CARET,
            b'~' => tk::TILDE,
            b'=' => tk::EQUALS,
            _ => return None,
        };
        Some((one, 1))
    }

    /// One atom: names/keywords-as-atoms, numbers, strings, f-strings,
    /// parenthesized forms, displays, and comprehensions.
    fn parse_atom(&mut self) {
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                let n = self.scan_name_len();
                let text = &self.bytes[self.pos as usize..(self.pos + n) as usize];
                let next = self.at(n);
                if next.is_some_and(|b| b == b'"' || b == b'\'') && Self::is_string_prefix(text) {
                    if Self::prefix_has_f(text) {
                        self.parse_fstring(n);
                    } else {
                        self.parse_plain_string(n);
                        self.parse_string_concat();
                    }
                } else {
                    self.bump(tk::NAME, n);
                }
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
            Some(b'"' | b'\'') => {
                self.parse_plain_string(0);
                self.parse_string_concat();
            }
            Some(b'(') => self.parse_paren(),
            Some(b'[') => self.parse_list(),
            Some(b'{') => self.parse_dict_or_set(),
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }

    /// Adjacent string literals concatenate: sibling STRING tokens
    /// (documented subset — no wrapper node in v0). Newlines between
    /// the literals stay structural (they end the logical line).
    fn parse_string_concat(&mut self) {
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'"' | b'\'') => self.parse_plain_string(0),
                Some(b) if b.is_ascii_alphabetic() => {
                    let n = self.scan_name_len();
                    let next = self.at(n);
                    if n > 0
                        && n <= 2
                        && next.is_some_and(|b| b == b'"' || b == b'\'')
                        && Self::is_string_prefix(
                            &self.bytes[self.pos as usize..(self.pos + n) as usize],
                        )
                    {
                        self.parse_plain_string(n);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    /// Plain string literal (any non-`f` prefix): one STRING token.
    fn parse_plain_string(&mut self, prefix_len: u32) {
        self.pos += prefix_len;
        let Some(quote) = self.peek() else {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        };
        let triple = self.at(1) == Some(quote) && self.at(2) == Some(quote);
        if let Some(n) = self.scan_plain_string_len(quote, triple) {
            // The scan ran from the quote; rewind over the prefix so
            // the STRING token covers prefix + literal.
            self.pos -= prefix_len;
            self.bump(tk::STRING, n + prefix_len);
        } else {
            // Unterminated: error over the rest of the line.
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// f-string: FSTRING node with START/MIDDLE/interpolations/END.
    /// Interpolation expressions use the full Pratt grammar (nested
    /// f-strings recurse); the format spec is raw (documented subset).
    #[allow(clippy::too_many_lines)] // one tokenizer state machine
    fn parse_fstring(&mut self, prefix_len: u32) {
        let node_mark = self.events.len();
        let quote = self.at(prefix_len).unwrap_or(b'"');
        let triple =
            self.at(prefix_len + 1) == Some(quote) && self.at(prefix_len + 2) == Some(quote);
        let q_len: u32 = if triple { 3 } else { 1 };
        self.bump(tk::FSTRING_START, prefix_len + q_len);
        self.events.insert(
            node_mark,
            Event::Start {
                kind: kinds::FSTRING,
                forward_parent: None,
            },
        );
        loop {
            // Literal chunk (`{{`/`}}` stay inside the middle text).
            let chunk = self.pos;
            loop {
                match self.peek() {
                    Some(b'{') if self.at(1) == Some(b'{') => self.pos += 2,
                    Some(b'}') if self.at(1) == Some(b'}') => self.pos += 2,
                    None | Some(b'{' | b'}') => break,
                    Some(b) if b == quote => {
                        if triple {
                            if self.at(1) == Some(quote) && self.at(2) == Some(quote) {
                                break;
                            }
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    Some(b'\n') if !triple => break,
                    Some(_) => self.pos += 1,
                }
            }
            if self.pos > chunk {
                // The scan advanced `pos` past the chunk — rewind so the
                // token range covers it exactly.
                let scanned = self.pos;
                self.pos = chunk;
                self.bump(tk::FSTRING_MIDDLE, scanned - chunk);
            }
            // Closing quote?
            if self.peek() == Some(quote) {
                if triple {
                    if self.at(1) == Some(quote) && self.at(2) == Some(quote) {
                        self.bump(tk::FSTRING_END, 3);
                        self.events.push(Event::Finish);
                        return;
                    }
                } else {
                    self.bump(tk::FSTRING_END, 1);
                    self.events.push(Event::Finish);
                    return;
                }
            }
            // Interpolation?
            if self.peek() == Some(b'{') {
                self.bump(tk::LBRACE, 1);
                self.depth += 1;
                self.skip_trivia();
                self.parse_expr_bp(0);
                self.skip_trivia();
                if self.peek() == Some(b'!') && self.at(1) != Some(b'=') {
                    self.bump(tk::BANG, 1);
                    self.skip_trivia();
                    let n = self.scan_name_len();
                    if n > 0 {
                        self.bump(tk::NAME, n);
                    }
                    self.skip_trivia();
                }
                if self.peek() == Some(b':') {
                    self.bump(tk::COLON, 1);
                    // Raw spec: literal until the matching `}` at this
                    // level; `{{`/`}}` stay literal (documented subset).
                    let spec = self.pos;
                    loop {
                        match self.peek() {
                            Some(b'}') if self.at(1) == Some(b'}') => self.pos += 2,
                            None | Some(b'}') => break,
                            Some(b'{') if self.at(1) == Some(b'{') => self.pos += 2,
                            Some(b'\n') if !triple => break,
                            Some(_) => self.pos += 1,
                        }
                    }
                    if self.pos > spec {
                        // Same rewind discipline as the body chunks.
                        let scanned = self.pos;
                        self.pos = spec;
                        self.bump(tk::FSTRING_MIDDLE, scanned - spec);
                    }
                }
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.depth = self.depth.saturating_sub(1);
                continue;
            }
            // Unterminated (EOF, stray `}`, or a newline in a
            // single-line f-string): one tolerated error; the line
            // machinery takes over from the boundary.
            let start = self.pos;
            self.fail_skip(start, false);
            self.events.push(Event::Finish);
            return;
        }
    }

    /// `(` … `)`: unit, tuple, parenthesized expression, or generator.
    fn parse_paren(&mut self) {
        if self.nest_depth >= MAX_DEPTH {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        }
        self.nest_depth += 1;
        self.parse_paren_guarded();
        self.nest_depth -= 1;
    }

    /// The guarded body of [`Self::parse_paren`] (depth accounting lives
    /// in the wrapper).
    fn parse_paren_guarded(&mut self) {
        let mark = self.events.len();
        self.bump_bracket(tk::LPAREN, 1, true);
        self.skip_trivia();
        if self.peek() == Some(b')') {
            self.bump_bracket(tk::RPAREN, 1, false);
            self.wrap_node(mark, kinds::TUPLE); // ()
            return;
        }
        self.parse_expr_bp(0);
        self.skip_trivia();
        if self.peek_kw("for") {
            self.parse_comp_clauses();
            self.wrap_node(mark, kinds::GENERATOR);
        } else if self.peek() == Some(b',') {
            while self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    break;
                }
                self.parse_expr_bp(0);
                self.skip_trivia();
            }
            self.wrap_node(mark, kinds::TUPLE);
        }
        if self.peek() == Some(b')') {
            self.bump_bracket(tk::RPAREN, 1, false);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// `[` … `]`: list display or list comprehension.
    fn parse_list(&mut self) {
        if self.nest_depth >= MAX_DEPTH {
            let start = self.pos;
            self.fail_skip(start, true);
            return;
        }
        self.nest_depth += 1;
        self.parse_list_guarded();
        self.nest_depth -= 1;
    }

    /// The guarded body of [`Self::parse_list`] (depth accounting lives
    /// in the wrapper).
    fn parse_list_guarded(&mut self) {
        let mark = self.events.len();
        self.bump_bracket(tk::LBRACKET, 1, true);
        self.skip_trivia();
        if self.peek() != Some(b']') && self.peek().is_some() {
            self.parse_expr_bp(0);
            self.skip_trivia();
            if self.peek_kw("for") {
                self.parse_comp_clauses();
            } else {
                while self.peek() == Some(b',') {
                    self.bump(tk::COMMA, 1);
                    self.skip_trivia();
                    if self.peek() == Some(b']') {
                        break;
                    }
                    self.parse_expr_bp(0);
                    self.skip_trivia();
                }
            }
        }
        if self.peek() == Some(b']') {
            self.bump_bracket(tk::RBRACKET, 1, false);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.wrap_node(mark, kinds::LIST);
    }

    /// `{` … `}`: dict (with `k: v` PAIR nodes) or set; both
    /// comprehension forms.
    fn parse_dict_or_set(&mut self) {
        let mark = self.events.len();
        self.bump_bracket(tk::LBRACE, 1, true);
        self.skip_trivia();
        if self.peek() == Some(b'}') {
            self.bump_bracket(tk::RBRACE, 1, false);
            self.wrap_node(mark, kinds::DICT); // {} = empty dict
            return;
        }
        let pair_mark = self.events.len();
        self.parse_expr_bp(0);
        self.skip_trivia();
        let mut is_dict = false;
        if self.peek() == Some(b':') {
            is_dict = true;
            self.bump(tk::COLON, 1);
            self.skip_trivia();
            self.parse_expr_bp(0);
            self.wrap_node(pair_mark, kinds::PAIR);
        }
        self.skip_trivia();
        if self.peek_kw("for") {
            self.parse_comp_clauses();
        } else {
            while self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                if self.peek() == Some(b'}') {
                    break;
                }
                let pair = self.events.len();
                self.parse_expr_bp(0);
                self.skip_trivia();
                if is_dict {
                    if self.peek() == Some(b':') {
                        self.bump(tk::COLON, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(0);
                        self.wrap_node(pair, kinds::PAIR);
                    } else {
                        // A colon-less entry in a dict display: tolerated.
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                }
                self.skip_trivia();
            }
        }
        if self.peek() == Some(b'}') {
            self.bump_bracket(tk::RBRACE, 1, false);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.wrap_node(mark, if is_dict { kinds::DICT } else { kinds::SET });
    }

    /// Comprehension clauses: `for targets in iter (if guard)*` — the
    /// iterable and guards parse at `min_bp == 1` so a trailing `if`
    /// guard is not eaten as a ternary.
    fn parse_comp_clauses(&mut self) {
        while self.peek_kw("for") {
            self.events.push(Event::Start {
                kind: kinds::COMPREHENSION,
                forward_parent: None,
            });
            self.bump_kw("for");
            self.skip_trivia();
            self.parse_target();
            self.skip_trivia();
            if self.peek_kw("in") {
                self.bump_kw("in");
                self.skip_trivia();
                self.parse_expr_bp(1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia();
            while self.peek_kw("if") {
                self.bump_kw("if");
                self.skip_trivia();
                self.parse_expr_bp(1);
                self.skip_trivia();
            }
            self.events.push(Event::Finish);
        }
    }

    /// Assignment targets: postfix-level expressions, comma list wraps
    /// into a TUPLE.
    fn parse_target(&mut self) {
        let mark = self.events.len();
        if self.peek() == Some(b'*') {
            self.bump(tk::STAR, 1);
            self.skip_trivia();
        }
        self.parse_expr_bp(11);
        self.skip_trivia();
        if self.peek() == Some(b',') {
            while self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                if self.peek_kw("in") || self.peek_kw("if") {
                    break;
                }
                if self.peek() == Some(b'*') {
                    self.bump(tk::STAR, 1);
                    self.skip_trivia();
                }
                self.parse_expr_bp(11);
                self.skip_trivia();
            }
            self.wrap_tuple(mark);
        }
    }
}

/// Byte length of an augmented-assignment token.
fn aug_len(op: u16) -> u32 {
    match op {
        tk::POWEREQ | tk::DOUBLE_SLASHEQ | tk::LSHIFT_EQ | tk::RSHIFT_EQ | tk::AT_SIGN_EQ => 3,
        _ => 2,
    }
}

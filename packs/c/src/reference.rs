//! Hand-written reference C11-subset parser on the runtime-core event
//! protocol (issue 034). Brace/semicolon structured — newlines are pure
//! trivia — with a precedence-climbing expression engine, the full
//! declarator grammar, a typedef-name table for the shadowing
//! heuristic, and preprocessor lines as structured trivia.

use std::collections::BTreeSet;
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

/// Parse C-subset source into the raw event stream.
#[allow(clippy::type_complexity)]
#[must_use]
pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize) {
    let mut parser = Parser::run(source);
    parser.parse_unit();
    (parser.events, parser.tokens, parser.errors)
}

/// Parse C-subset source into a green tree (one implicit
/// [`crate::ROOT_KIND`] root).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {
    let mut parser = Parser::run(source);
    parser.parse_unit();
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

/// Type keywords recognised at the head of a declaration.
const TYPE_KEYWORDS: &[&str] = &[
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "const",
    "volatile", "static", "extern", "register", "inline", "restrict", "_Bool", "struct", "union",
    "enum", "typedef",
];

struct Parser<'s> {
    bytes: &'s [u8],
    pos: u32,
    last_end: u32,
    events: Vec<Event>,
    tokens: Vec<(u16, Range<u32>)>,
    trivia: Vec<(Range<u32>, u16, bool)>,
    errors: usize,
    /// Names introduced by `typedef` — the shadowing heuristic.
    typedefs: BTreeSet<String>,
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
            typedefs: BTreeSet::new(),
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
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'_'),
        }
    }

    fn bump_kw(&mut self, kw: &str) {
        self.bump(tk::NAME, u32::try_from(kw.len()).unwrap_or(u32::MAX));
    }

    /// Whether an identifier at the cursor is a known typedef name.
    fn peek_typedef(&self) -> bool {
        let n = self.scan_name_len();
        if n == 0 {
            return false;
        }
        let text = &self.bytes[self.pos as usize..(self.pos + n) as usize];
        // Boundaries checked by the caller via peek_kw-like logic.
        self.typedefs
            .contains(&String::from_utf8_lossy(text).into_owned())
    }

    /// Whether the cursor starts a type (type keywords, struct/union/
    /// enum specifiers, or a typedef name).
    fn peek_is_type(&self) -> bool {
        for kw in TYPE_KEYWORDS {
            if self.peek_kw(kw) {
                return true;
            }
        }
        self.peek_typedef()
    }

    fn skip_trivia(&mut self) {
        let mut start = self.pos;
        let mut kind = tk::WHITESPACE;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') => self.pos += 1,
                Some(b'\\') if self.at(1) == Some(b'\n') => self.pos += 2,
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
    // Scanner
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

    fn scan_number_len(&self) -> u32 {
        let start = self.pos as usize;
        let mut idx = start;
        if idx + 1 < self.bytes.len() && self.bytes[idx] == b'0' {
            match self.bytes[idx + 1] {
                b'x' | b'X' => {
                    idx += 2;
                    while idx < self.bytes.len() && (self.bytes[idx].is_ascii_alphanumeric()) {
                        idx += 1;
                    }
                    return u32::try_from(idx - start).unwrap_or(1);
                }
                _ => {}
            }
        }
        while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx < self.bytes.len() && self.bytes[idx] == b'.' {
            idx += 1;
            while idx < self.bytes.len() && self.bytes[idx].is_ascii_digit() {
                idx += 1;
            }
        }
        if idx < self.bytes.len() && matches!(self.bytes[idx], b'e' | b'E' | b'p' | b'P') {
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
        // Suffixes: u/U/l/L combos + f/F.
        while idx < self.bytes.len()
            && matches!(self.bytes[idx], b'u' | b'U' | b'l' | b'L' | b'f' | b'F')
        {
            idx += 1;
        }
        u32::try_from(idx - start).unwrap_or(1)
    }

    /// `"…"` with escapes; `None` = unterminated.
    fn scan_string_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        loop {
            match self.bytes.get(idx)? {
                b'\\' => idx += 2,
                b'\n' => return None,
                b'"' => return u32::try_from(idx + 1 - start).ok(),
                _ => idx += 1,
            }
        }
    }

    /// `'…'` with escapes; `None` = unterminated.
    fn scan_char_len(&self) -> Option<u32> {
        let start = self.pos as usize;
        let mut idx = start + 1;
        loop {
            match self.bytes.get(idx)? {
                b'\\' => idx += 2,
                b'\n' => return None,
                b'\'' => return u32::try_from(idx + 1 - start).ok(),
                _ => idx += 1,
            }
        }
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

    fn bump_string(&mut self) {
        if let Some(len) = self.scan_string_len() {
            self.bump(tk::STRING, len);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    // ------------------------------------------------------------------
    // Translation unit
    // ------------------------------------------------------------------

    fn parse_unit(&mut self) {
        while self.peek().is_some() {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }
            // Preprocessor line: `#` to the (continuation-aware) newline.
            if self.peek() == Some(b'#') {
                self.parse_preproc();
                continue;
            }
            if self.peek() == Some(b'}') {
                self.force_byte();
                continue;
            }
            let before = self.pos;
            self.parse_external();
            if self.pos == before && self.peek().is_some() {
                self.force_byte();
            }
        }
    }

    /// `# … \n` — one PREPROC node (structured trivia for the grammar).
    fn parse_preproc(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::PREPROC,
            forward_parent: None,
        });
        self.bump(tk::HASH, 1);
        loop {
            // Spaces/tabs only — the newline terminates the line; the
            // runs ride whitespace trivia so the render stays lossless.
            let ws_start = self.pos;
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
                self.pos += 1;
            }
            if self.pos > ws_start {
                self.push_trivia(ws_start, tk::WHITESPACE, false);
            }
            match self.peek() {
                None | Some(b'\n') => break,
                Some(b'\\') if self.at(1) == Some(b'\n') => {
                    // Line continuation: the backslash-newline is trivia.
                    let start = self.pos;
                    self.pos += 2;
                    self.push_trivia(start, tk::WHITESPACE, false);
                }
                _ => {
                    self.bump_misc();
                }
            }
        }
        // Consume the terminating newline as whitespace trivia.
        if self.peek() == Some(b'\n') {
            let start = self.pos;
            self.pos += 1;
            self.push_trivia(start, tk::WHITESPACE, false);
        }
        self.events.push(Event::Finish);
    }

    /// One external definition or global declaration.
    fn parse_external(&mut self) {
        self.skip_trivia();
        if self.peek_kw("typedef") {
            self.parse_typedef();
            return;
        }
        if !self.peek_is_type() {
            // Stray statement at file scope (recovered).
            self.parse_stmt();
            self.skip_semi();
            return;
        }
        let decl_start = self.events.len();
        // Base type + qualifiers/specifiers.
        self.parse_base_type();
        self.skip_trivia();
        // A bare `struct S { … };` specifier declaration.
        if matches!(self.peek(), Some(b';')) {
            self.wrap_node_at(decl_start, kinds::DECLARATION);
            self.bump(tk::SEMI, 1);
            self.events.push(Event::Finish);
            return;
        }
        // First declarator.
        self.parse_declarator();
        self.skip_trivia();
        // Function definition vs declaration: `{` after the params.
        if self.peek() == Some(b'{') {
            self.wrap_node_at(decl_start, kinds::FUNCTION);
            self.parse_block();
            self.events.push(Event::Finish);
            return;
        }
        // Initialiser.
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(1);
            self.skip_trivia();
        }
        loop {
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                self.parse_declarator();
                self.skip_trivia();
                if self.peek() == Some(b'=') {
                    self.bump(tk::ASSIGN, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
                    self.skip_trivia();
                }
                continue;
            }
            break;
        }
        self.wrap_node_at(decl_start, kinds::DECLARATION);
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// Wrap retroactively from `decl_start` (the precede-insert trick
    /// cannot move backwards, so insert directly).
    fn wrap_node_at(&mut self, start: usize, kind: u16) {
        self.events.insert(
            start,
            Event::Start {
                kind,
                forward_parent: None,
            },
        );
    }

    /// `typedef <type> <declarator>;` — record the name.
    fn parse_typedef(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::TYPEDEF,
            forward_parent: None,
        });
        self.bump_kw("typedef");
        self.skip_trivia();
        self.parse_base_type();
        self.skip_trivia();
        // The declared name: strip pointer stars.
        while self.peek() == Some(b'*') {
            self.bump(tk::STAR, 1);
            self.skip_trivia();
        }
        let n = self.scan_name_len();
        if n > 0 {
            let name =
                String::from_utf8_lossy(&self.bytes[self.pos as usize..(self.pos + n) as usize])
                    .into_owned();
            self.typedefs.insert(name);
            self.bump(tk::NAME, n);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.skip_trivia();
        // Optional array/param suffix on the typedef.
        if self.peek() == Some(b'[') {
            self.bump(tk::LBRACKET, 1);
            self.skip_balanced_close(tk::RBRACKET, tk::LBRACKET);
        } else if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            self.skip_balanced_close(tk::RPAREN, tk::LPAREN);
        }
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
        self.events.push(Event::Finish);
    }

    /// Skip to and consume the matching closer (token-tree style).
    fn skip_balanced_close(&mut self, close: u16, open: u16) {
        let mut depth = 1_u32;
        while let Some(b) = self.peek() {
            if b == delimiter_byte(close) {
                self.bump(close, 1);
                depth -= 1;
                if depth == 0 {
                    return;
                }
            } else if b == delimiter_byte(open) {
                self.bump(open, 1);
                depth += 1;
            } else if b == b'"' {
                self.bump_string();
            } else if b == b'\'' {
                match self.scan_char_len() {
                    Some(len) => self.bump(tk::CHAR, len),
                    None => {
                        self.pos += 1;
                    }
                }
            } else {
                self.bump_misc();
            }
        }
    }

    /// Base type: keywords + optional struct/union/enum specifier.
    fn parse_base_type(&mut self) {
        if self.peek_kw("struct") || self.peek_kw("union") || self.peek_kw("enum") {
            let kw = if self.peek_kw("struct") {
                "struct"
            } else if self.peek_kw("union") {
                "union"
            } else {
                "enum"
            };
            self.events.push(Event::Start {
                kind: kinds::RECORD,
                forward_parent: None,
            });
            self.bump_kw(kw);
            self.skip_trivia();
            self.bump_name();
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
                        kind: kinds::MEMBER,
                        forward_parent: None,
                    });
                    // Enum constants carry no type; struct fields do.
                    if self.peek_is_type() {
                        self.parse_base_type();
                        self.skip_trivia();
                        self.parse_declarator();
                    } else {
                        self.bump_name();
                    }
                    self.skip_trivia();
                    if self.peek() == Some(b'=') {
                        self.bump(tk::ASSIGN, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(1);
                        self.skip_trivia();
                    }
                    self.events.push(Event::Finish);
                    self.skip_trivia();
                    // Struct fields end with `;`; enum constants with
                    // `,` — both continue the member list (the loop
                    // head opens the next MEMBER node).
                    if self.peek() == Some(b';') {
                        self.bump(tk::SEMI, 1);
                        self.skip_trivia();
                        if self.peek() == Some(b'}') || self.peek().is_none() {
                            break;
                        }
                        // rewind: the loop head re-opens the MEMBER node
                        let body_start = self.events.len();
                        self.events.truncate(body_start);
                        continue;
                    }
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
            self.events.push(Event::Finish);
            return;
        }
        // Keyword soup: `unsigned long int` etc.
        let mut any = false;
        loop {
            let mut matched = false;
            for kw in TYPE_KEYWORDS {
                if self.peek_kw(kw) && *kw != "typedef" {
                    self.bump_kw(kw);
                    self.skip_trivia();
                    any = true;
                    matched = true;
                    break;
                }
            }
            if !matched {
                break;
            }
        }
        if !any && self.peek_typedef() {
            self.bump_name();
            self.skip_trivia();
        }
    }

    /// One declarator: `* const name[N] (params)` — parenthesised
    /// function-pointer declarators recurse (`(*f)(void)`).
    fn parse_declarator(&mut self) {
        self.skip_trivia();
        let mark = self.events.len();
        // Pointers + qualifiers.
        while self.peek() == Some(b'*') {
            self.bump(tk::STAR, 1);
            self.skip_trivia();
            while self.peek_kw("const") || self.peek_kw("volatile") || self.peek_kw("restrict") {
                self.bump_kw(if self.peek_kw("const") {
                    "const"
                } else if self.peek_kw("volatile") {
                    "volatile"
                } else {
                    "restrict"
                });
                self.skip_trivia();
            }
        }
        if self.peek() == Some(b'(') {
            // `(*f)(…)` / `(*f)[N]` — a grouped inner declarator.
            self.events.push(Event::Start {
                kind: kinds::DECLARATOR,
                forward_parent: None,
            });
            self.bump(tk::LPAREN, 1);
            self.skip_trivia();
            self.parse_declarator();
            self.skip_trivia();
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            // Trailing params/array bind to the group.
            self.skip_trivia();
            if self.peek() == Some(b'(') {
                self.bump(tk::LPAREN, 1);
                self.parse_params();
            } else if self.peek() == Some(b'[') {
                self.bump(tk::LBRACKET, 1);
                self.skip_array_tail();
            }
            self.events.push(Event::Finish);
            return;
        }
        // Direct declarator: name [N] (params) — abstract declarators
        // (`char *` in a param list) end after the stars.
        if self.name_ahead() {
            self.bump_name();
        } else if self.peek() == Some(b')') || self.peek() == Some(b',') {
            return;
        } else {
            self.bump_name();
        }
        self.skip_trivia();
        loop {
            match self.peek() {
                Some(b'[') => {
                    self.events.push(Event::Start {
                        kind: kinds::DECLARATOR,
                        forward_parent: None,
                    });
                    self.bump(tk::LBRACKET, 1);
                    self.skip_trivia();
                    if self.peek() != Some(b']') && self.peek().is_some() {
                        self.parse_expr_bp(1);
                        self.skip_trivia();
                    }
                    if self.peek() == Some(b']') {
                        self.bump(tk::RBRACKET, 1);
                    } else {
                        let start = self.pos;
                        self.fail_skip(start, true);
                    }
                    self.events.push(Event::Finish);
                    self.skip_trivia();
                }
                Some(b'(') => {
                    self.bump(tk::LPAREN, 1);
                    self.parse_params();
                    self.skip_trivia();
                }
                _ => break,
            }
        }
        let _ = mark;
    }

    fn skip_array_tail(&mut self) {
        self.skip_trivia();
        if self.peek() != Some(b']') && self.peek().is_some() {
            self.parse_expr_bp(1);
            self.skip_trivia();
        }
        if self.peek() == Some(b']') {
            self.bump(tk::RBRACKET, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    /// Parameter list: `void`, `type name`, `…`.
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
                self.bump(tk::ELLIPSIS, 3);
                self.skip_trivia();
                if self.peek() == Some(b')') {
                    self.bump(tk::RPAREN, 1);
                }
                self.events.push(Event::Finish);
                return;
            }
            if self.peek_is_type() {
                self.parse_base_type();
                self.skip_trivia();
                // An abstract declarator (`int`, `char *`) has no name:
                // a declarator only follows when a name/`(`/`*` does.
                if matches!(self.peek(), Some(b'*')) || self.name_ahead() {
                    self.parse_declarator();
                }
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
        self.events.push(Event::Finish);
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn parse_stmt(&mut self) {
        self.skip_trivia();
        match self.peek() {
            Some(b'{') => self.parse_block(),
            Some(b';') => {
                self.bump(tk::SEMI, 1);
            }
            None => {}
            _ => {
                if self.peek_kw("if") {
                    self.parse_if();
                } else if self.peek_kw("while") {
                    self.events.push(Event::Start {
                        kind: kinds::WHILE,
                        forward_parent: None,
                    });
                    self.bump_kw("while");
                    self.skip_trivia();
                    self.paren_expr();
                    self.parse_stmt();
                    self.events.push(Event::Finish);
                } else if self.peek_kw("do") {
                    self.events.push(Event::Start {
                        kind: kinds::WHILE,
                        forward_parent: None,
                    });
                    self.bump_kw("do");
                    self.parse_stmt();
                    self.skip_trivia();
                    if self.peek_kw("while") {
                        self.bump_kw("while");
                        self.skip_trivia();
                        self.paren_expr();
                    }
                    self.skip_semi();
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
                    if self.peek() != Some(b';') && self.peek().is_some() {
                        self.parse_expr_bp(1);
                    }
                    self.skip_semi();
                    self.events.push(Event::Finish);
                } else if self.peek_kw("break") || self.peek_kw("continue") {
                    self.events.push(Event::Start {
                        kind: kinds::JUMP,
                        forward_parent: None,
                    });
                    self.bump_kw(if self.peek_kw("break") {
                        "break"
                    } else {
                        "continue"
                    });
                    self.skip_semi();
                    self.events.push(Event::Finish);
                } else if self.peek_is_type() {
                    // A local declaration.
                    self.parse_local_decl();
                } else {
                    self.parse_expr_stmt();
                }
            }
        }
    }

    /// A local declaration: base type + declarators + inits + `;`.
    fn parse_local_decl(&mut self) {
        let decl_start = self.events.len();
        if self.peek_kw("typedef") {
            self.parse_typedef();
            return;
        }
        self.parse_base_type();
        self.skip_trivia();
        self.parse_declarator();
        self.skip_trivia();
        if self.peek() == Some(b'=') {
            self.bump(tk::ASSIGN, 1);
            self.skip_trivia();
            self.parse_expr_bp(1);
            self.skip_trivia();
        }
        loop {
            if self.peek() == Some(b',') {
                self.bump(tk::COMMA, 1);
                self.skip_trivia();
                self.parse_declarator();
                self.skip_trivia();
                if self.peek() == Some(b'=') {
                    self.bump(tk::ASSIGN, 1);
                    self.skip_trivia();
                    self.parse_expr_bp(1);
                    self.skip_trivia();
                }
                continue;
            }
            break;
        }
        self.events.insert(
            decl_start,
            Event::Start {
                kind: kinds::DECLARATION,
                forward_parent: None,
            },
        );
        self.events.push(Event::Finish);
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        } else {
            let start = self.pos;
            self.fail_skip(start, true);
        }
    }

    fn parse_if(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::IF,
            forward_parent: None,
        });
        self.bump_kw("if");
        self.skip_trivia();
        self.paren_expr();
        self.parse_stmt();
        self.skip_trivia();
        if self.peek_kw("else") {
            self.events.push(Event::Start {
                kind: kinds::ELSE,
                forward_parent: None,
            });
            self.bump_kw("else");
            if self.peek_kw("if") {
                self.parse_if();
            } else {
                self.parse_stmt();
            }
            self.events.push(Event::Finish);
        }
        self.events.push(Event::Finish);
    }

    fn parse_for(&mut self) {
        self.events.push(Event::Start {
            kind: kinds::FOR,
            forward_parent: None,
        });
        self.bump_kw("for");
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.bump(tk::LPAREN, 1);
            // Init: a declaration or expression or empty.
            self.skip_trivia();
            if self.peek() != Some(b';') {
                if self.peek_is_type() {
                    self.parse_base_type();
                    self.skip_trivia();
                    self.parse_declarator();
                    self.skip_trivia();
                    if self.peek() == Some(b'=') {
                        self.bump(tk::ASSIGN, 1);
                        self.skip_trivia();
                        self.parse_expr_bp(1);
                    }
                } else {
                    self.parse_expr_bp(1);
                }
            }
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            // Cond.
            self.skip_trivia();
            if self.peek() != Some(b';') {
                self.parse_expr_bp(1);
            }
            self.skip_trivia();
            if self.peek() == Some(b';') {
                self.bump(tk::SEMI, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            // Step.
            self.skip_trivia();
            if self.peek() != Some(b')') {
                self.parse_expr_bp(1);
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
        self.parse_stmt();
        self.events.push(Event::Finish);
    }

    fn paren_expr(&mut self) {
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
                    Some(b'#') => {
                        self.parse_preproc();
                    }
                    _ => {}
                }
                if self.peek() == Some(b'}') || self.peek().is_none() {
                    break;
                }
                let before = self.pos;
                self.parse_stmt();
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

    fn skip_semi(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump(tk::SEMI, 1);
        }
    }

    fn parse_expr_stmt(&mut self) {
        let mark = self.events.len();
        self.skip_trivia();
        self.parse_expr_bp(0);
        self.events.insert(
            mark,
            Event::Start {
                kind: kinds::EXPR_STMT,
                forward_parent: None,
            },
        );
        self.events.push(Event::Finish);
        self.skip_semi();
    }

    // ------------------------------------------------------------------
    // Expressions — precedence climbing over the C11 table
    // ------------------------------------------------------------------

    fn parse_expr_bp(&mut self, min_bp: u8) {
        let mark = self.events.len();
        // Cast: `( Type ) expr` — only when the parens close on a type.
        if self.peek() == Some(b'(') && self.cast_ahead() {
            self.events.push(Event::Start {
                kind: kinds::CAST,
                forward_parent: None,
            });
            self.bump(tk::LPAREN, 1);
            self.skip_trivia();
            self.parse_base_type();
            self.skip_trivia();
            while self.peek() == Some(b'*') {
                self.bump(tk::STAR, 1);
                self.skip_trivia();
            }
            if self.peek() == Some(b')') {
                self.bump(tk::RPAREN, 1);
            } else {
                let start = self.pos;
                self.fail_skip(start, true);
            }
            self.skip_trivia();
            self.parse_expr_bp(11); // casts bind like unary
            self.events.push(Event::Finish);
        } else if let Some((kind, n, rbp)) = self.peek_unary() {
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
        // Ternary at the top level.
        // Ternary parses at init depth too (`int t = a ? b : c;`).
        if min_bp <= 1 && self.peek() == Some(b'?') {
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

    /// The cast heuristic: `( … )` whose inside parses as a bare type
    /// (keywords/typedef + stars, then `)`) — NOT a parenthesised expr.
    fn cast_ahead(&self) -> bool {
        const _REACHABLE: bool = true;
        if self.peek() != Some(b'(') {
            return false;
        }
        let mut idx = self.pos as usize + 1;
        // skip ws
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            idx += 1;
        }
        // A type keyword or a typedef name must start.
        let sub = Parser {
            bytes: self.bytes,
            pos: u32::try_from(idx).unwrap_or(u32::MAX),
            last_end: 0,
            events: Vec::new(),
            tokens: Vec::new(),
            trivia: Vec::new(),
            errors: 0,
            typedefs: self.typedefs.clone(),
        };
        if !sub.peek_is_type() {
            return false;
        }
        // Scan to the matching `)` — only type keywords, names, stars,
        // and ws may appear.
        let mut depth = 1_u32;
        while idx < self.bytes.len() {
            match self.bytes[idx] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        // The next byte decides: an expression follows.
                        return true;
                    }
                }
                b'"' | b'\'' | b';' | b'{' => return false,
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// Prefix unary: `*`, `&`, `-`, `+`, `!`, `~`, `++`, `--`, `sizeof`.
    fn peek_unary(&self) -> Option<(u16, u32, u8)> {
        match self.peek() {
            Some(b'-') if !self.starts_with(b"--") && !self.starts_with(b"-=") => {
                Some((tk::MINUS, 1, 12))
            }
            Some(b'+') if !self.starts_with(b"++") && !self.starts_with(b"+=") => {
                Some((tk::PLUS, 1, 12))
            }
            Some(b'!') if !self.starts_with(b"!=") => Some((tk::BANG, 1, 12)),
            Some(b'~') => Some((tk::TILDE, 1, 12)),
            Some(b'*') if !self.starts_with(b"*=") => Some((tk::STAR, 1, 12)),
            Some(b'&') if !self.starts_with(b"&&") && !self.starts_with(b"&=") => {
                Some((tk::AMP, 1, 12))
            }
            Some(b'+') if self.starts_with(b"++") => Some((tk::INCREMENT, 2, 13)),
            Some(b'-') if self.starts_with(b"--") => Some((tk::DECREMENT, 2, 13)),
            Some(_) if self.peek_kw("sizeof") => Some((tk::NAME, 6, 12)),
            _ => None,
        }
    }

    /// Postfix: calls, index, `.member`, `->member`, `++`/`--`.
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
                    self.wrap_node(mark, kinds::MEMBER_ACCESS);
                }
                Some(b'-') if self.starts_with(b"->") => {
                    self.bump(tk::ARROW, 2);
                    self.skip_trivia();
                    self.bump_name();
                    self.wrap_node(mark, kinds::MEMBER_ACCESS);
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

    /// The C11 binary table + assignment (right-assoc, lowest-but-comma).
    fn parse_binops(&mut self, mark: usize, min_bp: u8) {
        while let Some((lbp, rbp, kind)) = self.peek_binop() {
            if lbp < min_bp {
                break;
            }
            self.bump(kind, self.punct_len());
            self.skip_trivia();
            self.parse_expr_bp(rbp);
            let node = if Self::is_assign_op(kind) {
                kinds::ASSIGN
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
                | tk::AMP_EQ
                | tk::PIPE_EQ
                | tk::CARET_EQ
                | tk::LSHIFT_EQ
                | tk::RSHIFT_EQ
        )
    }

    fn peek_binop(&self) -> Option<(u8, u8, u16)> {
        let (kind, _) = self.peek_punct()?;
        let (lbp, rbp) = match kind {
            tk::OR => (2_u8, 3_u8),
            tk::AND => (3, 4),
            tk::PIPE => (4, 5),
            tk::CARET => (5, 6),
            tk::AMP => (6, 7),
            tk::EQ | tk::NEQ => (7, 8),
            tk::LT | tk::GT | tk::LE | tk::GE => (8, 9),
            tk::LSHIFT | tk::RSHIFT => (9, 10),
            tk::PLUS | tk::MINUS => (10, 11),
            tk::STAR | tk::SLASH | tk::PERCENT => (12, 13),
            tk::ASSIGN
            | tk::PLUSEQ
            | tk::MINUSEQ
            | tk::STAREQ
            | tk::SLASHEQ
            | tk::PERCENTEQ
            | tk::AMP_EQ
            | tk::PIPE_EQ
            | tk::CARET_EQ
            | tk::LSHIFT_EQ
            | tk::RSHIFT_EQ => (1, 1),
            _ => return None,
        };
        Some((lbp, rbp, kind))
    }

    fn punct_len(&self) -> u32 {
        const THREE: &[&[u8]] = &[b"<<=", b">>=", b"..."];
        const TWO: &[&[u8]] = &[
            b"==", b"!=", b"<=", b">=", b"&&", b"||", b"<<", b">>", b"++", b"--", b"+=", b"-=",
            b"*=", b"/=", b"%=", b"&=", b"|=", b"^=", b"->",
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
        let start = usize::try_from(self.pos).unwrap_or(0);
        let bytes = self.bytes.get(start..start + len as usize)?;
        let kind = match bytes {
            b"<<=" => tk::LSHIFT_EQ,
            b">>=" => tk::RSHIFT_EQ,
            b"==" => tk::EQ,
            b"!=" => tk::NEQ,
            b"<=" => tk::LE,
            b">=" => tk::GE,
            b"&&" => tk::AND,
            b"||" => tk::OR,
            b"<<" => tk::LSHIFT,
            b">>" => tk::RSHIFT,
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
            b"->" => tk::ARROW,
            b"{" => tk::LBRACE,
            b"}" => tk::RBRACE,
            b"(" => tk::LPAREN,
            b")" => tk::RPAREN,
            b"[" => tk::LBRACKET,
            b"]" => tk::RBRACKET,
            b";" => tk::SEMI,
            b"," => tk::COMMA,
            b":" => tk::COLON,
            b"." => tk::DOT,
            b"?" => tk::QUESTION,
            b"+" => tk::PLUS,
            b"-" => tk::MINUS,
            b"*" => tk::STAR,
            b"/" => tk::SLASH,
            b"%" => tk::PERCENT,
            b"!" => tk::BANG,
            b"&" => tk::AMP,
            b"|" => tk::PIPE,
            b"^" => tk::CARET,
            b"~" => tk::TILDE,
            b"<" => tk::LT,
            b">" => tk::GT,
            b"=" => tk::ASSIGN,
            _ => return None,
        };
        Some((kind, len))
    }

    /// One atom: literals, names (+ call/index postfix come later),
    /// parenthesised expressions, sizeof(type).
    #[allow(clippy::too_many_lines)] // one atom dispatcher
    fn parse_atom(&mut self) {
        match self.peek() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                if self.peek_kw("sizeof") {
                    // `sizeof(expr)` / `sizeof(type)`.
                    let mark = self.events.len();
                    self.bump_kw("sizeof");
                    self.skip_trivia();
                    if self.peek() == Some(b'(') {
                        self.bump(tk::LPAREN, 1);
                        self.skip_trivia();
                        if self.peek_is_type() {
                            self.events.push(Event::Start {
                                kind: kinds::TYPE_EXPR,
                                forward_parent: None,
                            });
                            self.parse_base_type();
                            self.skip_trivia();
                            while self.peek() == Some(b'*') {
                                self.bump(tk::STAR, 1);
                                self.skip_trivia();
                            }
                            self.events.push(Event::Finish);
                        } else {
                            self.parse_expr_bp(0);
                        }
                        self.skip_trivia();
                        if self.peek() == Some(b')') {
                            self.bump(tk::RPAREN, 1);
                        } else {
                            let start = self.pos;
                            self.fail_skip(start, true);
                        }
                        self.wrap_node(mark, kinds::TYPE_EXPR);
                        return;
                    }
                    return;
                }
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
            Some(b'"') => self.bump_string(),
            Some(b'\'') => {
                if let Some(len) = self.scan_char_len() {
                    self.bump(tk::CHAR, len);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
            }
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
            Some(b'{') => {
                // Brace initialiser list `{ a, b, c }` — a TUPLE node.
                let mark = self.events.len();
                self.bump(tk::LBRACE, 1);
                loop {
                    self.skip_trivia();
                    match self.peek() {
                        Some(b'}') | None => break,
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
                if self.peek() == Some(b'}') {
                    self.bump(tk::RBRACE, 1);
                } else {
                    let start = self.pos;
                    self.fail_skip(start, true);
                }
                self.wrap_node(mark, kinds::TYPE_EXPR); // brace init group
            }
            _ => {
                let start = self.pos;
                self.fail_skip(start, true);
            }
        }
    }
}

impl Parser<'_> {
    /// Whether a plain identifier starts at the cursor (after trivia).
    fn name_ahead(&self) -> bool {
        let mut idx = self.pos as usize;
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            idx += 1;
        }
        self.bytes
            .get(idx)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    }

    /// Bump one misc token: the longest punct match or a name/number.
    fn bump_misc(&mut self) {
        self.skip_trivia();
        if let Some(n) = {
            let n = self.scan_name_len();
            if n > 0 { Some(n) } else { None }
        } {
            self.bump(tk::NAME, n);
            return;
        }
        let (kind, len) = self.peek_punct().unwrap_or((tk::BANG, 1));
        self.bump(kind, len);
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

//! The Rust scanner (issue 033): identifiers/keywords, literals with
//! escapes, **lifetime vs char-literal disambiguation**, nested block
//! comments, doc comments as trivia, and the two-to-three-char punct
//! surface.
//!
//! Output: one `(kind, range)` feed for the parser (clean — trivia
//! rides the separate trivia table) plus `(range, trivia kind,
//! trailing)` records covering every gap.

use crate::token_kinds as tk;

/// One scanner token (kind + clean source range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanToken {
    /// The token kind.
    pub kind: u16,
    /// The clean byte range.
    pub range: std::ops::Range<u32>,
}

/// The scan result: the token feed plus trivia records.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    /// Clean tokens in source order.
    pub tokens: Vec<ScanToken>,
    /// `(range, trivia kind, trailing)` covering every gap.
    pub trivia: Vec<(std::ops::Range<u32>, u16, bool)>,
    /// Unterminated literals / stray characters (error regions).
    pub errors: usize,
}

/// The unmatched-character error kind (scanner-local sentinel).
const BAD: u16 = 0xFFFF - 1;

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan `source` into the feed + trivia.
#[must_use]
#[allow(clippy::too_many_lines)] // the dispatch table is the grammar data
pub fn scan(source: &str) -> Scan {
    let bytes = source.as_bytes();
    let mut scan = Scan::default();
    let mut pos = 0_usize;
    let mut line_has_token = false;
    let end = u32::try_from(source.len()).unwrap_or(u32::MAX);

    while pos < bytes.len() {
        let start = pos;
        let b = bytes[pos];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => {
                while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n') {
                    pos += 1;
                }
                let range = span(start, pos);
                let trailing = line_has_token;
                scan.trivia.push((range, tk::WHITESPACE, trailing));
                if bytes[start] == b'\n' {
                    line_has_token = false;
                }
            }
            b'/' if pos + 1 < bytes.len() && bytes[pos + 1] == b'/' => {
                let doc = pos + 2 < bytes.len() && matches!(bytes[pos + 2], b'/' | b'!');
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                let kind = if doc { tk::DOC_COMMENT } else { tk::COMMENT };
                let range = span(start, pos);
                scan.trivia.push((range, kind, line_has_token));
            }
            b'/' if pos + 1 < bytes.len() && bytes[pos + 1] == b'*' => {
                let doc = pos + 2 < bytes.len()
                    && (bytes[pos + 2] == b'*' || bytes[pos + 2] == b'!')
                    && !(pos + 3 < bytes.len() && bytes[pos + 3] == b'/');
                pos += 2;
                let mut depth = 1_usize;
                while pos < bytes.len() && depth > 0 {
                    if bytes[pos] == b'/' && pos + 1 < bytes.len() && bytes[pos + 1] == b'*' {
                        depth += 1;
                        pos += 2;
                    } else if bytes[pos] == b'*' && pos + 1 < bytes.len() && bytes[pos + 1] == b'/'
                    {
                        depth -= 1;
                        pos += 2;
                    } else {
                        pos += 1;
                    }
                }
                if depth > 0 {
                    scan.errors += 1; // unterminated block comment
                }
                let kind = if doc { tk::DOC_COMMENT } else { tk::COMMENT };
                let range = span(start, pos);
                scan.trivia.push((range, kind, line_has_token));
            }
            b'"' => scan_string(&mut scan, bytes, &mut pos),
            b'\'' => scan_quote(&mut scan, bytes, &mut pos),
            _ if b.is_ascii_digit() => scan_number(&mut scan, bytes, &mut pos),
            _ if is_ident_start(b) => {
                while pos < bytes.len() && is_ident_cont(bytes[pos]) {
                    pos += 1;
                }
                let kind = keyword(&source[start..pos]);
                push(&mut scan, kind, span(start, pos));
                line_has_token = true;
            }
            _ => scan_punct(&mut scan, bytes, &mut pos, &mut line_has_token),
        }
    }
    let _ = end;
    scan
}

fn span(start: usize, end: usize) -> std::ops::Range<u32> {
    let s = u32::try_from(start).unwrap_or(u32::MAX);
    let e = u32::try_from(end).unwrap_or(u32::MAX);
    s..e.max(s)
}

fn push(scan: &mut Scan, kind: u16, range: std::ops::Range<u32>) {
    scan.tokens.push(ScanToken { kind, range });
}

/// `"…"` with `\` escapes; unterminated strings are an error region.
fn scan_string(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    *pos += 1; // opening quote
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\\' => *pos += 2,
            b'"' => {
                *pos += 1;
                push(scan, tk::STRING_LIT, span(start, *pos));
                return;
            }
            b'\n' => break, // unterminated: stop at EOL
            _ => *pos += 1,
        }
    }
    scan.errors += 1;
    push(scan, BAD, span(start, *pos));
}

/// The quote disambiguation (issue 033 TDD): `'x'`-shaped single-char
/// runs (and `'\…'` escapes) are char literals, everything else is a
/// lifetime.
fn scan_quote(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    *pos += 1; // the quote
    // Escaped char literal: `'\n'`, `'\u{…}'`.
    if *pos < bytes.len() && bytes[*pos] == b'\\' {
        *pos += 1;
        if *pos < bytes.len() && bytes[*pos] == b'u' {
            while *pos < bytes.len() && bytes[*pos] != b'}' && bytes[*pos] != b'\n' {
                *pos += 1;
            }
            if *pos < bytes.len() && bytes[*pos] == b'}' {
                *pos += 1;
            }
        } else {
            *pos += 1;
        }
        if *pos < bytes.len() && bytes[*pos] == b'\'' {
            *pos += 1;
            push(scan, tk::CHAR_LIT, span(start, *pos));
        } else {
            scan.errors += 1;
            push(scan, BAD, span(start, *pos));
        }
        return;
    }
    // Ident run after the quote.
    let run_start = *pos;
    while *pos < bytes.len() && is_ident_cont(bytes[*pos]) {
        *pos += 1;
    }
    let run_len = *pos - run_start;
    if run_len == 1 && *pos < bytes.len() && bytes[*pos] == b'\'' {
        *pos += 1;
        push(scan, tk::CHAR_LIT, span(start, *pos));
    } else if run_len > 0 {
        // Lifetime: `'a`, `'static` (closing quote would make it a
        // multi-char char literal — invalid, so it is a lifetime).
        push(scan, tk::LIFETIME, span(start, *pos));
    } else {
        // Bare quote: stray.
        scan.errors += 1;
        push(scan, BAD, span(start, *pos));
    }
}

/// Numeric literals: `1_000`, `1.5`, `1e3`, suffixes (`u32`, `f64`).
fn scan_number(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    let mut is_float = false;
    while *pos < bytes.len() && (bytes[*pos].is_ascii_digit() || bytes[*pos] == b'_') {
        *pos += 1;
    }
    // Fraction: only when `.` is followed by a digit (`1..2` stays ints).
    if *pos + 1 < bytes.len() && bytes[*pos] == b'.' && bytes[*pos + 1].is_ascii_digit() {
        is_float = true;
        *pos += 1;
        while *pos < bytes.len() && (bytes[*pos].is_ascii_digit() || bytes[*pos] == b'_') {
            *pos += 1;
        }
    }
    if *pos < bytes.len() && matches!(bytes[*pos], b'e' | b'E') {
        let mut probe = *pos + 1;
        if probe < bytes.len() && matches!(bytes[probe], b'+' | b'-') {
            probe += 1;
        }
        if probe < bytes.len() && bytes[probe].is_ascii_digit() {
            is_float = true;
            *pos = probe;
            while *pos < bytes.len() && (bytes[*pos].is_ascii_digit() || bytes[*pos] == b'_') {
                *pos += 1;
            }
        }
    }
    // Literal suffix rides the token (`1u32`, `1.5f64`).
    while *pos < bytes.len() && is_ident_cont(bytes[*pos]) {
        *pos += 1;
    }
    let kind = if is_float { tk::FLOAT_LIT } else { tk::INT_LIT };
    push(scan, kind, span(start, *pos));
}

/// Two/one-char punctuation (three-char handled first).
#[allow(clippy::too_many_lines)] // flat punct table
fn scan_punct(scan: &mut Scan, bytes: &[u8], pos: &mut usize, line_has_token: &mut bool) {
    let start = *pos;
    let b = bytes[*pos];
    let two = |next: u8| *pos + 1 < bytes.len() && bytes[*pos + 1] == next;
    let kind = match b {
        b'-' if two(b'>') => {
            *pos += 2;
            tk::ARROW
        }
        b'=' if two(b'>') => {
            *pos += 2;
            tk::FATARROW
        }
        b':' if two(b':') => {
            *pos += 2;
            tk::COLONCOLON
        }
        b'.' if two(b'.') => {
            if *pos + 2 < bytes.len() && bytes[*pos + 2] == b'=' {
                *pos += 3;
                tk::DOTDOTEQ
            } else {
                *pos += 2;
                tk::DOTDOT
            }
        }
        _ if two(b'=') => {
            *pos += 2;
            match b {
                b'=' => tk::EQ,
                b'!' => tk::NE,
                b'<' => tk::LE,
                b'>' => tk::GE,
                b'+' => tk::PLUSEQ,
                b'-' => tk::MINUSEQ,
                b'*' => tk::STAREQ,
                b'/' => tk::SLASHEQ,
                _ => tk::PERCENTEQ,
            }
        }
        b'&' if two(b'&') => {
            *pos += 2;
            tk::ANDAND
        }
        b'|' if two(b'|') => {
            *pos += 2;
            tk::OROR
        }
        b'<' if two(b'<') => {
            *pos += 2;
            tk::SHL
        }
        b'>' if two(b'>') => {
            *pos += 2;
            tk::SHR
        }
        _ => {
            *pos += 1;
            match b {
                b'+' => tk::PLUS,
                b'-' => tk::MINUS,
                b'*' => tk::STAR,
                b'/' => tk::SLASH,
                b'%' => tk::PERCENT,
                b'^' => tk::CARET,
                b'!' => tk::BANG,
                b'&' => tk::AMP,
                b'|' => tk::PIPE,
                b'<' => tk::LT,
                b'>' => tk::GT,
                b'=' => tk::ASSIGN,
                b'.' => tk::DOT,
                b',' => tk::COMMA,
                b';' => tk::SEMI,
                b':' => tk::COLON,
                b'#' => tk::POUND,
                b'(' => tk::LPAREN,
                b')' => tk::RPAREN,
                b'[' => tk::LBRACK,
                b']' => tk::RBRACK,
                b'{' => tk::LBRACE,
                b'}' => tk::RBRACE,
                _ => BAD,
            }
        }
    };
    if kind == BAD {
        scan.errors += 1;
    }
    push(scan, kind, span(start, *pos));
    *line_has_token = true;
}

/// Keyword lookup (`_` is an identifier — the pattern layer reads it).
fn keyword(word: &str) -> u16 {
    match word {
        "fn" => tk::KW_FN,
        "let" => tk::KW_LET,
        "struct" => tk::KW_STRUCT,
        "enum" => tk::KW_ENUM,
        "impl" => tk::KW_IMPL,
        "trait" => tk::KW_TRAIT,
        "use" => tk::KW_USE,
        "mod" => tk::KW_MOD,
        "pub" => tk::KW_PUB,
        "const" => tk::KW_CONST,
        "static" => tk::KW_STATIC,
        "type" => tk::KW_TYPE,
        "if" => tk::KW_IF,
        "else" => tk::KW_ELSE,
        "match" => tk::KW_MATCH,
        "while" => tk::KW_WHILE,
        "loop" => tk::KW_LOOP,
        "for" => tk::KW_FOR,
        "in" => tk::KW_IN,
        "return" => tk::KW_RETURN,
        "break" => tk::KW_BREAK,
        "continue" => tk::KW_CONTINUE,
        "true" => tk::KW_TRUE,
        "false" => tk::KW_FALSE,
        "as" => tk::KW_AS,
        "where" => tk::KW_WHERE,
        "mut" => tk::KW_MUT,
        "self" => tk::KW_SELF,
        "crate" => tk::KW_CRATE,
        "super" => tk::KW_SUPER,
        "move" => tk::KW_MOVE,
        "ref" => tk::KW_REF,
        "unsafe" => tk::KW_UNSAFE,
        _ => tk::IDENT,
    }
}

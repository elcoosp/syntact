//! The C scanner (issue 034): literals with escapes, hex/octal/float
//! numbers, `//` and nested-free `/* */` comments as trivia, and
//! preprocessor lines (with `\` continuations) as single PP tokens.
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
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::COMMENT, line_has_token));
            }
            b'/' if pos + 1 < bytes.len() && bytes[pos + 1] == b'*' => {
                pos += 2;
                while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                    pos += 1;
                }
                if pos + 1 < bytes.len() {
                    pos += 2;
                } else {
                    pos = bytes.len();
                    scan.errors += 1; // unterminated block comment
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::COMMENT, line_has_token));
            }
            b'#' => {
                // Preprocessor line, with `\`-newline continuations.
                loop {
                    while pos < bytes.len() && bytes[pos] != b'\n' {
                        pos += 1;
                    }
                    // Continue when the line ends in a backslash.
                    let mut probe = pos;
                    while probe > start && (bytes[probe - 1] == b'\r' || bytes[probe - 1] == b' ') {
                        probe -= 1;
                    }
                    if probe > start && bytes[probe - 1] == b'\\' && pos < bytes.len() {
                        pos += 1;
                    } else {
                        break;
                    }
                }
                push(&mut scan, tk::PP_LINE, span(start, pos));
                line_has_token = true;
            }
            b'"' => scan_string(&mut scan, bytes, &mut pos),
            b'\'' => scan_char(&mut scan, bytes, &mut pos),
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
            b'\n' => break,
            _ => *pos += 1,
        }
    }
    scan.errors += 1;
    push(scan, BAD, span(start, *pos));
}

/// `'…'` char literal with escapes.
fn scan_char(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    *pos += 1;
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\\' => *pos += 2,
            b'\'' => {
                *pos += 1;
                push(scan, tk::CHAR_LIT, span(start, *pos));
                return;
            }
            b'\n' => break,
            _ => *pos += 1,
        }
    }
    scan.errors += 1;
    push(scan, BAD, span(start, *pos));
}

/// Numeric literals: `0x1F`, `0777`, `42`, `1.5e-3`, suffixes.
fn scan_number(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    let mut is_float = false;
    if bytes[*pos] == b'0' && *pos + 1 < bytes.len() && matches!(bytes[*pos + 1], b'x' | b'X') {
        *pos += 2;
        while *pos < bytes.len() && bytes[*pos].is_ascii_hexdigit() {
            *pos += 1;
        }
    } else {
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if *pos < bytes.len() && bytes[*pos] == b'.' {
            is_float = true;
            *pos += 1;
            while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
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
                while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
                    *pos += 1;
                }
            }
        }
    }
    // Literal suffix (`u`, `UL`, `f`, `L`) rides the token.
    while *pos < bytes.len() && is_ident_cont(bytes[*pos]) {
        *pos += 1;
    }
    let kind = if is_float { tk::FLOAT_LIT } else { tk::INT_LIT };
    push(scan, kind, span(start, *pos));
}

/// Two/one-char punctuation.
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
                b'%' => tk::PERCENTEQ,
                b'&' => tk::ANDEQ,
                b'|' => tk::OREQ,
                b'^' => tk::XOREQ,
                _ => BAD,
            }
        }
        b'<' if two(b'<') => {
            if *pos + 2 < bytes.len() && bytes[*pos + 2] == b'=' {
                *pos += 3;
                tk::SHLEQ
            } else {
                *pos += 2;
                tk::SHL
            }
        }
        b'>' if two(b'>') => {
            if *pos + 2 < bytes.len() && bytes[*pos + 2] == b'=' {
                *pos += 3;
                tk::SHREQ
            } else {
                *pos += 2;
                tk::SHR
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
        b'+' if two(b'+') => {
            *pos += 2;
            tk::INCR
        }
        b'-' if two(b'-') => {
            *pos += 2;
            tk::DECR
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
                b'~' => tk::TILDE,
                b'<' => tk::LT,
                b'>' => tk::GT,
                b'=' => tk::ASSIGN,
                b'.' => tk::DOT,
                b':' => tk::COLON,
                b';' => tk::SEMI,
                b',' => tk::COMMA,
                b'?' => tk::QUESTION,
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

/// Keyword lookup.
fn keyword(word: &str) -> u16 {
    match word {
        "int" => tk::KW_INT,
        "char" => tk::KW_CHAR,
        "float" => tk::KW_FLOAT,
        "double" => tk::KW_DOUBLE,
        "void" => tk::KW_VOID,
        "long" => tk::KW_LONG,
        "short" => tk::KW_SHORT,
        "signed" => tk::KW_SIGNED,
        "unsigned" => tk::KW_UNSIGNED,
        "_Bool" => tk::KW_BOOL,
        "const" => tk::KW_CONST,
        "volatile" => tk::KW_VOLATILE,
        "struct" => tk::KW_STRUCT,
        "union" => tk::KW_UNION,
        "enum" => tk::KW_ENUM,
        "if" => tk::KW_IF,
        "else" => tk::KW_ELSE,
        "while" => tk::KW_WHILE,
        "do" => tk::KW_DO,
        "for" => tk::KW_FOR,
        "return" => tk::KW_RETURN,
        "break" => tk::KW_BREAK,
        "continue" => tk::KW_CONTINUE,
        "sizeof" => tk::KW_SIZEOF,
        "typedef" => tk::KW_TYPEDEF,
        "static" => tk::KW_STATIC,
        "extern" => tk::KW_EXTERN,
        "inline" => tk::KW_INLINE,
        _ => tk::IDENT,
    }
}

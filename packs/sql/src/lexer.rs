//! The SQL scanner (issue 035): **case-insensitive keywords**,
//! `'…'` strings with doubled-quote escapes, hex/decimal/float
//! numbers, `--` and `/* */` comments as trivia, `||` concat.
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
            b'-' if pos + 1 < bytes.len() && bytes[pos + 1] == b'-' => {
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
                    scan.errors += 1;
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::BLOCK_COMMENT, line_has_token));
            }
            b'\'' => scan_string(&mut scan, bytes, &mut pos),
            _ if b.is_ascii_digit() => scan_number(&mut scan, bytes, &mut pos),
            _ if is_ident_start(b) => {
                while pos < bytes.len() && is_ident_cont(bytes[pos]) {
                    pos += 1;
                }
                // Case-insensitive keyword lookup (issue 035 TDD).
                let word = source[start..pos].to_ascii_lowercase();
                let kind = keyword(&word);
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

/// `'…'` with `''` escapes; unterminated strings are an error region.
fn scan_string(scan: &mut Scan, bytes: &[u8], pos: &mut usize) {
    let start = *pos;
    *pos += 1; // opening quote
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'\'' => {
                if *pos + 1 < bytes.len() && bytes[*pos + 1] == b'\'' {
                    *pos += 2; // doubled quote escape
                } else {
                    *pos += 1;
                    push(scan, tk::STRING_LIT, span(start, *pos));
                    return;
                }
            }
            b'\n' => break, // unterminated: stop at EOL
            _ => *pos += 1,
        }
    }
    scan.errors += 1;
    push(scan, BAD, span(start, *pos));
}

/// Numeric literals: `0x1F`, `42`, `1.5e-3`.
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
        if *pos + 1 < bytes.len() && bytes[*pos] == b'.' && bytes[*pos + 1].is_ascii_digit() {
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
    let kind = if is_float { tk::FLOAT_LIT } else { tk::INT_LIT };
    push(scan, kind, span(start, *pos));
}

/// Two/one-char punctuation (`<>` is `!=`).
#[allow(clippy::too_many_lines)] // flat punct table
fn scan_punct(scan: &mut Scan, bytes: &[u8], pos: &mut usize, line_has_token: &mut bool) {
    let start = *pos;
    let b = bytes[*pos];
    let two = |next: u8| *pos + 1 < bytes.len() && bytes[*pos + 1] == next;
    let kind = match b {
        b'|' if two(b'|') => {
            *pos += 2;
            tk::CONCAT
        }
        b'<' if two(b'>') => {
            *pos += 2;
            tk::NE
        }
        _ if two(b'=') => {
            *pos += 2;
            match b {
                b'!' => tk::NE,
                b'<' => tk::LE,
                b'>' => tk::GE,
                _ => BAD,
            }
        }
        _ => {
            *pos += 1;
            match b {
                b'*' => tk::STAR,
                b'+' => tk::PLUS,
                b'-' => tk::MINUS,
                b'/' => tk::SLASH,
                b'%' => tk::PERCENT,
                b'=' => tk::EQ,
                b'!' => tk::NE,
                b'<' => tk::LT,
                b'>' => tk::GT,
                b',' => tk::COMMA,
                b';' => tk::SEMI,
                b'(' => tk::LPAREN,
                b')' => tk::RPAREN,
                b'.' => tk::DOT,
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

/// Keyword lookup — the input is already lowercased.
fn keyword(word: &str) -> u16 {
    match word {
        "select" => tk::KW_SELECT,
        "from" => tk::KW_FROM,
        "where" => tk::KW_WHERE,
        "group" => tk::KW_GROUP,
        "by" => tk::KW_BY,
        "order" => tk::KW_ORDER,
        "limit" => tk::KW_LIMIT,
        "offset" => tk::KW_OFFSET,
        "having" => tk::KW_HAVING,
        "with" => tk::KW_WITH,
        "as" => tk::KW_AS,
        "on" => tk::KW_ON,
        "join" => tk::KW_JOIN,
        "left" => tk::KW_LEFT,
        "right" => tk::KW_RIGHT,
        "inner" => tk::KW_INNER,
        "outer" => tk::KW_OUTER,
        "cross" => tk::KW_CROSS,
        "and" => tk::KW_AND,
        "or" => tk::KW_OR,
        "not" => tk::KW_NOT,
        "null" => tk::KW_NULL,
        "is" => tk::KW_IS,
        "in" => tk::KW_IN,
        "like" => tk::KW_LIKE,
        "between" => tk::KW_BETWEEN,
        "case" => tk::KW_CASE,
        "when" => tk::KW_WHEN,
        "then" => tk::KW_THEN,
        "else" => tk::KW_ELSE,
        "end" => tk::KW_END,
        "insert" => tk::KW_INSERT,
        "into" => tk::KW_INTO,
        "values" => tk::KW_VALUES,
        "update" => tk::KW_UPDATE,
        "set" => tk::KW_SET,
        "delete" => tk::KW_DELETE,
        "create" => tk::KW_CREATE,
        "table" => tk::KW_TABLE,
        "primary" => tk::KW_PRIMARY,
        "key" => tk::KW_KEY,
        "foreign" => tk::KW_FOREIGN,
        "references" => tk::KW_REFERENCES,
        "unique" => tk::KW_UNIQUE,
        "check" => tk::KW_CHECK,
        "default" => tk::KW_DEFAULT,
        "cast" => tk::KW_CAST,
        "distinct" => tk::KW_DISTINCT,
        "union" => tk::KW_UNION,
        "all" => tk::KW_ALL,
        "asc" => tk::KW_ASC,
        "desc" => tk::KW_DESC,
        "autoincrement" => tk::KW_AUTOINCREMENT,
        "if" => tk::KW_IF,
        "glob" => tk::KW_GLOB,
        "exists" => tk::KW_EXISTS,
        _ => tk::IDENT,
    }
}

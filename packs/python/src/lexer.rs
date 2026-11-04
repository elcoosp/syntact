//! The Python scanner (issue 028): logical lines with INDENT/DEDENT,
//! bracket continuation, comment/blank trivia, and f-strings with a
//! nested interpolation mode stack (`f"a {f"{b}"} c"`).
//!
//! Output: one `(kind, range)` feed for the parser (clean — trivia rides
//! the separate trivia table) plus `(range, trivia kind, trailing)`
//! records covering every gap.

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
    /// Unmatched indentation / stray characters (error regions).
    pub errors: usize,
}

/// The f-string mode stack frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// Scanning f-string literal text (a `{` enters interpolation).
    Text,
    /// Inside an interpolation expression (brace-free subset).
    Interp,
}

/// The unmatched-character error kind (scanner-local sentinel).
const BAD: u16 = 0xFFFF - 1;

/// Scan `source` into the feed + trivia.
#[must_use]
#[allow(clippy::too_many_lines)] // the mode machine is the grammar data
pub fn scan(source: &str) -> Scan {
    let bytes = source.as_bytes();
    let mut scan = Scan::default();
    let mut pos = 0_usize;
    let mut indents: Vec<u32> = vec![0];
    let mut brackets = 0_usize;
    let mut frames: Vec<Frame> = Vec::new();
    let mut interp_braces = 0_usize;
    let mut line_start = true;
    let end = u32::try_from(source.len()).unwrap_or(u32::MAX);

    let push = |scan: &mut Scan, kind: u16, range: std::ops::Range<u32>| {
        scan.tokens.push(ScanToken { kind, range });
    };

    while pos < bytes.len() {
        // Line start: indentation + logical-line bookkeeping.
        if line_start && brackets == 0 && frames.is_empty() {
            let indent_start = pos;
            while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
                pos += 1;
            }
            // Blank or comment-only line: pure trivia, no NEWLINE/INDENT.
            let mut probe = pos;
            while probe < bytes.len() && matches!(bytes[probe], b' ' | b'\t') {
                probe += 1;
            }
            let blankish = probe >= bytes.len() || bytes[probe] == b'\n' || bytes[probe] == b'#';
            if blankish {
                let mut trivia_start = indent_start;
                if pos > indent_start {
                    let range = span(indent_start, pos);
                    scan.trivia.push((range, tk::WHITESPACE, false));
                    trivia_start = pos;
                }
                if pos < bytes.len() && bytes[pos] == b'#' {
                    let cstart = pos;
                    while pos < bytes.len() && bytes[pos] != b'\n' {
                        pos += 1;
                    }
                    let range = span(cstart, pos);
                    scan.trivia.push((range, tk::COMMENT, true));
                    trivia_start = pos;
                }
                if pos < bytes.len() && bytes[pos] == b'\n' {
                    pos += 1;
                    let range = span(trivia_start, pos);
                    scan.trivia.push((range, tk::WHITESPACE, true));
                }
                continue;
            }
            // Real statement line: INDENT / DEDENT decisions.
            let level = indentation_level(&source[indent_start..pos]);
            let top = *indents.last().expect("indent stack never empty");
            if level > top {
                indents.push(level);
                push(&mut scan, tk::INDENT, span(pos, pos));
            } else if level < top {
                while *indents.last().expect("never empty") > level {
                    indents.pop();
                    push(&mut scan, tk::DEDENT, span(pos, pos));
                }
                if *indents.last().expect("never empty") != level {
                    // Mismatched indentation: an error region on the run —
                    // it covers the indentation bytes, so no trivia here.
                    scan.errors += 1;
                    push(&mut scan, BAD, span(indent_start, pos));
                    line_start = false;
                    continue;
                }
            }
            if pos > indent_start {
                let range = span(indent_start, pos);
                scan.trivia.push((range, tk::WHITESPACE, false));
            }
            line_start = false;
            continue;
        }

        let start = pos;
        let b = bytes[pos];
        match b {
            b'\n' => {
                pos += 1;
                if brackets == 0 && frames.is_empty() {
                    push(&mut scan, tk::NEWLINE, span(start, pos));
                    line_start = true;
                } else {
                    let range = span(start, pos);
                    scan.trivia.push((range, tk::WHITESPACE, true));
                }
            }
            b' ' | b'\t' | b'\r' => {
                while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r') {
                    pos += 1;
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::WHITESPACE, true));
            }
            b'#' => {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::COMMENT, true));
            }
            b'"' | b'\'' => {
                if frames.last() == Some(&Frame::Text) {
                    // f-string closing quote.
                    pos += 1;
                    push(&mut scan, tk::FSTRING_END, span(start, pos));
                    frames.pop();
                    continue;
                }
                let Some(len) = scan_string_len(source, pos) else {
                    pos += 1;
                    scan.errors += 1;
                    push(&mut scan, BAD, span(start, pos));
                    continue;
                };
                pos += len;
                push(&mut scan, tk::STRING, span(start, pos));
            }
            b'f' if bytes
                .get(pos + 1)
                .is_some_and(|q| *q == b'"' || *q == b'\'')
                && (frames.is_empty() || frames.last() == Some(&Frame::Interp)) =>
            {
                pos += 2;
                push(&mut scan, tk::FSTRING_START, span(start, pos));
                frames.push(Frame::Text);
            }
            b'{' if frames.last() == Some(&Frame::Text) => {
                pos += 1;
                push(&mut scan, tk::INTERP_LBRACE, span(start, pos));
                frames.push(Frame::Interp);
                interp_braces = 0;
            }
            b'}' if frames.last() == Some(&Frame::Interp) => {
                pos += 1;
                if interp_braces > 0 {
                    interp_braces -= 1;
                } else {
                    frames.pop();
                }
                push(&mut scan, tk::INTERP_RBRACE, span(start, pos));
            }
            b'{' if frames.last() == Some(&Frame::Interp) => {
                // Nested brace displays inside interpolations are outside
                // the documented subset: an error region, still progress.
                interp_braces += 1;
                pos += 1;
                scan.errors += 1;
                push(&mut scan, BAD, span(start, pos));
            }
            _ if frames.last() == Some(&Frame::Text) => {
                // f-string literal text: everything up to the next `{`,
                // closing quote or line end (verbatim — lossless).
                while pos < bytes.len() && !matches!(bytes[pos], b'{' | b'}' | b'"' | b'\'' | b'\n')
                {
                    pos += 1;
                }
                if pos == start {
                    pos += 1; // stray char inside text mode
                    scan.errors += 1;
                    push(&mut scan, BAD, span(start, pos));
                } else {
                    push(&mut scan, tk::FSTRING_TEXT, span(start, pos));
                }
            }
            _ => {
                let Some((kind, len)) = scan_atom(source, pos) else {
                    pos += 1;
                    scan.errors += 1;
                    push(&mut scan, BAD, span(start, pos));
                    continue;
                };
                match kind {
                    Atom::Op(kind) => {
                        if matches!(kind, tk::LPAREN | tk::LBRACK) {
                            brackets += 1;
                        }
                        if matches!(kind, tk::RPAREN | tk::RBRACK) && brackets > 0 {
                            brackets -= 1;
                        }
                        pos += len;
                        push(&mut scan, kind, span(start, pos));
                    }
                    Atom::Word(kind) => {
                        pos += len;
                        push(&mut scan, kind, span(start, pos));
                    }
                }
            }
        }
    }
    // Flush DEDENTs at EOF (zero-width at the source end).
    while indents.len() > 1 {
        indents.pop();
        push(&mut scan, tk::DEDENT, span(bytes.len(), bytes.len()));
    }
    let _ = end;
    scan
}

fn span(start: usize, end: usize) -> std::ops::Range<u32> {
    u32::try_from(start).expect("offset fits u32")..u32::try_from(end).expect("offset fits u32")
}

fn indentation_level(text: &str) -> u32 {
    // Spaces count 1; a tab normalizes to the next multiple of 4 (the
    // documented subset prefers spaces — tabs still make progress).
    let mut level = 0_u32;
    for c in text.chars() {
        if c == '\t' {
            level = (level / 4 + 1) * 4;
        } else {
            level += 1;
        }
    }
    level
}

enum Atom {
    Op(u16),
    Word(u16),
}

/// The longest operator/word at `pos`.
#[allow(clippy::too_many_lines)] // the operator table is the grammar data
fn scan_atom(source: &str, pos: usize) -> Option<(Atom, usize)> {
    let bytes = source.as_bytes();
    let b = *bytes.get(pos)?;
    // Two-char operators first (longest match).
    for (text, kind) in [
        ("**", tk::PSTAR),
        ("//", tk::DSLASH),
        ("==", tk::EQ),
        ("!=", tk::NE),
        ("<=", tk::LE),
        (">=", tk::GE),
        ("->", tk::ARROW),
        ("<<", tk::LSHIFT),
        (">>", tk::RSHIFT),
    ] {
        if source[pos..].starts_with(text) {
            return Some((Atom::Op(kind), 2));
        }
    }
    let one = |kind| Some((Atom::Op(kind), 1_usize));
    match b {
        b'+' => one(tk::PLUS),
        b'-' => one(tk::MINUS),
        b'*' => one(tk::STAR),
        b'/' => one(tk::SLASH),
        b'%' => one(tk::PERCENT),
        b'<' => one(tk::LT),
        b'>' => one(tk::GT),
        b'=' => one(tk::ASSIGN),
        b'(' => one(tk::LPAREN),
        b')' => one(tk::RPAREN),
        b'[' => one(tk::LBRACK),
        b']' => one(tk::RBRACK),
        b':' => one(tk::COLON),
        b',' => one(tk::COMMA),
        b'.' => one(tk::DOT),
        b'@' => one(tk::AT),
        b'|' => one(tk::PIPE),
        b'^' => one(tk::CARET),
        b'&' => one(tk::AMP),
        b'~' => one(tk::TILDE),
        b'0'..=b'9' => {
            let mut end = pos + 1;
            while end < bytes.len()
                && (bytes[end].is_ascii_digit() || matches!(bytes[end], b'.' | b'_'))
            {
                end += 1;
            }
            Some((Atom::Word(tk::NUMBER), end - pos))
        }
        _ if b.is_ascii_alphabetic() || b == b'_' => {
            let mut end = pos + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let word = &source[pos..end];
            let kind = match word {
                "if" => tk::KW_IF,
                "elif" => tk::KW_ELIF,
                "else" => tk::KW_ELSE,
                "while" => tk::KW_WHILE,
                "for" => tk::KW_FOR,
                "in" => tk::KW_IN,
                "def" => tk::KW_DEF,
                "return" => tk::KW_RETURN,
                "pass" => tk::KW_PASS,
                "break" => tk::KW_BREAK,
                "continue" => tk::KW_CONTINUE,
                "and" => tk::KW_AND,
                "or" => tk::KW_OR,
                "not" => tk::KW_NOT,
                "is" => tk::KW_IS,
                "True" => tk::KW_TRUE,
                "False" => tk::KW_FALSE,
                "None" => tk::KW_NONE,
                _ => tk::NAME,
            };
            Some((Atom::Word(kind), end - pos))
        }
        _ => None,
    }
}

/// The length of a simple string starting at `pos` (the opening quote).
fn scan_string_len(source: &str, pos: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let quote = *bytes.get(pos)?;
    let mut idx = pos + 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' => idx += 2,
            b'\n' => return None, // single-line strings in the subset
            _ if bytes[idx] == quote => return Some(idx + 1 - pos),
            _ => idx += 1,
        }
    }
    None
}

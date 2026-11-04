//! The TypeScript scanner (issue 027): single tokens with **`>` never
//! merged** (nested generics close naturally — the parser combines
//! adjacent `>` into shifts), template literals with a code/text mode
//! stack, `//`/`/* */`/`/** */` comments as trivia.
//!
//! Note the deliberate asymmetry: `===`, `!==`, `=>`, `==`, `!=`, `<=`,
//! `>=`, `&&`, `||`, `**`, `...` ARE merged (unambiguous inside
//! generics); `>>` and `>>>` are NOT — that is the `>>`-split rule.

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
    /// Stray characters / unterminated literals.
    pub errors: usize,
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
    // Per-template-frame state: `true` while inside that frame's `${}`
    // interpolation (code mode), `false` in its text mode.
    let mut templates: Vec<bool> = Vec::new();
    let mut brackets = 0_usize;

    let push = |scan: &mut Scan, kind: u16, range: std::ops::Range<u32>| {
        scan.tokens.push(ScanToken { kind, range });
    };
    // Code mode = top level (no frame) or the top frame's interpolation.
    let in_code = |templates: &Vec<bool>| templates.last().copied().unwrap_or(true);

    while pos < bytes.len() {
        let start = pos;
        let b = bytes[pos];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => {
                while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n') {
                    pos += 1;
                }
                let range = span(start, pos);
                let trailing = !source[start..pos].contains('\n');
                scan.trivia.push((range, tk::WHITESPACE, trailing));
            }
            b'/' if source[pos..].starts_with("/**") => {
                // Doc comment: closed by the first `*/`.
                let close = source[pos..]
                    .find("*/")
                    .map_or(bytes.len(), |idx| pos + idx + 2);
                pos = close;
                let range = span(start, pos);
                scan.trivia.push((range, tk::DOC_COMMENT, false));
            }
            b'/' if source[pos..].starts_with("//") => {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                let range = span(start, pos);
                scan.trivia.push((range, tk::COMMENT, true));
            }
            b'/' if source[pos..].starts_with("/*") => {
                let close = source[pos..]
                    .find("*/")
                    .map_or(bytes.len(), |idx| pos + idx + 2);
                pos = close;
                let range = span(start, pos);
                scan.trivia.push((range, tk::COMMENT, false));
            }
            b'`' => {
                pos += 1;
                if in_code(&templates) {
                    // Open a template: its TEXT mode begins.
                    push(&mut scan, tk::TEMPLATE_START, span(start, pos));
                    templates.push(false);
                } else {
                    // Close the innermost template; the enclosing mode
                    // (code, if any) resumes.
                    push(&mut scan, tk::TEMPLATE_END, span(start, pos));
                    templates.pop();
                }
            }
            b'$' if source[pos..].starts_with("${") && !in_code(&templates) => {
                pos += 2;
                push(&mut scan, tk::TEMPLATE_DOLLAR, span(start, pos));
                *templates.last_mut().expect("template frame") = true;
            }
            b'}' if in_code(&templates) && !templates.is_empty() && brackets == 0 => {
                // Close the top interpolation; back to its text mode.
                pos += 1;
                push(&mut scan, tk::RBRACE, span(start, pos));
                *templates.last_mut().expect("template frame") = false;
            }
            b'{' if in_code(&templates) => {
                brackets += 1;
                pos += 1;
                push(&mut scan, tk::LBRACE, span(start, pos));
            }
            b'}' if in_code(&templates) => {
                brackets = brackets.saturating_sub(1);
                pos += 1;
                push(&mut scan, tk::RBRACE, span(start, pos));
            }
            _ if !templates.is_empty() && !in_code(&templates) => {
                // Template text of the top frame: verbatim until `${`,
                // the closing backtick or a stray `}`.
                while pos < bytes.len()
                    && bytes[pos] != b'`'
                    && !source[pos..].starts_with("${")
                    && bytes[pos] != b'}'
                {
                    pos += 1;
                }
                if pos == start {
                    pos += 1;
                    scan.errors += 1;
                    push(&mut scan, BAD, span(start, pos));
                } else {
                    push(&mut scan, tk::TEMPLATE_TEXT, span(start, pos));
                }
            }
            b'"' | b'\'' => {
                let Some(len) = scan_string_len(source, pos) else {
                    pos += 1;
                    scan.errors += 1;
                    push(&mut scan, BAD, span(start, pos));
                    continue;
                };
                pos += len;
                push(&mut scan, tk::STRING, span(start, pos));
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
                        if kind == tk::LBRACK {
                            brackets += 1;
                        }
                        if kind == tk::RBRACK && brackets > 0 {
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
    scan
}

fn span(start: usize, end: usize) -> std::ops::Range<u32> {
    u32::try_from(start).expect("offset fits u32")..u32::try_from(end).expect("offset fits u32")
}

enum Atom {
    Op(u16),
    Word(u16),
}

/// The longest operator/word at `pos`. Deliberately NO `>>`/`>>>`
/// merging (the generics split rule); `<<` also stays split for
/// symmetry.
#[allow(clippy::too_many_lines)] // the operator table is the grammar data
fn scan_atom(source: &str, pos: usize) -> Option<(Atom, usize)> {
    let bytes = source.as_bytes();
    let b = *bytes.get(pos)?;
    for (text, kind) in [
        ("===", Atom::Op(tk::EQ3)),
        ("!==", Atom::Op(tk::NEQ3)),
        ("=>", Atom::Op(tk::FAT_ARROW)),
        ("...", Atom::Op(tk::ELLIPSIS)),
        ("==", Atom::Op(tk::EQUALS)),
        ("!=", Atom::Op(tk::NE)),
        ("<=", Atom::Op(tk::LE)),
        (">=", Atom::Op(tk::GE)),
        ("&&", Atom::Op(tk::ANDAND)),
        ("||", Atom::Op(tk::OROR)),
        ("**", Atom::Op(tk::PSTAR)),
    ] {
        if source[pos..].starts_with(text) {
            return Some((kind, text.len()));
        }
    }
    let one = |kind| Some((Atom::Op(kind), 1_usize));
    match b {
        b'{' => one(tk::LBRACE),
        b'}' => one(tk::RBRACE),
        b'(' => one(tk::LPAREN),
        b')' => one(tk::RPAREN),
        b'[' => one(tk::LBRACK),
        b']' => one(tk::RBRACK),
        b';' => one(tk::SEMI),
        b',' => one(tk::COMMA),
        b'.' => one(tk::DOT),
        b':' => one(tk::COLON),
        b'?' => one(tk::QUESTION),
        b'=' => one(tk::EQUALS),
        b'+' => one(tk::PLUS),
        b'-' => one(tk::MINUS),
        b'*' => one(tk::STAR),
        b'/' => one(tk::SLASH),
        b'%' => one(tk::PERCENT),
        b'<' => one(tk::LT),
        b'>' => one(tk::GT),
        b'!' => one(tk::BANG),
        b'&' => one(tk::AMP),
        b'|' => one(tk::PIPE),
        b'^' => one(tk::CARET),
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
        _ if b.is_ascii_alphabetic() || b == b'_' || b == b'$' => {
            let mut end = pos + 1;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'$'))
            {
                end += 1;
            }
            let word = &source[pos..end];
            let kind = match word {
                "import" => tk::KW_IMPORT,
                "from" => tk::KW_FROM,
                "export" => tk::KW_EXPORT,
                "default" => tk::KW_DEFAULT,
                "interface" => tk::KW_INTERFACE,
                "type" => tk::KW_TYPE,
                "class" => tk::KW_CLASS,
                "enum" => tk::KW_ENUM,
                "function" => tk::KW_FUNCTION,
                "const" => tk::KW_CONST,
                "let" => tk::KW_LET,
                "var" => tk::KW_VAR,
                "if" => tk::KW_IF,
                "else" => tk::KW_ELSE,
                "while" => tk::KW_WHILE,
                "for" => tk::KW_FOR,
                "of" => tk::KW_OF,
                "return" => tk::KW_RETURN,
                "readonly" => tk::KW_READONLY,
                "private" => tk::KW_PRIVATE,
                "public" => tk::KW_PUBLIC,
                "static" => tk::KW_STATIC,
                "true" => tk::KW_TRUE,
                "false" => tk::KW_FALSE,
                "null" => tk::KW_NULL,
                "undefined" => tk::KW_UNDEFINED,
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
            b'\n' => return None,
            _ if bytes[idx] == quote => return Some(idx + 1 - pos),
            _ => idx += 1,
        }
    }
    None
}

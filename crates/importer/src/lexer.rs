//! JS-subset lexer for `grammar.js` sources (issue 037).
//!
//! The importer never executes JavaScript — it lexes just enough of the
//! surface syntax (identifiers, strings, regex literals, numbers,
//! punctuation, comments) for the parser to recover the `grammar({...})`
//! object literal. Anything outside the subset fails loudly with an
//! offset so the report can point at the source.

use std::fmt;

use crate::ImportError;

/// A lexical token with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token payload.
    pub tok: Tok,
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
}

/// Token payloads the subset needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// An identifier or keyword (`choice`, `$`, `module`).
    Ident(String),
    /// A string literal, unescaped (`'json'` → `json`).
    Str(String),
    /// A regex literal: body and flags (`/ab?/gi` → `ab?`, `gi`).
    Regex(String, String),
    /// A numeric literal, kept verbatim (`2`).
    Num(String),
    /// Punctuation: `{ } ( ) [ ] , : ; . = =>`.
    Punct(&'static str),
    /// End of input.
    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ident(name) => write!(f, "`{name}`"),
            Self::Str(text) => write!(f, "string {text:?}"),
            Self::Regex(body, flags) => write!(f, "regex /{body}/{flags}"),
            Self::Num(text) => write!(f, "number {text}"),
            Self::Punct(p) => write!(f, "`{p}`"),
            Self::Eof => write!(f, "end of input"),
        }
    }
}

/// Lex `src` into the JS-subset token stream.
///
/// # Errors
/// [`ImportError::Parse`] on an unterminated string/regex/comment or a
/// character outside the subset.
pub fn lex(src: &str) -> Result<Vec<Token>, ImportError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i = skip_line_comment(src, i);
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i = skip_block_comment(src, i)?;
            }
            b'/' => {
                let (tok, next) = lex_regex(src, i)?;
                out.push(tok);
                i = next;
            }
            b'\'' | b'"' => {
                let (tok, next) = lex_string(src, i)?;
                out.push(tok);
                i = next;
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Num(src[start..i].to_owned()),
                    start,
                    end: i,
                });
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
                {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Ident(src[start..i].to_owned()),
                    start,
                    end: i,
                });
            }
            _ => {
                let (tok, next) = lex_punct(src, i)?;
                out.push(Token {
                    tok,
                    start: i,
                    end: next,
                });
                i = next;
            }
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        start: bytes.len(),
        end: bytes.len(),
    });
    Ok(out)
}

fn skip_line_comment(src: &str, mut i: usize) -> usize {
    while i < src.len() && src.as_bytes()[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(src: &str, i: usize) -> Result<usize, ImportError> {
    let bytes = src.as_bytes();
    let mut j = i + 2;
    while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
        j += 1;
    }
    if j + 1 >= bytes.len() {
        return Err(ImportError::Parse {
            offset: i,
            message: "unterminated block comment".to_owned(),
        });
    }
    Ok(j + 2)
}

/// Regex literals are unambiguous in the subset: a `/` never follows an
/// identifier, number, string, or closing bracket (there is no division
/// to express), so every other `/` starts a regex.
fn lex_regex(src: &str, start: usize) -> Result<(Token, usize), ImportError> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    let mut body = String::new();
    let mut in_class = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\\' if i + 1 < bytes.len() => {
                body.push(b as char);
                body.push(bytes[i + 1] as char);
                i += 2;
            }
            b'[' => {
                in_class = true;
                body.push('[');
                i += 1;
            }
            b']' => {
                in_class = false;
                body.push(']');
                i += 1;
            }
            b'/' if !in_class => {
                let mut flags = String::new();
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    flags.push(bytes[j] as char);
                    j += 1;
                }
                return Ok((
                    Token {
                        tok: Tok::Regex(body, flags),
                        start,
                        end: j,
                    },
                    j,
                ));
            }
            b'\n' => break,
            _ => {
                // Multibyte UTF-8: slice the full char.
                let ch = src[i..].chars().next().unwrap_or('?');
                body.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Err(ImportError::Parse {
        offset: start,
        message: "unterminated regex literal".to_owned(),
    })
}

fn lex_string(src: &str, start: usize) -> Result<(Token, usize), ImportError> {
    let bytes = src.as_bytes();
    let quote = bytes[start];
    let mut i = start + 1;
    let mut text = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\\' if i + 1 < bytes.len() => {
                let (ch, next) = unescape(src, i)?;
                text.push(ch);
                i = next;
            }
            b if b == quote => {
                return Ok((
                    Token {
                        tok: Tok::Str(text),
                        start,
                        end: i + 1,
                    },
                    i + 1,
                ));
            }
            b'\n' => break,
            _ => {
                let ch = src[i..].chars().next().unwrap_or('?');
                text.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Err(ImportError::Parse {
        offset: start,
        message: "unterminated string literal".to_owned(),
    })
}

/// Unescape one JS escape sequence starting at the backslash.
fn unescape(src: &str, backslash: usize) -> Result<(char, usize), ImportError> {
    let bytes = src.as_bytes();
    let after = backslash + 1;
    let b = bytes[after];
    let ch = match b {
        b'n' => '\n',
        b't' => '\t',
        b'r' => '\r',
        b'0' => '\0',
        b'u' => {
            // \uXXXX (the subset never needs surrogate pairs).
            let hex = src
                .get(after + 1..after + 5)
                .ok_or_else(|| ImportError::Parse {
                    offset: backslash,
                    message: "truncated \\u escape".to_owned(),
                })?;
            let code = u32::from_str_radix(hex, 16).map_err(|_| ImportError::Parse {
                offset: backslash,
                message: "malformed \\u escape".to_owned(),
            })?;
            return char::from_u32(code).map(|c| (c, after + 5)).ok_or_else(|| {
                ImportError::Parse {
                    offset: backslash,
                    message: "malformed \\u escape".to_owned(),
                }
            });
        }
        other => other as char,
    };
    Ok((ch, after + 1))
}

fn lex_punct(src: &str, i: usize) -> Result<(Tok, usize), ImportError> {
    let bytes = src.as_bytes();
    let b = bytes[i];
    let two = |expected: &[u8; 2]| bytes.len() >= i + 2 && &bytes[i..i + 2] == expected;
    if two(b"=>") {
        return Ok((Tok::Punct("=>"), i + 2));
    }
    let one = match b {
        b'{' => "{",
        b'}' => "}",
        b'(' => "(",
        b')' => ")",
        b'[' => "[",
        b']' => "]",
        b',' => ",",
        b':' => ":",
        b';' => ";",
        b'.' => ".",
        b'=' => "=",
        _ => {
            return Err(ImportError::Parse {
                offset: i,
                message: format!(
                    "unexpected character `{}` outside the grammar.js subset",
                    src[i..].chars().next().unwrap_or('?')
                ),
            });
        }
    };
    Ok((Tok::Punct(one), i + 1))
}

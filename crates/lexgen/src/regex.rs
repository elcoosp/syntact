//! Regex subset compiler for token patterns (issue 014): the syntax
//! token definitions need — literals, `.`, character classes, the `\d`,
//! `\w`, `\s` shorthands, quantifiers `* + ? {m} {m,} {m,n}`, alternation
//! and grouping. Anything else is a [`LexError`] with a byte position.

use std::fmt;

/// A compiled regex AST node (post-parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Regex {
    /// Matches the empty string.
    Empty,
    /// One character within a sorted, disjoint set of inclusive scalar
    /// ranges.
    Class(Vec<(u32, u32)>),
    /// Concatenation.
    Concat(Vec<Regex>),
    /// Alternation (the DFA resolves ambiguity: longest match, then
    /// declaration order).
    Alt(Vec<Regex>),
    /// Zero or more.
    Star(Box<Regex>),
    /// One or more.
    Plus(Box<Regex>),
    /// Zero or one.
    Opt(Box<Regex>),
    /// Between `min` and `max` occurrences (inclusive; `None` =
    /// unbounded).
    Repeat {
        /// Minimum repetitions.
        min: u32,
        /// Maximum repetitions (`None` = unbounded).
        max: Option<u32>,
        /// The repeated expression.
        inner: Box<Regex>,
    },
}

/// A regex parse error: message plus the byte offset where it was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    /// What went wrong.
    pub message: String,
    /// Byte offset into the pattern.
    pub position: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.position)
    }
}

impl std::error::Error for LexError {}

/// Expansion cap for bounded repetitions (keeps NFA size linear).
pub const MAX_REPEAT: u32 = 256;

/// Highest Unicode scalar value + 1 (the class-space end sentinel).
pub const SCALAR_END: u32 = 0x11_0000;

/// Parse a token-pattern regex.
pub fn parse_regex(pattern: &str) -> Result<Regex, LexError> {
    let mut parser = Parser {
        chars: pattern.chars().collect(),
        pos: 0,
    };
    let regex = parser.parse_alt()?;
    if parser.pos != parser.chars.len() {
        return Err(parser.err("unbalanced `)`".into()));
    }
    Ok(regex)
}

/// `\w` expansion.
#[must_use]
pub fn word_ranges() -> Vec<(u32, u32)> {
    vec![
        ('0' as u32, '9' as u32),
        ('A' as u32, 'Z' as u32),
        ('_' as u32, '_' as u32),
        ('a' as u32, 'z' as u32),
    ]
}

/// `\s` expansion.
#[must_use]
pub fn space_ranges() -> Vec<(u32, u32)> {
    vec![
        ('\t' as u32, '\n' as u32),
        ('\x0C' as u32, '\r' as u32),
        (' ' as u32, ' ' as u32),
    ]
}

/// `\d` expansion.
#[must_use]
pub fn digit_ranges() -> Vec<(u32, u32)> {
    vec![('0' as u32, '9' as u32)]
}

/// Complement of a range set over the scalar space.
#[must_use]
pub fn negate_ranges(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut sorted: Vec<(u32, u32)> = ranges.to_vec();
    sorted.sort_unstable();
    let mut out = Vec::new();
    let mut cursor = 0u32;
    for (lo, hi) in sorted {
        if lo > cursor {
            out.push((cursor, lo - 1));
        }
        cursor = cursor.max(hi.saturating_add(1));
    }
    if cursor < SCALAR_END {
        out.push((cursor, SCALAR_END - 1));
    }
    out
}

/// Normalize a range set: sort + merge overlapping/touching ranges.
#[must_use]
pub fn normalize_ranges(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut sorted: Vec<(u32, u32)> = ranges.to_vec();
    sorted.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (lo, hi) in sorted {
        match out.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => {
                last.1 = last.1.max(hi);
            }
            _ => out.push((lo, hi)),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn err(&self, message: String) -> LexError {
        LexError {
            message,
            position: self.pos,
        }
    }

    fn parse_alt(&mut self) -> Result<Regex, LexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().expect("one branch")
        } else {
            Regex::Alt(branches)
        })
    }

    fn parse_concat(&mut self) -> Result<Regex, LexError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.parse_repeat()?);
        }
        Ok(match parts.len() {
            0 => Regex::Empty,
            1 => parts.pop().expect("one part"),
            _ => Regex::Concat(parts),
        })
    }

    fn parse_repeat(&mut self) -> Result<Regex, LexError> {
        let atom = self.parse_atom()?;
        let mut current = atom;
        loop {
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    current = Regex::Star(Box::new(current));
                }
                Some('+') => {
                    self.pos += 1;
                    current = Regex::Plus(Box::new(current));
                }
                Some('?') => {
                    self.pos += 1;
                    current = Regex::Opt(Box::new(current));
                }
                Some('{') => {
                    let Some((min, max)) = self.try_parse_braces()? else {
                        break;
                    };
                    current = match (min, max) {
                        (0, Some(0)) => Regex::Empty,
                        (0, Some(1)) => Regex::Opt(Box::new(current)),
                        (0, None) => Regex::Star(Box::new(current)),
                        (1, None) => Regex::Plus(Box::new(current)),
                        (min, max) => Regex::Repeat {
                            min,
                            max,
                            inner: Box::new(current),
                        },
                    };
                }
                _ => break,
            }
        }
        Ok(current)
    }

    /// `{m}`, `{m,}` or `{m,n}`. A `{` that does not open a valid
    /// quantifier rewinds and is treated as a literal.
    fn try_parse_braces(&mut self) -> Result<Option<(u32, Option<u32>)>, LexError> {
        let start = self.pos;
        self.pos += 1; // `{`
        let Some(min) = self.parse_number() else {
            self.pos = start;
            return Ok(None);
        };
        let max = match self.peek() {
            Some('}') => {
                self.pos += 1;
                Some(min)
            }
            Some(',') => {
                self.pos += 1;
                if self.peek() == Some('}') {
                    self.pos += 1;
                    None
                } else {
                    let Some(n) = self.parse_number() else {
                        return Err(self.err("expected max in {m,n}".into()));
                    };
                    if self.peek() != Some('}') {
                        return Err(self.err("expected `}` after {m,n".into()));
                    }
                    self.pos += 1;
                    Some(n)
                }
            }
            _ => {
                self.pos = start;
                return Ok(None);
            }
        };
        if min > MAX_REPEAT || max.is_some_and(|max| max > MAX_REPEAT) {
            return Err(LexError {
                message: format!("repetition bound above cap {MAX_REPEAT}"),
                position: start,
            });
        }
        if max.is_some_and(|max| max < min) {
            return Err(LexError {
                message: "repetition max below min".into(),
                position: start,
            });
        }
        Ok(Some((min, max)))
    }

    fn parse_number(&mut self) -> Option<u32> {
        let digits = self.chars[self.pos..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        let text: String = self.chars[self.pos..self.pos + digits].iter().collect();
        self.pos += digits;
        text.parse().ok()
    }

    fn parse_atom(&mut self) -> Result<Regex, LexError> {
        match self.bump() {
            None => Ok(Regex::Empty),
            Some('(') => {
                let inner = self.parse_alt()?;
                if self.bump() != Some(')') {
                    return Err(self.err("expected `)`".into()));
                }
                Ok(inner)
            }
            Some('[') => self.parse_class(),
            Some('.') => Ok(Regex::Class(vec![
                (0, '\n' as u32 - 1),
                ('\n' as u32 + 1, SCALAR_END - 1),
            ])),
            Some('\\') => Ok(Regex::Class(self.parse_escape_ranges()?)),
            Some('*' | '+' | '?') => Err(self.err("quantifier without atom".into())),
            Some(')') => Err(self.err("unbalanced `)`".into())),
            Some(c) => Ok(Regex::Class(vec![(c as u32, c as u32)])),
        }
    }

    fn parse_class(&mut self) -> Result<Regex, LexError> {
        let negate = if self.peek() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.bump() else {
                return Err(self.err("unterminated `[`".into()));
            };
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo: u32 = if c == '\\' {
                // Shorthands expand to their full range sets inside
                // classes and stay atomic; single-char escapes fall
                // through to the (optional) range logic.
                let expansion = self.parse_escape_ranges()?;
                match expansion.as_slice() {
                    [(single_lo, single_hi)] if single_lo == single_hi => *single_lo,
                    atomic => {
                        ranges.extend(atomic.iter().copied());
                        continue;
                    }
                }
            } else {
                c as u32
            };
            // Range? (`-` before `]` is a literal dash.)
            if self.peek() == Some('-')
                && self
                    .chars
                    .get(self.pos + 1)
                    .is_some_and(|&next| next != ']')
            {
                self.pos += 1; // `-`
                let hi_char = self.bump().expect("checked above");
                let hi = if hi_char == '\\' {
                    let expansion = self.parse_escape_ranges()?;
                    if expansion.len() != 1 {
                        return Err(self.err("shorthand cannot end a range".into()));
                    }
                    expansion[0].1
                } else {
                    hi_char as u32
                };
                if hi < lo {
                    return Err(self.err("class range high below low".into()));
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        if ranges.is_empty() {
            return Err(self.err("empty class".into()));
        }
        if negate {
            ranges = negate_ranges(&ranges);
        } else {
            ranges = normalize_ranges(&ranges);
        }
        Ok(Regex::Class(ranges))
    }

    /// The range expansion of one escape: shorthands (`\d`, `\w`, `\s`)
    /// contribute multiple ranges; anything else is the escaped
    /// character itself.
    fn parse_escape_ranges(&mut self) -> Result<Vec<(u32, u32)>, LexError> {
        let Some(c) = self.bump() else {
            return Err(self.err("trailing `\\`".into()));
        };
        Ok(match c {
            'd' => digit_ranges(),
            'w' => word_ranges(),
            's' => space_ranges(),
            'n' => vec![('\n' as u32, '\n' as u32)],
            'r' => vec![('\r' as u32, '\r' as u32)],
            't' => vec![('\t' as u32, '\t' as u32)],
            '0' => vec![('\0' as u32, '\0' as u32)],
            other => vec![(other as u32, other as u32)],
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(pattern: &str) -> Vec<(u32, u32)> {
        match parse_regex(pattern).expect("parses") {
            Regex::Class(ranges) => ranges,
            other => panic!("expected class, got {other:?}"),
        }
    }

    #[test]
    fn literal_char_is_a_singleton_class() {
        assert_eq!(classes("a"), [('a' as u32, 'a' as u32)]);
    }

    #[test]
    fn class_ranges_and_negation() {
        assert_eq!(classes("[a-c]"), [('a' as u32, 'c' as u32)]);
        assert_eq!(
            classes("[^a-c]"),
            [
                (0, 'a' as u32 - 1),
                ('c' as u32 + 1, crate::regex::SCALAR_END - 1)
            ]
        );
    }

    #[test]
    fn shorthands_expand() {
        assert_eq!(classes(r"\d"), [('0' as u32, '9' as u32)]);
        assert_eq!(classes(r"\w").len(), 4);
        assert_eq!(classes(r"\s").len(), 3);
    }

    #[test]
    fn class_with_shorthand_and_range() {
        // [a-z\d_] merges into two ranges (a-z, underscore, 0-9 → sorted
        // merged: 0-9, A-Z? no — only a-z, 0-9, _).
        let ranges = classes("[\\d_a-z]");
        assert!(ranges.contains(&('0' as u32, '9' as u32)));
        assert!(ranges.contains(&('a' as u32, 'z' as u32)));
        assert!(ranges.contains(&('_' as u32, '_' as u32)));
    }

    #[test]
    fn dot_excludes_newline() {
        let ranges = classes(".");
        assert!(
            !ranges
                .iter()
                .any(|&(lo, hi)| ('\n' as u32) >= lo && ('\n' as u32) <= hi)
        );
    }

    #[test]
    fn quantifier_shapes() {
        assert!(matches!(parse_regex("a*").expect("parses"), Regex::Star(_)));
        assert!(matches!(parse_regex("a+").expect("parses"), Regex::Plus(_)));
        assert!(matches!(parse_regex("a?").expect("parses"), Regex::Opt(_)));
        assert!(matches!(
            parse_regex("a{2,4}").expect("parses"),
            Regex::Repeat {
                min: 2,
                max: Some(4),
                ..
            }
        ));
        assert!(matches!(
            parse_regex("a{2,}").expect("parses"),
            Regex::Repeat {
                min: 2,
                max: None,
                ..
            }
        ));
        assert!(matches!(
            parse_regex("a{3}").expect("parses"),
            Regex::Repeat {
                min: 3,
                max: Some(3),
                ..
            }
        ));
    }

    #[test]
    fn brace_quantifier_edges() {
        // {m,n} with max < min errors; over-cap errors; `{` literal falls
        // back when not a quantifier.
        assert!(parse_regex("a{4,2}").is_err());
        assert!(parse_regex("a{257}").is_err());
        // `a{b`: `{b` is not a quantifier → literal concatenation.
        assert!(matches!(
            parse_regex("a{b").expect("parses"),
            Regex::Concat(parts) if parts.len() == 3
        ));
    }

    #[test]
    fn alternation_and_grouping_nest() {
        let regex = parse_regex("(ab|c)+").expect("parses");
        assert!(matches!(regex, Regex::Plus(_)));
    }

    #[test]
    fn escaped_metacharacters_are_literals() {
        assert_eq!(classes(r"\."), [('.' as u32, '.' as u32)]);
        assert_eq!(classes(r"\\"), [('\\' as u32, '\\' as u32)]);
    }

    #[test]
    fn parse_errors_carry_positions() {
        let err = parse_regex("(ab").expect_err("unbalanced");
        assert!(err.message.contains('`'));
        assert_eq!(err.position, 3);
        assert!(parse_regex("*a").is_err());
        assert!(parse_regex("[a-").is_err());
        assert!(parse_regex("[]").is_err());
    }

    #[test]
    fn normalize_merges_touching_ranges() {
        assert_eq!(
            normalize_ranges(&[(0, 9), (10, 20), (30, 40)]),
            [(0, 20), (30, 40)]
        );
    }
}

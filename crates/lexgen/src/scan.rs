//! Scanning (issue 014): longest-match over the DFA with
//! declaration-order tie-breaks (the keyword strategy — declare keywords
//! before the identifier regex and they win ties), zero-length match
//! alignment (ε-accepting patterns never emit, so scanning always makes
//! progress) and trivia classification straight from the token table.

use gir_ir::{Grammar, TokenPattern};

use crate::dfa::Dfa;
use crate::nfa::{Fragment, Nfa};
use crate::regex::{LexError, Regex, parse_regex};

/// One lexeme produced by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lexeme {
    /// A matched token (kind, byte range).
    Token {
        /// The token kind.
        kind: u16,
        /// Byte range (clean — trivia is not part of token events,
        /// ADR-008).
        range: std::ops::Range<u32>,
        /// Whether the token is trivia-classified.
        trivia: bool,
    },
    /// An unmatched character (one char wide, guarantees progress).
    Error {
        /// Byte range of the offending character.
        range: std::ops::Range<u32>,
    },
}

/// The compiled lexer for a grammar's token table.
#[derive(Debug, Clone)]
pub struct Scanner {
    dfa: Dfa,
    trivia: Vec<bool>,
}

/// Compile a grammar's tokens into a scanner. Token declaration order is
/// the tie-break priority (keywords first ⇒ keywords win equal-length
/// matches).
pub fn compile(grammar: &Grammar) -> Result<Scanner, LexError> {
    let mut fragments: Vec<Fragment> = Vec::new();
    let mut pattern_of: Vec<u16> = Vec::new();
    for (idx, token) in grammar.tokens.iter().enumerate() {
        let regex: Regex = match &token.pattern {
            TokenPattern::Lit { text } => literal_regex(text),
            TokenPattern::Re { regex } => parse_regex(regex)?,
        };
        fragments.push(Nfa::build_fragment(&regex));
        pattern_of.push(u16::try_from(idx).expect("token count fits u16"));
    }
    let nfa = Nfa::merge(&fragments, &pattern_of);
    let dfa = Dfa::from_nfa(&nfa);
    let trivia = grammar.tokens.iter().map(|token| token.trivia).collect();
    Ok(Scanner { dfa, trivia })
}

/// A literal token as a regex: every char escaped by construction (each
/// becomes its own class).
fn literal_regex(text: &str) -> Regex {
    Regex::Concat(
        text.chars()
            .map(|c| Regex::Class(vec![(c as u32, c as u32)]))
            .collect(),
    )
}

impl Scanner {
    /// Scan the whole source into lexemes (token events + trivia flags,
    /// unmatched chars as [`Lexeme::Error`]).
    #[must_use]
    pub fn scan(&self, source: &str) -> Vec<Lexeme> {
        let bytes = source.as_bytes();
        let mut lexemes = Vec::new();
        let mut pos = 0usize;
        while pos < bytes.len() {
            if let Some((len, kind)) = self.longest_match(source, pos) {
                let end = pos + len;
                lexemes.push(Lexeme::Token {
                    kind,
                    range: u32::try_from(pos).expect("offset fits u32")
                        ..u32::try_from(end).expect("offset fits u32"),
                    trivia: self.trivia[kind as usize],
                });
                pos = end;
            } else {
                // No match: consume one char as an error lexeme.
                let mut end = pos + 1;
                while end < bytes.len() && !source.is_char_boundary(end) {
                    end += 1;
                }
                lexemes.push(Lexeme::Error {
                    range: u32::try_from(pos).expect("offset fits u32")
                        ..u32::try_from(end).expect("offset fits u32"),
                });
                pos = end;
            }
        }
        lexemes
    }

    /// Longest match at `pos`: `(byte length, token kind)`, or `None`.
    /// Zero-length (ε) accepts are never returned — that is the
    /// zero-length alignment guaranteeing progress.
    #[must_use]
    pub fn longest_match(&self, source: &str, pos: usize) -> Option<(usize, u16)> {
        let bytes = source.as_bytes();
        let mut state = self.dfa.start;
        let mut best: Option<(usize, u16)> = None;
        let mut cursor = pos;
        // The start state itself may accept; that is a zero-length match
        // and is deliberately ignored (progress guarantee).
        while cursor < bytes.len() {
            // Decode one char (patterns are scalar-based).
            let rest = &source[cursor..];
            let c = rest.chars().next()?;
            let class = self.dfa.classes.class_of(c as u32);
            let Some(next) = self.dfa.transitions[state as usize][class] else {
                break;
            };
            state = next;
            cursor += c.len_utf8();
            if let Some(pattern) = self.dfa.accept[state as usize] {
                best = Some((cursor - pos, pattern));
            }
        }
        best
    }

    /// The number of tokens the scanner knows about.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.trivia.len()
    }

    /// The number of DFA states (char-class compressed).
    #[must_use]
    pub fn dfa_len(&self) -> usize {
        self.dfa.len()
    }
}

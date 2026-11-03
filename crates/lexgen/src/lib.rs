//! # lexgen — the lexer compiler (issue 014, §4.2)
//!
//! Token patterns ([`TokenPattern::Lit`] and the regex subset in
//! [`TokenPattern::Re`]) compile to a Thompson NFA (one fragment per
//! pattern, merged with target rebasing), then to a DFA over compressed
//! character classes.
//!
//! Matching semantics:
//! - **longest match** wins (the scanner never backtracks the DFA);
//! - **declaration order** breaks equal-length ties — declare keywords
//!   before the identifier regex and keywords win the tie;
//! - **zero-length matches are discarded** (ε-accepting patterns cannot
//!   stall the scanner — the zero-length alignment rule);
//! - unmatched characters become one-char [`Lexeme::Error`] lexemes, so
//!   scanning always makes progress and stays lossless.

pub mod dfa;
pub mod nfa;
pub mod regex;
pub mod scan;

pub use dfa::{CharClasses, Dfa};
pub use nfa::{Fragment, Nfa};
pub use regex::{LexError, Regex, parse_regex};
pub use scan::{Lexeme, Scanner, compile};

// Re-export for the pattern type used by callers.
pub use gir_ir::TokenPattern;

//! The surface document: span-preserving shape of a `.gir` KDL file
//! (issue 004). The loader produces it; rendering reproduces it; the
//! IR lowering (`to_grammar`) consumes it.

use crate::error::ByteSpan;
use gir_ir::PrattHint;

/// `language` node: metadata + capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageMeta {
    /// Language name (e.g. `"json"`).
    pub name: String,
    /// Version string (e.g. `"1.0.0"`); empty when absent.
    pub version: String,
    /// File extensions (without the dot), in declaration order.
    pub extensions: Vec<String>,
    /// Unicode-aware lexing flag.
    pub unicode: bool,
    /// The `language` node span.
    pub span: ByteSpan,
}

/// What a declared token matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKindSurf {
    /// Fixed literal text (`kind=lit "{"`).
    Lit {
        /// The literal text.
        text: String,
    },
    /// A regex (`kind=re "[0-9]+"`).
    Re {
        /// The regex source.
        regex: String,
    },
}

/// One `tokens` block entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDecl {
    /// Token name (surface identifier).
    pub name: String,
    /// What the token matches.
    pub kind: TokenKindSurf,
    /// Tree-visible leaf (`node=true`).
    pub node: bool,
    /// Whitespace/comment class (`trivia=true`).
    pub trivia: bool,
    /// Structural-index boundary string (`index-bound="\""`).
    pub index_bound: Option<String>,
    /// The whole node span.
    pub span: ByteSpan,
    /// The name identifier span.
    pub name_span: ByteSpan,
}

/// An element inside a `(seq)` body: the item plus its byte span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqEl {
    /// The item.
    pub item: ItemSurf,
    /// A backward-binding marker (`opt`/`rep`/`rep1`/`and`/`not`), if any.
    pub marker: Option<Marker>,
    /// Span of the item text (identifier or quoted literal).
    pub span: ByteSpan,
}

/// One element of a `(seq)` rule body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemSurf {
    /// Reference to a declared rule or token by name.
    Ref {
        /// The referenced name.
        name: String,
    },
    /// Anonymous literal (quoted string): auto-declared as a token.
    Lit {
        /// The literal text.
        text: String,
    },
}

/// Backward-binding markers in `(seq)` bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `opt` — previous element becomes optional.
    Opt,
    /// `rep` — previous element repeats zero or more times.
    Rep,
    /// `rep1` — previous element repeats one or more times.
    Rep1,
    /// `and` — previous element becomes a positive lookahead.
    And,
    /// `not` — previous element becomes a negative lookahead.
    Not,
}

impl Marker {
    /// Parse a marker word.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "opt" => Some(Self::Opt),
            "rep" => Some(Self::Rep),
            "rep1" => Some(Self::Rep1),
            "and" => Some(Self::And),
            "not" => Some(Self::Not),
            _ => None,
        }
    }

    /// The marker's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Opt => "opt",
            Self::Rep => "rep",
            Self::Rep1 => "rep1",
            Self::And => "and",
            Self::Not => "not",
        }
    }
}

/// `kind=sep` configuration: `element (separator element)* [separator]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SepCfg {
    /// The element reference name.
    pub element: String,
    /// Span of the element name.
    pub element_span: ByteSpan,
    /// The separator literal text.
    pub separator: String,
    /// Trailing separator allowed (`allow-trailing=true`).
    pub allow_trailing: bool,
}

/// A rule body kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKindSurf {
    /// `kind=alt`: ordered choice of rule/token references.
    Alt {
        /// The alternatives (name + span each).
        refs: Vec<(String, ByteSpan)>,
    },
    /// `kind=seq`: ordered sequence of items with optional markers.
    Seq {
        /// The items in order.
        items: Vec<SeqEl>,
    },
    /// `kind=sep`: separated-list sugar.
    Sep(SepCfg),
}

/// One `grammar` block entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDecl {
    /// Rule name (surface identifier).
    pub name: String,
    /// The body.
    pub kind: RuleKindSurf,
    /// Tree node emission (`node=true`).
    pub node: bool,
    /// Incremental-reuse boundary (`reuse=true`).
    pub reuse: bool,
    /// Error-wrapping recovery policy (`wrap-error=true`).
    pub wrap_error: bool,
    /// Pratt precedence hint (`lbp=`/`rbp=`/`assoc=` properties).
    pub pratt: Option<PrattHint>,
    /// The whole node span.
    pub span: ByteSpan,
    /// The name identifier span.
    pub name_span: ByteSpan,
}

/// The span-preserving surface document for a `.gir` package file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GirDoc {
    /// The `language` metadata.
    pub language: LanguageMeta,
    /// Declared tokens, in surface order.
    pub tokens: Vec<TokenDecl>,
    /// Declared rules, in surface order.
    pub rules: Vec<RuleDecl>,
}

//! Surface errors with byte spans (issue 004: error reporting with spans).

use std::fmt;

/// A byte span into the KDL source text (`start..end`, byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ByteSpan {
    /// Start byte offset.
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

impl ByteSpan {
    /// A span from a KDL/miette [`SourceSpan`] (offset + length).
    #[must_use]
    pub fn new(start: usize, len: usize) -> Self {
        Self {
            start,
            end: start + len,
        }
    }

    /// The empty span at `offset`.
    #[must_use]
    pub const fn empty(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// Slice the source text this span covers (clamped — spans from
    /// hand-built documents may exceed the source; slicing never panics).
    #[must_use]
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        let len = source.len();
        let start = self.start.min(len);
        let end = self.end.clamp(start, len);
        &source[start..end]
    }
}

impl From<miette::SourceSpan> for ByteSpan {
    fn from(span: miette::SourceSpan) -> Self {
        Self::new(span.offset(), span.len())
    }
}

/// An error while loading a `.gir` KDL surface into a [`GirDoc`]
/// (`crate::GirDoc`), always carrying the offending [`ByteSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    /// The KDL text itself failed to parse.
    Kdl {
        /// Where the parse failed.
        span: ByteSpan,
        /// The parser's message.
        message: String,
    },
    /// A top-level node that is not `language`/`tokens`/`grammar`.
    UnknownSection {
        /// The section name.
        name: String,
        /// The node span.
        span: ByteSpan,
    },
    /// `language` appears more than once.
    DuplicateLanguage {
        /// The second node's span.
        span: ByteSpan,
    },
    /// No `language` node at the top level.
    MissingLanguage,
    /// `language` lacks its name argument.
    MissingLanguageName {
        /// The node span.
        span: ByteSpan,
    },
    /// A name is declared twice (rules and tokens share the surface
    /// namespace) or collides with a reserved marker word.
    DuplicateName {
        /// The duplicated name.
        name: String,
        /// The second declaration's name span.
        span: ByteSpan,
    },
    /// A name uses a reserved marker word (`opt`, `rep`, `rep1`, `and`,
    /// `not` bind backward in `(seq)` bodies).
    ReservedName {
        /// The reserved word.
        name: String,
        /// The name span.
        span: ByteSpan,
    },
    /// A token/rule `kind` property is not one of the known kinds.
    UnknownKind {
        /// The offending kind word.
        kind: String,
        /// The kind entry span.
        span: ByteSpan,
    },
    /// An entry has the wrong value type (e.g. `kind=3`).
    BadValue {
        /// What was expected.
        expected: &'static str,
        /// The entry span.
        span: ByteSpan,
    },
    /// A required argument/property is missing.
    MissingArg {
        /// What is missing.
        expected: &'static str,
        /// The node span.
        span: ByteSpan,
    },
    /// A `(seq)` marker (`opt`, `rep`, `rep1`, `and`, `not`) has no
    /// preceding element to bind to.
    DanglingMarker {
        /// The marker word.
        marker: String,
        /// The marker span.
        span: ByteSpan,
    },
    /// A rule reference names nothing declared.
    UnknownRef {
        /// The referenced name.
        name: String,
        /// The reference span.
        span: ByteSpan,
    },
    /// A Pratt property is malformed (bad binding power or associativity).
    BadPratt {
        /// Why.
        reason: String,
        /// The offending entry span.
        span: ByteSpan,
    },
    /// IR-level validation failure surfaced at build time (empty
    /// alternatives, builder errors).
    Ir {
        /// The offending rule's span.
        span: ByteSpan,
        /// The IR message.
        message: String,
    },
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kdl { span, message } => {
                write!(f, "KDL parse error at byte {}: {message}", span.start)
            }
            Self::UnknownSection { name, span } => {
                write!(f, "unknown section `{name}` at byte {}", span.start)
            }
            Self::DuplicateLanguage { span } => {
                write!(f, "`language` declared twice at byte {}", span.start)
            }
            Self::MissingLanguage => write!(f, "no `language` node at the top level"),
            Self::MissingLanguageName { span } => {
                write!(f, "`language` needs a name argument at byte {}", span.start)
            }
            Self::DuplicateName { name, span } => {
                write!(f, "`{name}` declared twice (byte {})", span.start)
            }
            Self::ReservedName { name, span } => {
                write!(
                    f,
                    "`{name}` is a reserved marker word (byte {})",
                    span.start
                )
            }
            Self::UnknownKind { kind, span } => {
                write!(f, "unknown kind `{kind}` at byte {}", span.start)
            }
            Self::BadValue { expected, span } => {
                write!(f, "expected {expected} at byte {}", span.start)
            }
            Self::MissingArg { expected, span } => {
                write!(f, "missing {expected} at byte {}", span.start)
            }
            Self::DanglingMarker { marker, span } => {
                write!(
                    f,
                    "marker `{marker}` at byte {} has no preceding element",
                    span.start
                )
            }
            Self::UnknownRef { name, span } => {
                write!(f, "reference to unknown `{name}` at byte {}", span.start)
            }
            Self::BadPratt { reason, span } => {
                write!(f, "bad pratt property at byte {}: {reason}", span.start)
            }
            Self::Ir { span, message } => {
                write!(f, "grammar invalid at byte {}: {message}", span.start)
            }
        }
    }
}

impl std::error::Error for SurfaceError {}

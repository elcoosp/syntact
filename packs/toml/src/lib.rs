//! # packs-toml — the TOML grammar pack (issue 025, roadmap §12.4)
//!
//! Reference TOML parser on the runtime-core event protocol: key/values
//! with dotted keys, basic + literal + multi-line strings, integers
//! (hex/oct/bin, underscores), floats, booleans, date-times, arrays,
//! inline tables, `[table]` / `[[array of tables]]` headers, `#`
//! comments as trivia.
//!
//! Kind numbering (documented mapping, mirroring the JSON pack):
//! - node kinds: [`kinds`], leaf tokens + punctuation: [`token_kinds`]
//!   (dense continuation),
//! - offending regions: [`runtime_core::ERROR_KIND`] nodes and tokens,
//! - implicit root: [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace AND `#` comments are never token events — they ride
//! [`TriviaRecord`]s (kinds [`token_kinds::WHITESPACE`] and
//! [`token_kinds::COMMENT`]) attached at tree-build time, so
//! `render(parse(x)) == x` byte-for-byte.

pub mod reference;

/// Node kinds for the TOML grammar (dense).
pub mod kinds {
    /// `[table]` header section.
    pub const TABLE: u16 = 0;
    /// `[[array of tables]]` header section.
    pub const ARRAY_TABLE: u16 = 1;
    /// `key = value` entry (dotted keys included).
    pub const KEY_VALUE: u16 = 2;
    /// A (dotted) key path: key parts joined by `.` tokens.
    pub const KEY: u16 = 3;
    /// `[a, b, …]` array value.
    pub const ARRAY: u16 = 4;
    /// `{ k = v, … }` inline table value.
    pub const INLINE_TABLE: u16 = 5;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// A bare key segment (`[A-Za-z0-9_-]+`).
    pub const BARE_KEY: u16 = 6;
    /// A quoted key or string value (basic/literal/multiline).
    pub const STRING: u16 = 7;
    /// An integer literal (dec/hex/oct/bin, underscores).
    pub const INTEGER: u16 = 8;
    /// A float literal (incl. `inf`/`nan`).
    pub const FLOAT: u16 = 9;
    /// `true` / `false`.
    pub const BOOLEAN: u16 = 10;
    /// Offset date-time / local date-time / local date / local time.
    pub const DATETIME: u16 = 11;
    /// `[`
    pub const LBRACKET: u16 = 12;
    /// `]`
    pub const RBRACKET: u16 = 13;
    /// `[[`
    pub const LDOUBLE: u16 = 14;
    /// `]]`
    pub const RDOUBLE: u16 = 15;
    /// `=`
    pub const EQUALS: u16 = 16;
    /// `.` (dotted key separator)
    pub const DOT: u16 = 17;
    /// `,`
    pub const COMMA: u16 = 18;
    /// `{`
    pub const LBRACE: u16 = 19;
    /// `}`
    pub const RBRACE: u16 = 20;
    /// Whitespace trivia: never a token event (ADR-008).
    pub const WHITESPACE: u16 = 21;
    /// `# …` comment trivia: never a token event (ADR-008).
    pub const COMMENT: u16 = 22;
}

/// Implicit document-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=22` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse TOML source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *TOML* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

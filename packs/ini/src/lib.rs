//! # packs-ini — community INI pack (issue 040)
//!
//! Reference INI parser on the runtime-core event protocol, at the
//! flagship TDD bar: `[section]` headers, `key = value` settings, `;`
//! and `#` comments as trivia, blank lines as whitespace trivia.
//!
//! Kind numbering (mirroring the other packs):
//! - node kinds: [`kinds`], tokens: [`token_kinds`] (dense continuation),
//! - offending regions: [`runtime_core::ERROR_KIND`] nodes and tokens,
//! - implicit root: [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace (including the newlines that terminate lines) and
//! comments are never token events — they ride [`TriviaRecord`]s
//! attached at tree-build time, so `render(parse(x)) == x`
//! byte-for-byte.

pub mod reference;

/// Node kinds for the INI grammar (dense).
pub mod kinds {
    /// `[section]` header.
    pub const SECTION: u16 = 0;
    /// `key = value` entry (the value may be absent).
    pub const SETTING: u16 = 1;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// `[`
    pub const LBRACKET: u16 = 2;
    /// `]`
    pub const RBRACKET: u16 = 3;
    /// `=`
    pub const EQUALS: u16 = 4;
    /// A section name or setting key (raw run, untrimmed).
    pub const BARE: u16 = 5;
    /// A setting value (raw run to end of line, may be empty).
    pub const VALUE: u16 = 6;
    /// Whitespace trivia: never a token event (ADR-008).
    pub const WHITESPACE: u16 = 7;
    /// `; …` / `# …` comment trivia: never a token event (ADR-008).
    pub const COMMENT: u16 = 8;
}

/// Implicit document-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=8` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse INI source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *INI* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

//! # packs-csv — community CSV pack (issue 040)
//!
//! Reference CSV (RFC 4180 subset) parser on the runtime-core event
//! protocol, at the flagship TDD bar: comma-separated rows, quoted
//! fields with `""` escapes (which may span lines and contain commas),
//! empty fields, and newline-terminated rows.
//!
//! Kind numbering (mirroring the other packs):
//! - node kinds: [`kinds`], tokens: [`token_kinds`] (dense continuation),
//! - offending regions: [`runtime_core::ERROR_KIND`] nodes and tokens,
//! - implicit root: [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! CSV has no comments; the only trivia is stray whitespace *between*
//! tokens (e.g. `"a" , b`). Newlines are **structural tokens** (row
//! terminators), never trivia. Tokens + trivia partition `[0, len)` so
//! `render(parse(x)) == x` byte-for-byte.

pub mod reference;

/// Node kinds for the CSV grammar (dense).
pub mod kinds {
    /// One output record: fields separated by commas.
    pub const ROW: u16 = 0;
    /// One field: quoted or bare.
    pub const FIELD: u16 = 1;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// `,`
    pub const COMMA: u16 = 2;
    /// A `"…"` field (with `""` escapes; may span lines).
    pub const QUOTED: u16 = 3;
    /// A raw run to the next `,`/newline (may be zero-width).
    pub const BARE: u16 = 4;
    /// `\n` or `\r\n` — the row terminator (structural).
    pub const NEWLINE: u16 = 5;
    /// Stray whitespace between tokens: never a token event (ADR-008).
    pub const WHITESPACE: u16 = 6;
}

/// Implicit document-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=6` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse CSV source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *CSV* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

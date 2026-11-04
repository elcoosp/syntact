//! # packs-markdown — community Markdown pack (issue 040)
//!
//! Reference Markdown-subset parser on the runtime-core event protocol,
//! at the flagship TDD bar: ATX headings (`#`–`######`), paragraph
//! lines, blank-line separation. Emphasis, lists, and fences are outside
//! the v0 subset — their bytes ride text tokens losslessly.
//!
//! Kind numbering (mirroring the other packs):
//! - node kinds: [`kinds`], tokens: [`token_kinds`] (dense continuation),
//! - offending regions: [`runtime_core::ERROR_KIND`] nodes and tokens,
//! - implicit root: [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Leading whitespace of a line and blank lines are whitespace trivia;
//! the newline that terminates a line is a **structural token** (it
//! ends paragraphs, like CSV rows). Tokens + trivia partition
//! `[0, len)` so `render(parse(x)) == x` byte-for-byte.

pub mod reference;

/// Node kinds for the Markdown-subset grammar (dense).
pub mod kinds {
    /// An ATX heading: `# text`.
    pub const HEADING: u16 = 0;
    /// One paragraph line of plain text.
    pub const PARAGRAPH: u16 = 1;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// `#`…`######` (1–6 hashes followed by a space or line end).
    pub const ATX: u16 = 2;
    /// The text run to the line end (may be empty after a bare `#`).
    pub const TEXT: u16 = 3;
    /// `\n` — the line terminator (structural).
    pub const NEWLINE: u16 = 4;
    /// Leading spaces/tabs and blank lines: never a token event.
    pub const WHITESPACE: u16 = 5;
}

/// Implicit document-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=5` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse Markdown source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *Markdown* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

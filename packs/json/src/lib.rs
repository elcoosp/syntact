//! # packs-json — the JSON grammar pack (roadmap §12.4, P0 gate)
//!
//! End-to-end reference pack: the hand-written **reference parser** on the
//! event protocol (the differential oracle — the hand-written parser
//! precedes codegen so generation chases a known-good shape).
//!
//! P0 exit gate (roadmap §12.3): JSON pack parses the official JSON test
//! suite subset; byte-faithful on corpus; fuzz-clean on losslessness.

pub mod reference;

/// Node kinds for the JSON grammar (dense, mirrors grammar.kdl ordering).
pub mod kinds {
    // Missing-docs is satisfied by the enum-like const doc lines below;
    // each constant carries its own doc comment.

    /// `object`
    pub const OBJECT: u16 = 0;
    /// `array`
    pub const ARRAY: u16 = 1;
    /// `pair` (name : value)
    pub const PAIR: u16 = 2;
    /// `string` (both object keys and string values)
    pub const STRING: u16 = 3;
    /// `number`
    pub const NUMBER: u16 = 4;
    /// `true`
    pub const TRUE: u16 = 5;
    /// `false`
    pub const FALSE: u16 = 6;
    /// `null`
    pub const NULL: u16 = 7;
}

/// Parse JSON with the reference parser, returning the green tree plus the
/// diagnostic side table (implemented in issue 019).
///
/// # Errors
/// Never fails structurally; malformed input produces ERROR events and a
/// non-zero error count (error recovery keeps the result lossless).
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;

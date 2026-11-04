//! # runtime-core — the event protocol, input layer, sinks, and green trees (§7)
//!
//! The runtime kernel every pack parser emits into and every consumer reads
//! from (roadmap §7). Core pieces:
//!
//! - [`events`] — the 8-byte event protocol ([`events::Event`]), packed
//!   [`events::Output`] streams, and [`events::process`] validation.
//! - [`input`] — the [`input::Source`] input layer (line index, byte-accurate
//!   positions, u32 length guard).
//! - [`sink`] — the [`sink::Sink`] consumer trait ([`sink::drive_sink`]), the
//!   validate/scan/skeleton mode sinks, and trivia records (ADR-008).
//! - [`tree`] — green trees with structural sharing ([`tree::GreenNode`],
//!   [`tree::TreeBuilder`]), byte-faithful rendering, interning.
//! - [`diagnostic::TokenSet`] + [`diagnostic`] — 192-bit expected sets and the
//!   diagnostic side table.

pub mod diagnostic;
pub mod events;
pub mod input;
pub mod persistent;
pub mod sink;
pub mod tree;

/// Node kind identifier (pack-local dense numbering; sentinels below).
pub type NodeKind = u16;

/// Token kind identifier (pack-local dense numbering; sentinels below).
pub type TokenKind = u16;

/// Error node kind: recovery wraps an offending region in a node of this
/// kind ([`crate::events::Event::Error`] is immediately followed by
/// `Start { kind: ERROR_KIND }` in every pack's discipline).
pub const ERROR_KIND: NodeKind = u16::MAX - 1;

/// Missing-token marker kind (biome's missing-marker convention): a
/// zero-width synthesized leaf.
pub const MISSING_KIND: NodeKind = u16::MAX - 2;

/// Pseudo-EOF token kind, synthesized by the tree builder at document end
/// (never contributes bytes — it is zero-width at the source end).
pub const EOF_KIND: TokenKind = u16::MAX;

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn sentinels_are_collision_free() {
        // Dense pack numbering lives in 0..=u16::MAX-4; the four sentinels
        // stack above it without overlap.
        let kinds = [MISSING_KIND, ERROR_KIND, EOF_KIND];
        assert!(
            kinds.windows(2).all(|w| w[0] < w[1]),
            "sentinels strictly ordered"
        );
    }
}

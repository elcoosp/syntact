//! # gir-ir — the canonical typed grammar IR (roadmap §5)
//!
//! One typed Rust shape every stage shares: KDL (gir-syntax) and importers
//! build it, analysis/transform/codegen consume it — nothing re-parses
//! strings.
//!
//! - [`interner`] — stable interned name handles ([`interner::Sym`]).
//! - [`ir`] — the IR itself: [`ir::Grammar`], [`ir::Rule`], [`ir::Element`],
//!   [`ir::TokenDef`], annotations ([`ir::PrattHint`], sync/insert sets),
//!   plus the validating [`ir::GrammarBuilder`].

pub mod interner;
pub mod ir;

pub use interner::{Interner, Sym};
pub use ir::{
    Associativity, Element, Grammar, GrammarBuilder, IrError, PrattHint, Rule, RuleId, TokenDef,
    TokenId, TokenPattern,
};

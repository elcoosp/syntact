//! # gir-transform — grammar-to-grammar passes (issue 013)
//!
//! Every transform consumes and produces the canonical typed IR
//! ([`Grammar`] → [`Grammar`]); nothing re-parses strings.
//!
//! - [`lower_pratt`] — pratt-hinted left-recursive rules → `atom (tail)*`
//!   repetition form (binding powers stay in the [`PrattHint`] annotation).
//! - [`inline_single_use`] — cycle-guarded folding of transparent
//!   single-use rules into their only call site.
//! - [`left_factor`] — common-prefix hoisting over every `Alt`, with a
//!   fixpoint.
//!
//! [`optimize`] runs the pipeline: pratt lowering, inlining, factoring.

pub mod factor;
pub mod inline;
pub mod pratt;

pub use factor::left_factor;
pub use gir_ir::{Grammar, PrattHint};
pub use inline::inline_single_use;
pub use pratt::lower_pratt;

/// The full transform pipeline, each stage to its fixpoint: pratt
/// lowering → cycle-guarded inlining → left factoring.
#[must_use]
pub fn optimize(grammar: &Grammar) -> Grammar {
    let lowered = lower_pratt(grammar);
    let inlined = inline_single_use(&lowered);
    left_factor(&inlined)
}

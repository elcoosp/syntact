//! # incremental — the incremental reparse engine (issue 030, §7.7)
//!
//! Token-stream reuse over the packs' lexers: edit normalization, a
//! single-token relex fast path with margin-based safety predicates
//! ([`engine::ContextHash`], window tiling, flank-kind agreement),
//! changed-ranges diffing, and seeded incremental-equivalence fuzzing
//! (`reparse(edit(x)) == fresh_parse(x)` at the token-stream level).
//!
//! # Pipeline
//!
//! 1. [`edit::Edit::normalize`] clips the edit to the source and
//!    canonicalizes the replaced range.
//! 2. The old token stream splits into *before* / *damaged* / *after*
//!    around the edited range.
//! 3. **Fast path** (at most one damaged token): relex the margin window
//!    spanning the flanking tokens; if the window tiles exactly and its
//!    flank kinds agree with the old neighbours (the context-hash
//!    predicates), splice the relexed window into the stream and shift
//!    the tail — no full relex.
//! 4. Otherwise the fallback relexes the whole new source; tokens whose
//!    kind and text survive outside the changed ranges are counted as
//!    reused (token-granularity [`engine::Stats`]).
//! 5. [`diff::changed_ranges`] reports the byte ranges that differ; the
//!    tests pin it against a brute-force diff.

pub mod diff;
pub mod edit;
pub mod engine;

pub use edit::Edit;
pub use engine::{
    ContextHash, Engine, Lexer, Outcome, Rejection, ReuseCursor, Stats, reused_subtree_bytes,
};

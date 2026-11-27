//! # incremental — the incremental reparse engine (issue 030, §7.7)
//!
//! Token-stream reuse over the packs' lexers: edit normalization, two
//! relex fast paths with safety predicates — the margin-window path for
//! single-token damage (window tiling, flank-kind + flank-text
//! agreement) and the §7.7 balanced-block path for multi-token damage
//! inside a bracket-balanced region (the smallest enclosing block
//! relexes alone; boundary kinds AND texts must survive) — both falling
//! through to the full relex on any rejection, changed-ranges diffing,
//! and seeded incremental-equivalence fuzzing (`reparse(edit(x)) ==
//! fresh_parse(x)` at the token-stream level).
//!
//! # Pipeline
//!
//! 1. [`edit::Edit::normalize`] clips the edit to the source and
//!    canonicalizes the replaced range.
//! 2. The old token stream splits into *before* / *damaged* / *after*
//!    around the edited range.
//! 3. **Fast path 1** (at most one damaged token): relex the margin
//!    window spanning the flanking tokens; if the window tiles exactly
//!    and its flank kinds agree with the old neighbours (the
//!    context-hash predicates), splice the relexed window into the
//!    stream and shift the tail — no full relex.
//! 4. **Fast path 2** (§7.7, multi-token damage in a bracket-balanced
//!    block, via [`Engine::incremental_edit_with_brackets`]): the
//!    smallest enclosing block relexes alone under the same predicate
//!    discipline (exact tiling, error-free fragment, boundary kind AND
//!    text agreement, pair-table balance); every rejection falls
//!    through — the method is a pure accelerator.
//! 5. Otherwise the fallback relexes the whole new source; tokens whose
//!    kind and text survive outside the changed ranges are counted as
//!    reused (token-granularity [`engine::Stats`]).
//! 5. [`diff::changed_ranges`] reports the byte ranges that differ; the
//!    tests pin it against a brute-force diff.
//! 6. [`tree_diff::changed_tree_ranges`] recomputes the damage at green-
//!    tree granularity — the roadmap §7.7 changed-ranges walk (pairwise
//!    old/new subtree classification) that downstream invalidation
//!    consumes; [`DiffVisitor`] is its observability seam.

pub mod diff;
pub mod edit;
pub mod engine;
pub mod tree_diff;

pub use edit::Edit;
pub use engine::{
    ContextHash, Engine, Lexer, Outcome, Rejection, ReuseCursor, Stats, reused_subtree_bytes,
};
pub use tree_diff::{DiffVisitor, RangeCollector, SubtreeVerdict, changed_tree_ranges, diff_trees};

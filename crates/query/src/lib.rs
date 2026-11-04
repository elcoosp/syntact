//! # query — the query compiler + streaming matcher (issue 031, §7.8)
//!
//! A tree-sitter-`.scm`-shaped query language compiled to flat
//! [`query::QueryStep`] arrays with O(1) kind-prefilter bitsets, matched
//! by a single-pass [`cursor::QueryCursor`] over the packed
//! [`runtime_core::events::Output`] stream — no tree construction.
//!
//! # Documented subset (v0)
//!
//! - **Patterns**: `(kind child1 child2 …)` with kind names resolved
//!   through a [`query::KindResolver`]; `_` wildcards; `@capture` names;
//!   child quantifiers `?` / `*` / `+` after a child pattern. One level
//!   of children (nested sub-patterns with grandchildren are a
//!   validation error, reported with the source span).
//! - **Matching**: a node matches a pattern when its kind matches and
//!   its child-kind sequence contains the child steps in order (gaps
//!   allowed, quantifiers per step). Evaluated when the node's `Exit`
//!   step streams by — the cursor buffers exactly one node's summary.
//! - **Predicates**: `(#eq? @cap "text")` / `(#not-eq? …)` compile to
//!   built-ins; arbitrary `(#name? args…)` dispatch to the host closure
//!   ([`cursor::PredicateFn`]). A match with a failing predicate is
//!   dropped.
//! - **Captures**: `(slot, span)` pairs; deduplicated per match and
//!   across matches.
//! - **O(1) alloc per candidate step**: the matcher runs on stack
//!   state; captures land in a caller-provided scratch buffer that is
//!   cleared (not reallocated) per candidate — the tests pin the
//!   capacity.
//!
//! # Query text grammar
//!
//! ```text
//! query      := pattern predicate*
//! pattern    := "(" kind capture? child* ")" | "_" capture?
//! child      := pattern quant? capture?
//! quant      := "?" | "*" | "+"
//! predicate  := "(" "#" name arg* ")"
//! capture    := "@" name
//! arg        := capture | string
//! ```

pub mod cursor;
pub mod query;

pub use cursor::{Capture, Match, PredicateArg, PredicateFn, QueryCursor};
pub use query::{
    ChildStep, CompiledQuery, KindBitset, KindResolver, PredArgSpec, Predicate, Quant, QueryError,
    QueryStep, compile, parse,
};

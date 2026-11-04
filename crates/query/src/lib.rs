//! # query — the query compiler + streaming cursor (issue 031, §7.8)
//!
//! Tree-sitter-shaped `.scm` queries over the packed event stream:
//!
//! ```scm
//! (call (identifier) @fn (arguments) @args) @call
//! (comment) @comment
//! ((identifier) @variable (#eq? @variable "x"))
//! ```
//!
//! - **Parse/validate with spans** — every query error carries the byte
//!   span of the offending text.
//! - **Compile** — patterns flatten to [`QueryStep`] arrays whose
//!   **kind-prefilter bitset** (`[u64; 3]`, 192 kinds) rejects a
//!   candidate in one AND+test.
//! - **Stream** — [`QueryCursor`] walks the packed [`Output`] steps (no
//!   tree materialization), reporting matches with **capture spans**
//!   (first token start → last token end, from the range table) and
//!   running **predicate closures** (`#eq?`).
//! - **O(1) alloc per candidate step** — the cursor's match scratch is
//!   allocated once per cursor and reused across candidates; the test
//!   suite pins that the scratch capacity never grows.
//! - **Capture dedup** — a match reports each (capture, span) pair once.

pub mod compile;
pub mod cursor;
pub mod parse;

pub use compile::{
    CompileError, CompiledQuery, KindResolver, PREFILTER_WORDS, QueryStep, SliceResolver, WILDCARD,
    bitset_allows, compile,
};
pub use cursor::{Match, QueryCursor};
pub use parse::{PredicateDef, QNode, Quant, QueryError, QuerySpec, parse_query};

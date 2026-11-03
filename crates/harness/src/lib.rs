//! # harness — conformance runner + corpus format + differential driver
//! (issue 017, §12 gates)
//!
//! One runner, three gates, per case:
//! 1. **Budgets** — recovered error count within the fixture's
//!    tolerated-error budget (0 means the fixture is valid and must be
//!    error-free);
//! 2. **Losslessness** — `render(parse(x)) == x` byte-for-byte;
//! 3. **Determinism + balance** — two parse runs produce identical event
//!    streams, and the wrapped stream validates through `events::process`.
//!
//! The corpus format is dependency-free text (diff-friendly, no escapes):
//!
//! ```text
//! # a comment
//! === case name
//! tolerated 0
//! ---
//! the raw source, every byte verbatim until the next `===` header
//! ```
//!
//! The differential stage drives TWO independent implementations (the
//! packs-json reference parser and the descent descriptor engine) over
//! the same inputs and compares the token shapes — the mechanics the
//! flagship packs reuse for their own oracles.

pub mod corpus;
pub mod differential;
pub mod metrics;
pub mod runner;

pub use corpus::{Case, Corpus};
pub use differential::{Differential, token_shape};
pub use metrics::MutationStats;
pub use runner::{Report, Summary, run_corpus};

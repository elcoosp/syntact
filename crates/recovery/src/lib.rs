//! # recovery — the deterministic cactus-stack repair planner (§7.5, issue 016)
//!
//! CPCT\*-shaped error repair: when the primary parser (descent, driven by
//! the LR shadow's states) hits a token the automaton cannot shift, the
//! planner searches **repair sequences** — inserts of expected tokens and
//! deletes of offending input — over the **cactus parse-stack prefixes**
//! (every search node owns a stack prefix; the search tree is the cactus).
//!
//! - **Uniform-cost search**: nodes expand in `(cost, ops)` order, so the
//!   first complete repair popped is the canonical minimal one.
//! - **Canonical ordering**: `RepairOp` has a total order (`Insert` <
//!   `Delete`, then by token) and plans carry their ops verbatim — equal
//!   costs resolve lexicographically, deterministically.
//! - **Work budget**: the budget counts *node expansions* (work), never
//!   wall-clock — CI-stable by construction.
//! - **Hint-driven sync tables**: grammar-derived sync tokens (closers,
//!   separators) get a delete-cost discount, steering repairs toward
//!   "skip to the next boundary" instead of token-by-token patching.
//!
//! The planner consults the shadow automaton only (`lr_shadow::Automaton`):
//! actions, gotos and per-state expected sets — the same tables the trace
//! CLI and the conformance harness use.

pub mod plan;

pub use plan::{CostModel, Planner, RepairOp, RepairPlan, SyncHints};

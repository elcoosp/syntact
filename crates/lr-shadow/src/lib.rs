//! # lr-shadow — canonical LR(1) shadow automaton (§4.2, issue 011)
//!
//! The shadow automaton is compiled alongside the primary descent backend
//! and supplies what descent does not compute itself: parse states (for
//! incremental reuse), expected-token sets (for parser-directed lexing and
//! diagnostics) and a deterministic table the recovery planner can consult.
//!
//! - [`lower`] — GIR element trees → flat productions (synthetic helpers
//!   for `Opt`/`Rep`, preserved predicates, pseudo-EOF sentinel).
//! - [`Automaton`] — canonical LR(1) kernel worklist construction with the
//!   conflict policy (shift-wins, silent synthetic ε/ε resolution),
//!   per-state expected sets and reachability.
//! - [`driver`] — the table-driven LR(1) runtime (roadmap §7.4 O1 tier):
//!   a deterministic stack machine over the `action`/`goto` tables, the
//!   table backend the `syntact test-all` oracle differential-tests
//!   against the descent engine.
//! - [`token_class`] — token-class inference (roadmap §6.1 pass 7):
//!   group tokens never distinguished in any parser state (the
//!   tree-sitter `ts_symbol_map` idea, extended) — the action-column
//!   equivalence over the resolved table, checked by driver verdict
//!   preservation under representative substitution.
//! - [`ambiguity`] — ambiguity marks (roadmap §6.2, ADR-007): recorded
//!   conflicts attributed back to GIR rules through production origins,
//!   with the two contending productions the determinism report renders
//!   as the counter-example derivations.
//! - [`island`] — the GLR island executor (roadmap §7.4, ADR-007): the
//!   runtime for the regions the analyzer marks ambiguous — the arena
//!   graph-structured stack with shared tails on fork, fork/merge caps
//!   (tree-sitter MAX_VERSION_COUNT-style), deterministic merge on equal
//!   `(state, position, cost)` and a work budget; forks at the recorded
//!   conflicts and hands back the winning derivation.

pub mod ambiguity;
pub mod automaton;
pub mod driver;
pub mod island;
pub mod lower;
pub mod token_class;

pub use ambiguity::{AmbiguityMark, ProductionSide, ambiguity_marks};
pub use automaton::{Action, Automaton, Conflict, ConflictKind, Item, State, StateId};
pub use driver::{ParseOutcome, StepError, parse};
pub use island::{IslandCaps, IslandOp, IslandRejection, IslandRun, RejectionReason};
pub use lower::{First, Look, Lowered, NontermId, Predicate, ProdKind, Production, Symbol};
pub use token_class::TokenClasses;

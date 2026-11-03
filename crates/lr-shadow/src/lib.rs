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

pub mod automaton;
pub mod lower;

pub use automaton::{Action, Automaton, Conflict, ConflictKind, Item, State, StateId};
pub use lower::{First, Look, Lowered, NontermId, Predicate, ProdKind, Production, Symbol};

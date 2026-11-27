//! # gir-analysis — FIRST/FOLLOW, LL(1) verdicts, determinism report
//!
//! Issues 010 + 012: fixpoint analysis over the canonical GIR.
//!
//! - [`first_follow`] — FIRST/NULLABLE and FOLLOW fixpoints.
//! - [`ll1`] — per-rule LL(1) verdicts with conflict enumeration.
//! - [`report`] — the determinism report (backend classification:
//!   pratt / descent / table, plus the §6.2 ambiguity marks with island
//!   candidates) consumed by codegen and `syntact check`.

pub mod first_follow;
pub mod ll1;
pub mod report;

pub use first_follow::{FirstSet, FirstSets, FollowSets, LookSet};
pub use ll1::{Conflict, Ll1Verdict, Obstruction};
pub use report::{
    Ambiguity, Backend, ConflictSite, DeterminismReport, RuleVerdict, left_recursion,
};

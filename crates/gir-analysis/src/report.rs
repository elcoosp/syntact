//! The determinism report (issue 012): per-rule LL(1)/Pratt verdicts,
//! conflict sites, and backend classification consumed by codegen.

use std::fmt;

use gir_ir::{Grammar, RuleId};

use crate::first_follow::{FirstSets, FollowSets};
use crate::ll1::Obstruction;

/// Which backend generates the code for a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Pratt precedence-climbing (rule carries a pratt hint; conflicts
    /// inside expression regions are expected and fine).
    Pratt,
    /// Deterministic recursive descent (LL(1)-dispatchable).
    Descent,
    /// Table-driven fallback (rule needs more than one-token lookahead
    /// or contains opaque constructs).
    Table,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pratt => write!(f, "pratt"),
            Self::Descent => write!(f, "descent"),
            Self::Table => write!(f, "table"),
        }
    }
}

/// One conflict site: rule, contended token, branch pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConflictSite {
    /// The rule with the conflict.
    pub rule: RuleId,
    /// The contended token.
    pub token: gir_ir::TokenId,
    /// Branch indices.
    pub branches: (usize, usize),
}

/// The verdict for one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVerdict {
    /// The rule.
    pub rule: RuleId,
    /// Backend classification.
    pub backend: Backend,
    /// Whether the rule is LL(1)-dispatchable.
    pub ll1: bool,
    /// Whether the rule is (indirectly) left-recursive — descent codegen
    /// rejects these unless a pratt hint lowered them.
    pub left_recursive: bool,
    /// Rule references only reachable rules see (informational).
    pub reachable: bool,
}

/// Full determinism classification for a grammar (issue 012): consumed
/// by codegen and by `syntact check`/`conflicts`.
#[derive(Debug, Clone)]
pub struct DeterminismReport {
    /// Per-rule verdicts, in rule-id order.
    pub verdicts: Vec<RuleVerdict>,
    /// Every LL(1) conflict site (unfiltered — pratt rules may carry
    /// conflicts that do not force the table backend).
    pub conflicts: Vec<ConflictSite>,
}

impl DeterminismReport {
    /// Classify a grammar: FIRST/FOLLOW fixpoints, LL(1) verdicts, left
    /// recursion, backend selection.
    #[must_use]
    pub fn compute(grammar: &Grammar) -> Self {
        let first = FirstSets::compute(grammar);
        let follow = FollowSets::compute(grammar, &first);
        let verdicts_ll1 = crate::ll1::ll1_verdicts(grammar, &first, &follow);
        let left_recursive = left_recursion(grammar);
        let unreachable: std::collections::BTreeSet<RuleId> =
            grammar.unreachable_rules().into_iter().collect();

        let mut conflicts = Vec::new();
        let verdicts: Vec<RuleVerdict> = grammar
            .rules
            .iter()
            .map(|rule| {
                let (_, ll1) = verdicts_ll1
                    .iter()
                    .find(|(id, _)| *id == rule.id)
                    .expect("verdict exists for every rule");
                if let Some(Obstruction::Conflicts(list)) = &ll1.obstruction {
                    for conflict in list {
                        conflicts.push(ConflictSite {
                            rule: rule.id,
                            token: conflict.token,
                            branches: conflict.branches,
                        });
                    }
                }
                let backend = if rule.pratt.is_some() {
                    Backend::Pratt
                } else if ll1.ok {
                    Backend::Descent
                } else {
                    Backend::Table
                };
                RuleVerdict {
                    rule: rule.id,
                    backend,
                    ll1: ll1.ok,
                    left_recursive: left_recursive[rule.id.0 as usize],
                    reachable: !unreachable.contains(&rule.id),
                }
            })
            .collect();
        Self {
            verdicts,
            conflicts,
        }
    }

    /// The backend chosen for a rule.
    #[must_use]
    pub fn backend_for(&self, rule: RuleId) -> Backend {
        self.verdicts
            .iter()
            .find(|verdict| verdict.rule == rule)
            .map_or(Backend::Table, |verdict| verdict.backend)
    }

    /// Whether every reachable rule classifies as deterministic
    /// (descent or pratt) — the grammar generates without table fallback.
    #[must_use]
    pub fn all_deterministic(&self) -> bool {
        self.verdicts
            .iter()
            .filter(|verdict| verdict.reachable)
            .all(|verdict| !matches!(verdict.backend, Backend::Table))
    }
}

impl fmt::Display for DeterminismReport {
    /// Deterministic snapshot form (issue 012 report snapshots):
    /// one line per rule + one line per conflict, in stable order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for verdict in &self.verdicts {
            writeln!(
                f,
                "rule {} backend={} ll1={} left_recursive={} reachable={}",
                verdict.rule.0,
                verdict.backend,
                verdict.ll1,
                verdict.left_recursive,
                verdict.reachable
            )?;
        }
        for conflict in &self.conflicts {
            writeln!(
                f,
                "conflict rule {} token {} branches ({}, {})",
                conflict.rule.0, conflict.token.0, conflict.branches.0, conflict.branches.1
            )?;
        }
        Ok(())
    }
}

/// Left-recursion (direct + indirect): `matrix[a][b]` is true when rule
/// `a` can begin with rule `b` through nullable prefixes. Boolean
/// fixpoint over a finite matrix — terminates.
#[must_use]
pub fn left_recursion(grammar: &Grammar) -> Vec<bool> {
    let first = FirstSets::compute(grammar);
    let n = grammar.rules.len();
    // begins_with[a][b]: a's body has b as a left corner (through
    // nullable prefixes).
    let mut begins_with: Vec<Vec<bool>> = vec![vec![false; n]; n];
    for rule in &grammar.rules {
        let mut corners: Vec<RuleId> = Vec::new();
        left_corners(&rule.body, &first.sets, &mut corners);
        for corner in corners {
            if let Ok(idx) = usize::try_from(corner.0) {
                if idx < n {
                    begins_with[rule.id.0 as usize][idx] = true;
                }
            }
        }
    }
    // Transitive closure (Warshall): a → k → b implies a begins with b.
    // Row-union formulation: when row `a` already reaches `k`, it also
    // reaches everything row `k` reaches. Monotone over a finite matrix
    // — the pass terminates after n iterations.
    for k in 0..n {
        let k_row = begins_with[k].clone();
        for row in &mut begins_with {
            if row[k] {
                for (cell, reach) in row.iter_mut().zip(k_row.iter().copied()) {
                    *cell |= reach;
                }
            }
        }
    }
    (0..n).map(|i| begins_with[i][i]).collect()
}

/// Collect the left corners of an element: rule references reachable
/// through nullable leading positions.
fn left_corners(
    element: &gir_ir::Element,
    first: &[crate::first_follow::FirstSet],
    out: &mut Vec<RuleId>,
) {
    match element {
        gir_ir::Element::Rule(id) => out.push(*id),
        gir_ir::Element::Token(_) | gir_ir::Element::Not(_) => {}
        gir_ir::Element::Seq(elements) => {
            for element in elements {
                left_corners(element, first, out);
                // Keep scanning only through nullable prefixes (AND
                // lookaheads consume nothing: they stay in the corner
                // chain too).
                let mut set = crate::first_follow::FirstSet::default();
                crate::first_follow::first_of(element, first, &mut set);
                let nullable = set.nullable || set.opaque;
                if !nullable {
                    break;
                }
            }
        }
        gir_ir::Element::Alt(branches) => {
            for branch in branches {
                left_corners(branch, first, out);
            }
        }
        gir_ir::Element::Opt(inner) | gir_ir::Element::Rep { inner, .. } => {
            left_corners(inner, first, out);
        }
        gir_ir::Element::And(inner) => left_corners(inner, first, out),
    }
}

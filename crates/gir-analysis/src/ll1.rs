//! LL(1) verdicts per rule (issue 010): pairwise FIRST-disjointness over
//! alternatives, nullable-branch FOLLOW interaction, and the
//! backtracking flag for rules containing negative lookaheads.

use gir_ir::{Element, Grammar, RuleId, TokenId};

use crate::first_follow::{FirstSets, FollowSets};

/// One LL(1) conflict: two alternatives of a rule contend for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Conflict {
    /// The conflicting token.
    pub token: TokenId,
    /// Indices of the two conflicting branches in the rule's `Alt`.
    pub branches: (usize, usize),
}

/// Why a rule is not LL(1)-dispatchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obstruction {
    /// Alternatives contend on lookahead tokens.
    Conflicts(Vec<Conflict>),
    /// The rule contains a negative lookahead (`Not`): its FIRST
    /// contribution is not a token set, so no LL(1) table exists.
    Opaque,
}

/// The LL(1) verdict for one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ll1Verdict {
    /// `true` when an LL(1) parser can dispatch this rule on one token
    /// of lookahead.
    pub ok: bool,
    /// Why the verdict is negative, when it is.
    pub obstruction: Option<Obstruction>,
}

impl Ll1Verdict {
    /// An unconditional pass.
    #[must_use]
    pub const fn pass() -> Self {
        Self {
            ok: true,
            obstruction: None,
        }
    }
}

/// Compute the LL(1) verdict for every rule.
///
/// The verdict is meaningful only for reachable rules with an `Alt`
/// body — `Seq` bodies are deterministic by construction; unreachable
/// rules report [`Ll1Verdict::pass`] (they generate no code).
#[must_use]
pub fn ll1_verdicts(
    grammar: &Grammar,
    first: &FirstSets,
    follow: &FollowSets,
) -> Vec<(RuleId, Ll1Verdict)> {
    grammar
        .rules
        .iter()
        .map(|rule| (rule.id, verdict_for(grammar, first, follow, rule.id)))
        .collect()
}

fn verdict_for(
    grammar: &Grammar,
    first: &FirstSets,
    follow: &FollowSets,
    id: RuleId,
) -> Ll1Verdict {
    let rule = grammar.rule(id);
    let Element::Alt(branches) = &rule.body else {
        // Sequences (and predicate-wrapped bodies) dispatch by position.
        return Ll1Verdict::pass();
    };
    let sets: Vec<crate::first_follow::FirstSet> = branches
        .iter()
        .map(|branch| {
            let mut set = crate::first_follow::FirstSet::default();
            crate::first_follow::first_of(branch, &first.sets, &mut set);
            set
        })
        .collect();
    if sets.iter().any(crate::first_follow::FirstSet::is_opaque) {
        return Ll1Verdict {
            ok: false,
            obstruction: Some(Obstruction::Opaque),
        };
    }
    let mut conflicts: Vec<Conflict> = Vec::new();
    for (i, a) in sets.iter().enumerate() {
        for (j, b) in sets.iter().enumerate().skip(i + 1) {
            for token in a.tokens.intersection(&b.tokens) {
                conflicts.push(Conflict {
                    token: *token,
                    branches: (i, j),
                });
            }
        }
    }
    for (i, a) in sets.iter().enumerate() {
        if !a.nullable {
            continue;
        }
        for token in &follow.follow(id).tokens {
            if a.tokens.contains(token) {
                continue;
            }
            // A nullable branch and another branch that can also be
            // entered on a FOLLOW token of the rule contend when any
            // other branch is nullable OR starts with that token.
            if let Some((j, _)) = sets
                .iter()
                .enumerate()
                .find(|(j, b)| *j != i && (b.nullable || b.tokens.contains(token)))
            {
                conflicts.push(Conflict {
                    token: *token,
                    branches: (i, j),
                });
            }
        }
    }
    if conflicts.is_empty() {
        Ll1Verdict::pass()
    } else {
        Ll1Verdict {
            ok: false,
            obstruction: Some(Obstruction::Conflicts(conflicts)),
        }
    }
}

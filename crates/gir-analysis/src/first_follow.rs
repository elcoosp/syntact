//! FIRST/NULLABLE/FOLLOW fixpoints over the GIR (issue 010).
//!
//! Classic fixpoint analysis on the canonical IR: [`FirstSets`] holds
//! per-rule FIRST token sets + nullability; [`FollowSets`] holds per-rule
//! FOLLOW sets (+ EOF). Both are monotone over finite lattices — the
//! fixpoint always terminates.

use std::collections::BTreeSet;

use gir_ir::{Element, Grammar, RuleId, TokenId};

/// FIRST + NULLABLE for one rule (or one element, transiently).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirstSet {
    /// Tokens that can start this rule/element.
    pub tokens: BTreeSet<TokenId>,
    /// Whether the rule/element can match the empty string (ε).
    pub nullable: bool,
    /// Whether the tree contains an opaque construct (a negative
    /// lookahead `Not`) whose FIRST contribution is not representable —
    /// rules marked opaque need backtracking (see [`crate::ll1`]).
    pub opaque: bool,
}

impl FirstSet {
    /// Whether this set is opaque (contains a negative lookahead).
    #[must_use]
    pub const fn is_opaque(&self) -> bool {
        self.opaque
    }

    /// The set containing exactly one token, non-nullable.
    #[must_use]
    pub fn of_token(token: TokenId) -> Self {
        Self {
            tokens: BTreeSet::from([token]),
            nullable: false,
            opaque: false,
        }
    }

    /// The ε set: nullable, no tokens.
    #[must_use]
    pub const fn epsilon() -> Self {
        Self {
            tokens: BTreeSet::new(),
            nullable: true,
            opaque: false,
        }
    }

    /// Union `other` into `self`; returns whether anything grew.
    pub fn union(&mut self, other: &Self) -> bool {
        let mut grew = false;
        for token in &other.tokens {
            grew |= self.tokens.insert(*token);
        }
        if other.nullable && !self.nullable {
            self.nullable = true;
            grew = true;
        }
        if other.opaque && !self.opaque {
            self.opaque = true;
            grew = true;
        }
        grew
    }
}

/// What can follow a position: tokens plus the end-of-input marker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LookSet {
    /// Tokens that can appear next.
    pub tokens: BTreeSet<TokenId>,
    /// End of input can appear next.
    pub eof: bool,
}

impl LookSet {
    /// The set containing only EOF.
    #[must_use]
    pub fn eof_only() -> Self {
        Self {
            tokens: BTreeSet::new(),
            eof: true,
        }
    }

    /// Insert one token; returns whether the set grew.
    pub fn insert(&mut self, token: TokenId) -> bool {
        self.tokens.insert(token)
    }

    /// Union `other` into `self`; returns whether anything grew.
    pub fn union(&mut self, other: &Self) -> bool {
        let mut grew = false;
        for token in &other.tokens {
            grew |= self.tokens.insert(*token);
        }
        if other.eof && !self.eof {
            self.eof = true;
            grew = true;
        }
        grew
    }
}

/// Per-rule FIRST + NULLABLE fixpoint result.
#[derive(Debug, Clone, Default)]
pub struct FirstSets {
    /// FIRST/nullable for every rule id (indexed by `RuleId`).
    pub sets: Vec<FirstSet>,
}

impl FirstSets {
    /// Compute the fixpoint (monotone growth over a finite lattice —
    /// always terminates; the loop count is bounded by rules × tokens).
    #[must_use]
    pub fn compute(grammar: &Grammar) -> Self {
        let mut sets: Vec<FirstSet> = vec![FirstSet::default(); grammar.rules.len()];
        loop {
            let mut changed = false;
            for rule in &grammar.rules {
                let idx = rule.id.0 as usize;
                let mut fresh = FirstSet::default();
                first_of(&rule.body, &sets, &mut fresh);
                // Monotone growth: fold the old approximation in (the
                // lattice only grows, so the fixpoint terminates).
                fresh.union(&sets[idx]);
                if sets[idx] != fresh {
                    sets[idx] = fresh;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self { sets }
    }

    /// FIRST of one rule.
    #[must_use]
    pub fn first(&self, id: RuleId) -> &FirstSet {
        &self.sets[id.0 as usize]
    }
}

/// FIRST + nullable of one element, reading the current per-rule
/// approximations (no recursion through rule references — the fixpoint
/// loop handles cross-rule growth). Public: the LL(1) checker reuses it
/// for per-branch FIRST sets.
pub fn first_of(element: &Element, sets: &[FirstSet], out: &mut FirstSet) {
    match element {
        Element::Token(id) => {
            out.union(&FirstSet::of_token(*id));
        }
        Element::Rule(id) => {
            let Some(rule_set) = sets.get(id.0 as usize) else {
                return;
            };
            out.union(rule_set);
        }
        Element::Seq(elements) => {
            for element in elements {
                let mut fresh = FirstSet::default();
                first_of(element, sets, &mut fresh);
                out.union(&fresh);
                if !fresh.nullable {
                    break;
                }
            }
        }
        Element::Alt(branches) => {
            for branch in branches {
                first_of(branch, sets, out);
            }
        }
        // Opt/Rep are nullable; And matches without consuming (inner
        // FIRST contributes, nothing consumed). All three: inner FIRST
        // + nullable.
        Element::Opt(inner) | Element::Rep { inner, .. } | Element::And(inner) => {
            first_of(inner, sets, out);
            out.nullable = true;
        }
        // A negative lookahead cannot be represented as a token set.
        Element::Not(_) => {
            out.opaque = true;
            out.nullable = true;
        }
    }
}

/// Per-rule FOLLOW fixpoint result (FOLLOW of the start rule includes
/// EOF).
#[derive(Debug, Clone, Default)]
pub struct FollowSets {
    /// FOLLOW for every rule id (indexed by `RuleId`).
    pub sets: Vec<LookSet>,
}

impl FollowSets {
    /// Compute the fixpoint.
    #[must_use]
    pub fn compute(grammar: &Grammar, first: &FirstSets) -> Self {
        let mut sets: Vec<LookSet> = vec![LookSet::default(); grammar.rules.len()];
        sets[grammar.start.0 as usize].eof = true;
        loop {
            let mut changed = false;
            for rule in &grammar.rules {
                let inherited = sets[rule.id.0 as usize].clone();
                if follow_walk(&rule.body, &inherited, first, &mut sets, &mut changed) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self { sets }
    }

    /// FOLLOW of one rule.
    #[must_use]
    pub fn follow(&self, id: RuleId) -> &LookSet {
        &self.sets[id.0 as usize]
    }
}

/// Propagate `look` into every rule reference inside `element`; returns
/// whether any FOLLOW set grew.
fn follow_walk(
    element: &Element,
    look: &LookSet,
    first: &FirstSets,
    sets: &mut [LookSet],
    changed: &mut bool,
) -> bool {
    match element {
        Element::Token(_) | Element::Not(_) | Element::And(_) => false,
        Element::Rule(id) => {
            let Some(slot) = sets.get_mut(id.0 as usize) else {
                return false;
            };
            let grew = slot.union(look);
            *changed |= grew;
            grew
        }
        Element::Seq(elements) => {
            // Walk right-to-left: `look` accumulates the FIRST of the
            // suffix; nullable suffixes keep growing `look`.
            let mut grew = false;
            let mut suffix = look.clone();
            for element in elements.iter().rev() {
                grew |= follow_walk(element, &suffix, first, sets, changed);
                let mut fresh = FirstSet::default();
                first_of(element, &first.sets, &mut fresh);
                if fresh.nullable || fresh.opaque {
                    suffix.union(&first_to_look(&fresh));
                } else {
                    suffix = first_to_look(&fresh);
                }
            }
            grew
        }
        Element::Alt(branches) => {
            let mut grew = false;
            for branch in branches {
                grew |= follow_walk(branch, look, first, sets, changed);
            }
            grew
        }
        Element::Opt(inner) => follow_walk(inner, look, first, sets, changed),
        Element::Rep { inner, .. } => {
            // After one iteration the inner can start again.
            let mut fresh = FirstSet::default();
            first_of(inner, &first.sets, &mut fresh);
            let mut inner_look = look.clone();
            inner_look.union(&first_to_look(&fresh));
            follow_walk(inner, &inner_look, first, sets, changed)
        }
    }
}

fn first_to_look(set: &FirstSet) -> LookSet {
    LookSet {
        tokens: set.tokens.clone(),
        eof: false,
    }
}

//! Ambiguity marks (roadmap §6.2, ADR-007): the automaton's recorded
//! conflicts attributed back to grammar rules through production origins.
//!
//! The LR(1) shadow automaton records every shift/reduce and
//! reduce/reduce contention it resolves (`Automaton::conflicts`). This
//! module maps each contention back to the GIR rule it belongs to — the
//! plan's analyzer-side ambiguity marking, the seam GLR islands build on
//! (§7.4) — and carries the two contending productions (the
//! counter-example derivations the determinism report renders).
//!
//! Attribution policy: a conflict's rule is the smallest [`RuleId`] among
//! the contending productions' origins (deterministic; the first
//! declaring rule of the contending pair). Contentions whose productions
//! have no origin (synthetic helpers, the augmented start) are skipped —
//! they belong to the lowering, not to an author region.

use std::collections::{BTreeMap, BTreeSet};

use gir_ir::RuleId;

use crate::automaton::{Action, Automaton, ConflictKind, Item, StateId, closure};
use crate::lower::{First, Look, NontermId, Production, Symbol};

/// One contending production: its origin, left-hand side and right-hand
/// side (the derivation the report renders).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProductionSide {
    /// The GIR rule this production lowers (`None` for synthetics).
    pub origin: Option<RuleId>,
    /// The left-hand side nonterminal.
    pub lhs: NontermId,
    /// The right-hand side symbols.
    pub symbols: Vec<Symbol>,
}

impl ProductionSide {
    fn of(production: &Production) -> Self {
        Self {
            origin: production.origin,
            lhs: production.lhs,
            symbols: production.symbols.clone(),
        }
    }
}

/// A conflict attributed to a grammar rule: the contended terminal, the
/// resolution kind, the contending productions and the states where the
/// contention arose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguityMark {
    /// The attributed rule (the first declaring origin of the contending
    /// pair).
    pub rule: RuleId,
    /// The contended terminal (the pseudo-EOF sentinel rides outside the
    /// token-id space).
    pub term: Look,
    /// How the automaton resolved the contention.
    pub kind: ConflictKind,
    /// The contending productions in deterministic order (shift side
    /// before reduce side for shift/reduce; kept before dropped for
    /// reduce/reduce).
    pub sides: Vec<ProductionSide>,
    /// The states where the contention was recorded (ascending).
    pub states: Vec<StateId>,
}

/// The full item set of a recorded state (kernel plus closure):
/// reductions may come from closure items (ε alternatives), which the
/// stored kernel does not retain.
fn full_items(
    automaton: &Automaton,
    state: StateId,
    prods_of: &[Vec<u32>],
    first: &First,
) -> BTreeSet<Item> {
    let Some(state_ref) = automaton.states.get(state.0 as usize) else {
        return BTreeSet::new();
    };
    closure(
        &state_ref.kernel,
        &automaton.lowered.productions,
        prods_of,
        first,
    )
}

/// Per-conflict attribution: the reduce production the rule keys on and
/// the contending sides in deterministic order.
fn sides_for(
    automaton: &Automaton,
    term: Look,
    kept: Action,
    items: &BTreeSet<Item>,
) -> Option<(u32, Vec<ProductionSide>)> {
    // Completed items reducing on `term`.
    let mut reduces: BTreeSet<u32> = BTreeSet::new();
    for item in items {
        let Some(production) = automaton.lowered.productions.get(item.prod as usize) else {
            continue;
        };
        if (item.dot as usize) >= production.symbols.len() && item.la == term {
            reduces.insert(item.prod);
        }
    }
    match kept {
        // Shift/reduce: the shift item's production first, then the
        // reduce the shift displaced.
        Action::Shift(_) => {
            let token = term.as_token()?;
            let mut shifts: BTreeSet<u32> = BTreeSet::new();
            for item in items {
                let Some(production) = automaton.lowered.productions.get(item.prod as usize) else {
                    continue;
                };
                if production.symbols.get(item.dot as usize) == Some(&Symbol::Token(token)) {
                    shifts.insert(item.prod);
                }
            }
            let shift = shifts.iter().next().copied()?;
            let reduce = reduces.iter().next().copied()?;
            let shift_side = side_of(automaton, shift)?;
            let reduce_side = side_of(automaton, reduce)?;
            Some((reduce, vec![shift_side, reduce_side]))
        }
        // Reduce/reduce: the kept (earlier) production first, then the
        // displaced one.
        Action::Reduce(kept_prod) => {
            let kept_side = side_of(automaton, kept_prod)?;
            let dropped = reduces.iter().find(|prod| **prod != kept_prod).copied()?;
            let dropped_side = side_of(automaton, dropped)?;
            Some((kept_prod, vec![kept_side, dropped_side]))
        }
        // The accept action only contends in degenerate augmented-start
        // shapes; there is no author-facing region to attribute.
        Action::Accept => None,
    }
}

/// The [`ProductionSide`] for a production index (`None` past the lowered
/// table — the augmented start has no derivation to render).
fn side_of(automaton: &Automaton, prod: u32) -> Option<ProductionSide> {
    automaton
        .lowered
        .productions
        .get(prod as usize)
        .map(ProductionSide::of)
}

/// The `production → lhs` index (rules first, synthetics after) the
/// closure recomputation needs.
fn prods_of(automaton: &Automaton) -> Vec<Vec<u32>> {
    let mut prods: Vec<Vec<u32>> = vec![Vec::new(); automaton.lowered.nonterms as usize];
    for (idx, production) in automaton.lowered.productions.iter().enumerate() {
        let idx = u32::try_from(idx).expect("production count fits u32");
        if let Some(slot) = prods.get_mut(production.lhs.0 as usize) {
            slot.push(idx);
        }
    }
    prods
}

/// Attribute every recorded conflict to grammar rules.
///
/// Conflicts group by `(rule, term, kind)`; sides dedup in first-seen
/// order and states accumulate ascending. The output is ordered by that
/// same key, so the marks are a deterministic function of the automaton.
#[must_use]
pub fn ambiguity_marks(automaton: &Automaton) -> Vec<AmbiguityMark> {
    let prods = prods_of(automaton);
    let first = First::compute(&automaton.lowered);
    let mut groups: BTreeMap<(RuleId, Look, ConflictKind), Group> = BTreeMap::new();
    for conflict in &automaton.conflicts {
        let Some(kept) = automaton.action(conflict.state, conflict.term).copied() else {
            continue;
        };
        let items = full_items(automaton, conflict.state, &prods, &first);
        let Some((reduce_prod, sides)) = sides_for(automaton, conflict.term, kept, &items) else {
            continue;
        };
        let Some(production) = automaton.lowered.productions.get(reduce_prod as usize) else {
            continue;
        };
        let Some(origin) = production.origin else {
            continue;
        };
        let group = groups
            .entry((origin, conflict.term, conflict.kind))
            .or_default();
        group.extend(sides, conflict.state);
    }
    groups
        .into_iter()
        .map(|((rule, term, kind), group)| AmbiguityMark {
            rule,
            term,
            kind,
            sides: group.sides,
            states: group.states,
        })
        .collect()
}

/// Accumulation buffer for one `(rule, term, kind)` group.
#[derive(Default)]
struct Group {
    sides: Vec<ProductionSide>,
    states: Vec<StateId>,
}

impl Group {
    fn extend(&mut self, sides: Vec<ProductionSide>, state: StateId) {
        for side in sides {
            if !self.sides.contains(&side) {
                self.sides.push(side);
            }
        }
        if !self.states.contains(&state) {
            self.states.push(state);
            self.states.sort_unstable();
        }
    }
}

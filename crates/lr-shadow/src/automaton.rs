//! The canonical LR(1) shadow automaton (issue 011): kernel worklist
//! construction over the lowered productions, goto/action tables, the
//! conflict policy (shift-wins, silent synthetic ε/ε resolution), per-state
//! expected sets and reachability.
//!
//! States are identified by their **kernel** item sets (`BTreeSet`-ordered,
//! so state numbering is a deterministic function of the grammar). The
//! pseudo-EOF sentinel [`Look::EOF`] rides outside the token id space, so
//! the automaton is stable under token-table growth.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gir_ir::Grammar;

use crate::lower::{
    First, Look, Lowered, NontermId, ProdKind, Production, Symbol, first_of_suffix,
};

// ---------------------------------------------------------------------------
// Items and states
// ---------------------------------------------------------------------------

/// An LR(1) item: production, dot position, lookahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Item {
    /// Production index in [`Lowered::productions`].
    pub prod: u32,
    /// Dot position (index into the production's symbols).
    pub dot: u32,
    /// The item's lookahead terminal.
    pub la: Look,
}

/// A state id (dense, worklist discovery order — deterministic because
/// kernels are ordered sets and symbols are walked in `BTreeMap` order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateId(pub u32);

/// One table action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Shift and move to the target state.
    Shift(StateId),
    /// Reduce the production (index in [`Lowered::productions`]).
    Reduce(u32),
    /// Accept: the augmented start's EOF move (the stream terminator).
    Accept,
}

/// A recorded table conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    /// The state where the conflict arose.
    pub state: StateId,
    /// The contested terminal.
    pub term: Look,
    /// The resolution taken.
    pub kind: ConflictKind,
}

/// How a conflict was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Shift/reduce: shift wins (recorded — the shadow automaton flags
    /// ambiguity even where descent codegen can live with it).
    ShiftReduce,
    /// Reduce/reduce: the earlier production (declaration order) wins.
    ReduceReduce,
}

/// One automaton state.
#[derive(Debug, Clone)]
pub struct State {
    /// The kernel items (closure items are not stored — they are
    /// recomputable and unused after table construction).
    pub kernel: BTreeSet<Item>,
    /// Terminal actions (`Look::EOF` included when present).
    pub actions: BTreeMap<Look, Action>,
    /// Nonterminal transitions.
    pub gotos: BTreeMap<NontermId, StateId>,
}

// ---------------------------------------------------------------------------
// Automaton
// ---------------------------------------------------------------------------

/// The canonical LR(1) shadow automaton over a [`Grammar`].
#[derive(Debug, Clone)]
pub struct Automaton {
    /// States, in discovery order (state 0 is the start state).
    pub states: Vec<State>,
    /// All conflicts recorded under the resolution policy.
    pub conflicts: Vec<Conflict>,
    /// The start state id (always 0).
    pub start: StateId,
    /// The lowering the automaton was built from.
    pub lowered: Lowered,
}

impl Automaton {
    /// Build the automaton: lower the grammar, run the kernel worklist,
    /// resolve conflicts, derive expected sets.
    ///
    /// # Panics
    /// When the grammar exceeds `u32::MAX` rules or productions
    /// (index-space exhaustion).
    #[must_use]
    pub fn build(grammar: &Grammar) -> Self {
        let lowered = crate::lower::lower(grammar);
        let first = First::compute(&lowered);

        // Augmented start: the last production is `aug → start EOF`.
        let aug_lhs = NontermId(lowered.nonterms);
        let mut productions = lowered.productions.clone();
        productions.push(Production {
            lhs: aug_lhs,
            symbols: vec![Symbol::Nonterm(lowered.start), Symbol::Eof],
            kind: ProdKind::Normal,
            origin: None,
        });
        let aug_prod = u32::try_from(productions.len() - 1).expect("production count fits u32");

        let mut prods_of: Vec<Vec<u32>> = vec![Vec::new(); lowered.nonterms as usize + 1];
        for (idx, production) in productions.iter().enumerate() {
            let prod = u32::try_from(idx).expect("production count fits u32");
            prods_of[production.lhs.0 as usize].push(prod);
        }

        let mut states: Vec<State> = Vec::new();
        let mut index: BTreeMap<BTreeSet<Item>, StateId> = BTreeMap::new();
        let mut conflicts: Vec<Conflict> = Vec::new();

        let start_kernel = BTreeSet::from([Item {
            prod: aug_prod,
            dot: 0,
            la: Look::EOF,
        }]);
        let start_state = StateId(0);
        states.push(State {
            kernel: start_kernel.clone(),
            actions: BTreeMap::new(),
            gotos: BTreeMap::new(),
        });
        index.insert(start_kernel, start_state);

        let mut queue: VecDeque<StateId> = VecDeque::from([start_state]);
        while let Some(state_id) = queue.pop_front() {
            let kernel = states[state_id.0 as usize].kernel.clone();
            let full = closure(&kernel, &productions, &prods_of, &first);

            // Group dot-advances by the symbol they move on.
            let mut moves: BTreeMap<Symbol, BTreeSet<Item>> = BTreeMap::new();
            for item in &full {
                let production = &productions[item.prod as usize];
                if let Some(&symbol) = production.symbols.get(item.dot as usize) {
                    let advanced = Item {
                        prod: item.prod,
                        dot: item.dot + 1,
                        la: item.la,
                    };
                    moves.entry(symbol).or_default().insert(advanced);
                }
            }
            for (symbol, advanced) in moves {
                match symbol {
                    // The augmented start's EOF move *is* the accept
                    // action: no extra state, EOF stays the stream
                    // terminator.
                    Symbol::Eof => set_action(
                        states.as_mut_slice(),
                        &productions,
                        state_id,
                        Look::EOF,
                        Action::Accept,
                        &mut conflicts,
                    ),
                    Symbol::Token(id) => {
                        let target = intern(&mut states, &mut index, &mut queue, advanced);
                        set_action(
                            states.as_mut_slice(),
                            &productions,
                            state_id,
                            Look::token(id),
                            Action::Shift(target),
                            &mut conflicts,
                        );
                    }
                    Symbol::Nonterm(nt) => {
                        let target = intern(&mut states, &mut index, &mut queue, advanced);
                        states[state_id.0 as usize].gotos.insert(nt, target);
                    }
                }
            }

            // Completed items reduce on their lookahead; the augmented
            // production accepts.
            for item in &full {
                let production = &productions[item.prod as usize];
                if (item.dot as usize) < production.symbols.len() {
                    continue;
                }
                let action = if item.prod == aug_prod {
                    Action::Accept
                } else {
                    Action::Reduce(item.prod)
                };
                set_action(
                    states.as_mut_slice(),
                    &productions,
                    state_id,
                    item.la,
                    action,
                    &mut conflicts,
                );
            }
        }

        Self {
            states,
            conflicts,
            start: start_state,
            lowered,
        }
    }

    /// The action for a terminal in a state.
    #[must_use]
    pub fn action(&self, state: StateId, term: Look) -> Option<&Action> {
        self.states[state.0 as usize].actions.get(&term)
    }

    /// The goto target for a nonterminal in a state.
    #[must_use]
    pub fn goto(&self, state: StateId, nt: NontermId) -> Option<StateId> {
        self.states[state.0 as usize].gotos.get(&nt).copied()
    }

    /// The state's expected terminal set (sorted: tokens by id, EOF last).
    #[must_use]
    pub fn expected(&self, state: StateId) -> Vec<Look> {
        self.states[state.0 as usize]
            .actions
            .keys()
            .copied()
            .collect()
    }

    /// Reachability from the start state over shifts and gotos.
    #[must_use]
    pub fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.states.len()];
        let mut queue: VecDeque<StateId> = VecDeque::from([self.start]);
        seen[self.start.0 as usize] = true;
        while let Some(state) = queue.pop_front() {
            let current = &self.states[state.0 as usize];
            let targets = current
                .actions
                .values()
                .filter_map(|action| match action {
                    Action::Shift(target) => Some(*target),
                    Action::Reduce(_) | Action::Accept => None,
                })
                .chain(current.gotos.values().copied());
            for target in targets {
                let idx = target.0 as usize;
                if !seen[idx] {
                    seen[idx] = true;
                    queue.push_back(target);
                }
            }
        }
        seen
    }
}

// ---------------------------------------------------------------------------
// Construction helpers
// ---------------------------------------------------------------------------

/// LR(1) closure over a kernel.
fn closure(
    kernel: &BTreeSet<Item>,
    productions: &[Production],
    prods_of: &[Vec<u32>],
    first: &First,
) -> BTreeSet<Item> {
    let mut set = kernel.clone();
    let mut work: Vec<Item> = kernel.iter().copied().collect();
    while let Some(item) = work.pop() {
        let production = &productions[item.prod as usize];
        let Some(&symbol) = production.symbols.get(item.dot as usize) else {
            continue;
        };
        let Symbol::Nonterm(nt) = symbol else {
            continue;
        };
        let las = first_of_suffix(&production.symbols[item.dot as usize + 1..], item.la, first);
        for la in las {
            for &prod in &prods_of[nt.0 as usize] {
                let fresh = Item { prod, dot: 0, la };
                if set.insert(fresh) {
                    work.push(fresh);
                }
            }
        }
    }
    set
}

/// Intern a kernel as a state (or find the existing one) and queue it.
fn intern(
    states: &mut Vec<State>,
    index: &mut BTreeMap<BTreeSet<Item>, StateId>,
    queue: &mut VecDeque<StateId>,
    kernel: BTreeSet<Item>,
) -> StateId {
    if let Some(&existing) = index.get(&kernel) {
        return existing;
    }
    let id = StateId(u32::try_from(states.len()).expect("state count fits u32"));
    states.push(State {
        kernel,
        actions: BTreeMap::new(),
        gotos: BTreeMap::new(),
    });
    index.insert(states[id.0 as usize].kernel.clone(), id);
    queue.push_back(id);
    id
}

/// Whether a production is a synthetic ε alternative (empty alternative
/// or predicate placeholder): ε/ε contentions between these resolve
/// silently per the issue 011 policy.
fn is_synthetic_epsilon(productions: &[Production], prod: u32) -> bool {
    matches!(
        productions[prod as usize].kind,
        ProdKind::Epsilon | ProdKind::Predicate
    )
}

/// Insert an action under the conflict policy: shift wins over reduce
/// (recorded); reduce/reduce keeps the earlier production — silently when
/// both are synthetic ε/ε alternatives, recorded otherwise.
fn set_action(
    states: &mut [State],
    productions: &[Production],
    state: StateId,
    term: Look,
    action: Action,
    conflicts: &mut Vec<Conflict>,
) {
    let slot = &mut states[state.0 as usize].actions;
    let Some(existing) = slot.get(&term).copied() else {
        slot.insert(term, action);
        return;
    };
    let (kept, dropped, kind) = match (existing, action) {
        // Shift wins regardless of insertion order.
        (Action::Shift(_), Action::Reduce(_)) => (existing, action, ConflictKind::ShiftReduce),
        (Action::Reduce(_), Action::Shift(_)) => (action, existing, ConflictKind::ShiftReduce),
        (Action::Reduce(kept_prod), Action::Reduce(dropped_prod)) => {
            let (kept_prod, dropped_prod) = if kept_prod <= dropped_prod {
                (kept_prod, dropped_prod)
            } else {
                (dropped_prod, kept_prod)
            };
            (
                Action::Reduce(kept_prod),
                Action::Reduce(dropped_prod),
                ConflictKind::ReduceReduce,
            )
        }
        // Accept only ever contends with itself (same augmented
        // production) — keep the first insertion.
        (Action::Accept, _) | (_, Action::Accept) => (existing, action, ConflictKind::ReduceReduce),
        // Identical shift target twice: nothing to do.
        (Action::Shift(_), Action::Shift(_)) => return,
    };
    let silent = matches!(
        (kept, dropped),
        (Action::Reduce(kept_prod), Action::Reduce(dropped_prod))
            if is_synthetic_epsilon(productions, kept_prod)
                && is_synthetic_epsilon(productions, dropped_prod)
    );
    slot.insert(term, kept);
    if !silent {
        conflicts.push(Conflict { state, term, kind });
    }
}

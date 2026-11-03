//! The repair planner (issue 016): uniform-cost search over cactus
//! parse-stack prefixes with a work budget, canonical `(cost, ops)`
//! ordering and hint-driven sync discounts.

use std::collections::{BTreeMap, BTreeSet};

use lr_shadow::{Action, Automaton, Look, StateId};

/// One repair operation.
///
/// Ordering is canonical: `Insert` < `Delete`, then by token — equal-cost
/// plans resolve lexicographically (the differential tests pin this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepairOp {
    /// Insert the expected token before the error point.
    Insert(Look),
    /// Delete the offending input token.
    Delete(Look),
}

/// A planned repair: operations in canonical order plus the total cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlan {
    /// The repair operations, canonical order.
    pub ops: Vec<RepairOp>,
    /// Total cost under the planner's [`CostModel`].
    pub cost: u32,
}

/// Uniform-cost search weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    /// Cost of inserting one expected token.
    pub insert: u32,
    /// Cost of deleting one offending token.
    pub delete: u32,
    /// Cost of a normal shift (0 — correct parses are free).
    pub shift: u32,
    /// Cost of a reduction (0 — reductions are parse mechanics).
    pub reduce: u32,
    /// Cost of deleting a sync-hinted token (the "skip to boundary"
    /// discount; lower than [`CostModel::delete`]).
    pub sync_delete: u32,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            insert: 2,
            delete: 2,
            shift: 0,
            reduce: 0,
            sync_delete: 1,
        }
    }
}

/// Hint-driven sync table: tokens that terminate an error region
/// (closers, statement separators — derived from the grammar's
/// sync sets, hinted by the pack).
#[derive(Debug, Clone, Default)]
pub struct SyncHints(BTreeSet<Look>);

impl SyncHints {
    /// Build the hint set from an iterator of sync tokens.
    #[must_use]
    pub fn from_tokens(tokens: impl IntoIterator<Item = Look>) -> Self {
        Self(tokens.into_iter().collect())
    }

    /// Whether `look` is a hinted sync token.
    #[must_use]
    pub fn contains(&self, look: Look) -> bool {
        self.0.contains(&look)
    }

    /// The hinted tokens (ascending).
    #[must_use]
    pub fn tokens(&self) -> Vec<Look> {
        self.0.iter().copied().collect()
    }
}

/// The deterministic repair planner over a shadow automaton.
pub struct Planner<'a> {
    auto: &'a Automaton,
    model: CostModel,
    sync: SyncHints,
}

/// One search node: a cactus-stack prefix plus an input offset (the
/// cost and ops live in the frontier key).
struct Node {
    stack: Vec<StateId>,
    offset: usize,
}

impl<'a> Planner<'a> {
    /// A planner over `auto` with the default cost model and no hints.
    #[must_use]
    pub fn new(auto: &'a Automaton) -> Self {
        Self {
            auto,
            model: CostModel::default(),
            sync: SyncHints::default(),
        }
    }

    /// Override the cost model.
    #[must_use]
    pub fn with_cost_model(mut self, model: CostModel) -> Self {
        self.model = model;
        self
    }

    /// Install the sync hints.
    #[must_use]
    pub fn with_sync_hints(mut self, sync: SyncHints) -> Self {
        self.sync = sync;
        self
    }

    /// The active cost model.
    #[must_use]
    pub fn cost_model(&self) -> &CostModel {
        &self.model
    }

    /// Plan the minimal-cost repair that lets the automaton consume the
    /// whole `lookahead` window and accept at EOF.
    ///
    /// `stack` is the parse stack at the error point (state 0 at the
    /// bottom); `lookahead` is the remaining token window (without the
    /// pseudo-EOF — the goal checks the EOF action itself); `budget`
    /// caps search-node expansions (work, not time).
    ///
    /// Returns the canonical minimal plan, or `None` when the budget is
    /// exhausted before a complete repair is found (or the window is
    /// unrepairable). Deterministic: identical inputs produce identical
    /// plans.
    #[must_use]
    pub fn plan(&self, stack: &[StateId], lookahead: &[Look], budget: u32) -> Option<RepairPlan> {
        // Frontier keyed by (cost, ops): canonical pop order. `Node` holds
        // the cactus-stack prefix for the key.
        let mut frontier: BTreeMap<(u32, Vec<RepairOp>), Node> = BTreeMap::new();
        let start = Node {
            stack: stack.to_vec(),
            offset: 0,
        };
        frontier.insert((0, Vec::new()), start);
        let mut visited: BTreeSet<(Vec<StateId>, usize)> = BTreeSet::new();
        let mut expansions = 0_u32;

        while let Some(((cost, ops), node)) = frontier.pop_first() {
            if expansions >= budget {
                return None; // work budget exhausted — no time-based fallback
            }
            expansions += 1;

            // Goal: the whole window consumed AND the stack reduces to
            // accept at the pseudo-EOF.
            if node.offset >= lookahead.len() && self.accepts_at_eof(&node.stack) {
                return Some(RepairPlan { ops, cost });
            }
            // Dedupe states: first (cheapest, canonical) expansion wins.
            if !visited.insert((node.stack.clone(), node.offset)) {
                continue;
            }

            let Some(&top) = node.stack.last() else {
                continue; // unreachable: stacks never empty
            };

            // 1. Normal parse move (shift or reduce chain) over the
            //    current input token — zero repair cost beyond the model.
            let normal = lookahead
                .get(node.offset)
                .copied()
                .and_then(|la| self.advance(&node.stack, la));
            if let Some(next_stack) = normal {
                Self::push_node(
                    &mut frontier,
                    next_stack,
                    node.offset + 1,
                    cost + self.model.shift,
                    &ops,
                    None,
                );
            }

            // 2. Insert an expected token (simulate shifting it; the
            //    pending reduce chain comes along inside `advance`).
            for expected in self.auto.expected(top) {
                if expected.is_eof() {
                    continue; // the pseudo-EOF is never insertable
                }
                let Some(next_stack) = self.advance(&node.stack, expected) else {
                    continue;
                };
                Self::push_node(
                    &mut frontier,
                    next_stack,
                    node.offset,
                    cost + self.model.insert,
                    &ops,
                    Some(RepairOp::Insert(expected)),
                );
            }

            // 3. Delete the offending token (discounted on sync hints).
            if let Some(&la) = lookahead.get(node.offset) {
                let delete_cost = if self.sync.contains(la) {
                    self.model.sync_delete
                } else {
                    self.model.delete
                };
                Self::push_node(
                    &mut frontier,
                    node.stack.clone(),
                    node.offset + 1,
                    cost + delete_cost,
                    &ops,
                    Some(RepairOp::Delete(la)),
                );
            }
        }
        None
    }

    fn push_node(
        frontier: &mut BTreeMap<(u32, Vec<RepairOp>), Node>,
        stack: Vec<StateId>,
        offset: usize,
        cost: u32,
        ops: &[RepairOp],
        op: Option<RepairOp>,
    ) {
        let mut key_ops = ops.to_vec();
        if let Some(op) = op {
            key_ops.push(op);
        }
        frontier
            .entry((cost, key_ops))
            .or_insert(Node { stack, offset });
    }

    /// Simulate the parse over one input token: apply the deterministic
    /// reduce chain, then shift. `None` when the automaton cannot consume
    /// `la` from this stack (or the reduce chain hits its step cap).
    fn advance(&self, stack: &[StateId], la: Look) -> Option<Vec<StateId>> {
        let mut stack = stack.to_vec();
        let mut steps = 0_u32;
        loop {
            let top = *stack.last()?;
            match self.auto.action(top, la).copied()? {
                Action::Shift(target) => {
                    stack.push(target);
                    return Some(stack);
                }
                Action::Reduce(prod) => {
                    let production = &self.auto.lowered.productions[prod as usize];
                    let arity = production.symbols.len();
                    if stack.len() < arity + 1 {
                        return None; // cannot pop below the base state
                    }
                    stack.truncate(stack.len() - arity);
                    let base = *stack.last()?;
                    let target = self.auto.goto(base, production.lhs)?;
                    stack.push(target);
                }
                Action::Accept => return None, // accept is the goal check's job
            }
            steps += 1;
            if steps > u32::try_from(stack.len()).unwrap_or(u32::MAX / 2) * 2 + 64 {
                return None; // ε-reduce cycle guard (bounded, deterministic)
            }
        }
    }

    /// Whether the stack reduces to the accept action at the pseudo-EOF
    /// (bounded ε-reduce chain).
    fn accepts_at_eof(&self, stack: &[StateId]) -> bool {
        self.accepts_at_eof_opt(stack).unwrap_or(false)
    }

    /// The `Option`-returning core of [`Self::accepts_at_eof`].
    fn accepts_at_eof_opt(&self, stack: &[StateId]) -> Option<bool> {
        let mut stack = stack.to_vec();
        let mut steps = 0_u32;
        loop {
            let top = *stack.last()?;
            match self.auto.action(top, Look::EOF).copied()? {
                Action::Accept => return Some(true),
                Action::Reduce(prod) => {
                    let production = &self.auto.lowered.productions[prod as usize];
                    let arity = production.symbols.len();
                    if stack.len() < arity + 1 {
                        return Some(false);
                    }
                    stack.truncate(stack.len() - arity);
                    let base = *stack.last()?;
                    let target = self.auto.goto(base, production.lhs)?;
                    stack.push(target);
                }
                Action::Shift(_) => return Some(false),
            }
            steps += 1;
            if steps > u32::try_from(stack.len()).unwrap_or(u32::MAX / 2) * 2 + 64 {
                return Some(false);
            }
        }
    }
}

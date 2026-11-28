//! The GLR island executor (roadmap §7.4, ADR-007): the runtime for the
//! regions the analyzer marks ambiguous — the third runtime backend over
//! the shadow automaton, beside the deterministic descent engine and the
//! table-driven [`crate::driver`].
//!
//! Plan lines pinned (§7.4): *"GLR islands for regions the analyzer marks
//! ambiguous: local graph-structured stack … with shared tails on fork …
//! we implement ~600 `LoC` of GSS with fork/merge caps borrowed from
//! tree-sitter's version bounds — MAX_VERSION_COUNT-style limits,
//! deterministic merge on equal (state, position, cost). Islands are
//! selected at compile time only (ADR-007); a runtime 'full GLR' never
//! exists."* And §12.2's status note: *"the island executor — the arena
//! GSS with fork/merge caps and the descent-engine integration — does
//! not"* (this module and `descent::island` close it).
//!
//! ## Shape
//!
//! - **The arena GSS**: stack versions are paths in one graph —
//!   nodes are state frames, parents are shared arena indices, so a fork
//!   adds children to the SAME predecessor (shared tails) and a merge
//!   lets one node carry both derivations' parent links. No per-version
//!   stack copies.
//! - **Forks at recorded conflicts**: the automaton's table is resolved
//!   (shift-wins, issue 011 policy), so the executor derives the viable
//!   action set at every recorded [`crate::automaton::Conflict`] — the
//!   table's kept action plus the displaced reductions recomputed from
//!   the state's full item set — and runs one stack version per action.
//!   Synthetic ε/ε contentions resolve silently in the table and never
//!   fork (the lowering's business, the [`crate::ambiguity`] policy).
//! - **Deterministic merge**: versions meeting at equal
//!   `(state, position)` join one node; the cheaper derivation survives,
//!   equal costs keep the first-discovered one — a deterministic function
//!   of `(state, position, cost)`. A merge that adds a genuinely new
//!   parent edge to a settled node re-queues it: the node's reductions
//!   fan over every parent path, so the enriched graph keeps parsing
//!   (this is what unwinds right-recursive lists through the self-goto
//!   states a canonical LR(1) build interns once). The parent-link dedup
//!   bounds the re-queues by the edge count; the step budget bounds the
//!   work.
//! - **Fork/merge caps**: live versions per level are capped at
//!   [`MAX_VERSIONS`] (tree-sitter's `MAX_VERSION_COUNT` bound); pruning
//!   drops the costliest version first (ties: the latest spawned), so a
//!   tightened cap changes outcomes deterministically instead of
//!   exploding.
//! - **Work budget**: the §7.5 posture — a step budget (shifts, reduces,
//!   accepts) instead of a wall clock; exhaustion is a deterministic
//!   rejection.
//!
//! The output is the winning version's verdict plus its derivation
//! ([`IslandRun::ops`], a replayable shift/reduce sequence) and the
//! fork/merge statistics — the same verdict-shaped contract the
//! [`crate::driver`] hands the cross-backend oracle, and the seam
//! `descent::island` runs over the descent engine's own feed.
//!
//! The arena type is crate-private: its shape is observable through the
//! run statistics ([`IslandRun::versions_spawned`] grows by merges, not
//! by per-version stack copies), which is the contract the gates pin.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::automaton::{Action, Automaton, Item, StateId, closure};
use crate::lower::{First, Look, NontermId, ProdKind, Production, Symbol};

/// The live-version cap, borrowed from tree-sitter's `MAX_VERSION_COUNT`
/// version bound (roadmap §7.4: "version caps like tree-sitter").
pub const MAX_VERSIONS: usize = 6;

/// The default work budget (shifts + reduces + accepts) — the §7.5
/// work-budget posture: machine-independent, never a wall clock.
pub const DEFAULT_STEP_BUDGET: u64 = 8_192;

/// Executor bounds (roadmap §7.4 "fork/merge caps").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IslandCaps {
    /// Maximum live stack versions per feed level; a fork that would
    /// exceed it prunes deterministically (costliest first, ties by the
    /// latest spawn).
    pub max_versions: usize,
    /// Maximum executed actions (shifts, reduces, accepts) before the
    /// search stops with a [`RejectionReason::StepBudget`] rejection.
    pub step_budget: u64,
}

impl Default for IslandCaps {
    fn default() -> Self {
        Self {
            max_versions: MAX_VERSIONS,
            step_budget: DEFAULT_STEP_BUDGET,
        }
    }
}

/// One derivation step of the winning version — a replayable shift/reduce
/// sequence over the automaton's productions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IslandOp {
    /// Shift the current token into the stack.
    Shift,
    /// Reduce the production (index into the lowered productions).
    Reduce(u32),
}

/// Why the executor stopped without acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// Every live version ran out of viable actions.
    NoAction,
    /// The work budget ran out before acceptance or exhaustion.
    StepBudget,
}

/// Where and why the executor stopped without acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandRejection {
    /// The feed index of the terminal the surviving versions could not
    /// place (`tokens.len()` when only the pseudo-EOF was left).
    pub at: usize,
    /// Why the search stopped.
    pub reason: RejectionReason,
    /// The union of the dead versions' expected terminal sets at `at`
    /// (sorted: tokens by id, EOF last — [`Automaton::expected`]).
    pub expected: Vec<Look>,
}

/// The result of one island run: verdict, winning derivation, and the
/// fork/merge statistics (`PartialEq`/`Eq` so determinism is pinnable —
/// the same contract as [`crate::driver::ParseOutcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandRun {
    /// Some derivation of the whole feed reached the accept action.
    pub accepted: bool,
    /// Tokens consumed when the run stopped (`tokens.len()` on accept;
    /// the stop position on reject).
    pub consumed: usize,
    /// The winning version's derivation, in execution order: the shifts
    /// and reductions of one accepting path (the synthetic accept-reduce
    /// of the augmented start is not included — the verdict carries it).
    /// Along first-parent chains the sequence is the exact derivation;
    /// reductions fanning over a merged node's additional parents record
    /// the ancestor's own path (the v0 derivation is one path, not the
    /// whole shared-packed forest — "a runtime 'full GLR' never exists").
    pub ops: Vec<IslandOp>,
    /// Fork events: conflict points where more than one action ran.
    pub forks: u64,
    /// Merges: versions joining an existing `(state, position)` node.
    pub merges: u64,
    /// Arena nodes created (the shared-tail GSS grows by this, not by
    /// per-version stack copies).
    pub versions_spawned: u64,
    /// Versions dropped by the [`IslandCaps::max_versions`] cap.
    pub pruned: u64,
    /// The rejection (`None` on accept).
    pub rejection: Option<IslandRejection>,
}

/// One GSS node: a state frame whose parents are shared arena indices. A
/// fork's children reference the same predecessor; a merge lets one node
/// carry both derivations' parent links.
struct GssNode {
    state: StateId,
    /// Executed actions along the surviving derivation (the merge cost).
    cost: u64,
    /// Predecessor arena indices, deduplicated (shared tails).
    parents: Vec<u32>,
    /// The op that created this node (`None` for the start frame).
    last_op: Option<IslandOp>,
    /// The derivation from the start state to this node, along the
    /// surviving (cheapest, first-discovered) path.
    ops: Vec<IslandOp>,
}

/// The arena graph-structured stack.
struct Gss {
    nodes: Vec<GssNode>,
}

impl Gss {
    fn spawn(&mut self, node: GssNode) -> u32 {
        let id = u32::try_from(self.nodes.len()).expect("gss node count fits u32");
        self.nodes.push(node);
        id
    }

    /// Link `parent` under `id`; `true` when the edge is genuinely new
    /// (a deduplicated re-link reports `false` — nothing to re-explore).
    fn link(&mut self, id: u32, parent: u32) -> bool {
        let node = &mut self.nodes[id as usize];
        if node.parents.contains(&parent) {
            return false;
        }
        node.parents.push(parent);
        true
    }
}

/// The viable action sets at recorded conflict points. Computed once per
/// run: the table is resolved, so the displaced actions are recomputed
/// from the states' full item sets (kernel plus closure).
struct ForkSets {
    sets: BTreeMap<(StateId, Look), Vec<Action>>,
    productions: Vec<Production>,
}

impl ForkSets {
    /// Derive the viable set at every recorded conflict: the table's kept
    /// action plus every completed reduction on the contested terminal.
    /// The augmented production completes to `Accept`, not `Reduce`.
    fn of(automaton: &Automaton) -> Self {
        // The augmented production rides past the lowering's own table
        // (`Automaton::build` appends it last to a clone of these
        // productions — `automaton.lowered` stays un-augmented).
        let mut productions = automaton.lowered.productions.clone();
        productions.push(Production {
            lhs: NontermId(automaton.lowered.nonterms),
            symbols: vec![Symbol::Nonterm(automaton.lowered.start), Symbol::Eof],
            kind: ProdKind::Normal,
            origin: None,
        });
        let aug_prod = u32::try_from(productions.len() - 1).expect("production count fits u32");
        let mut prods_of: Vec<Vec<u32>> = vec![Vec::new(); productions.len()];
        for (idx, production) in productions.iter().enumerate() {
            let idx = u32::try_from(idx).expect("production count fits u32");
            if let Some(slot) = prods_of.get_mut(production.lhs.0 as usize) {
                slot.push(idx);
            }
        }
        let first = First::compute(&automaton.lowered);
        let mut sets: BTreeMap<(StateId, Look), Vec<Action>> = BTreeMap::new();
        let mut item_cache: BTreeMap<StateId, BTreeSet<Item>> = BTreeMap::new();
        for conflict in &automaton.conflicts {
            let items = item_cache
                .entry(conflict.state)
                .or_insert_with(|| {
                    let Some(state_ref) = automaton.states.get(conflict.state.0 as usize) else {
                        return BTreeSet::new();
                    };
                    closure(&state_ref.kernel, &productions, &prods_of, &first)
                })
                .clone();
            let mut viable: Vec<Action> = items
                .iter()
                .filter(|item| item.la == conflict.term)
                .filter_map(|item| {
                    let production = &productions[item.prod as usize];
                    if (item.dot as usize) < production.symbols.len() {
                        return None;
                    }
                    Some(if item.prod == aug_prod {
                        Action::Accept
                    } else {
                        Action::Reduce(item.prod)
                    })
                })
                .collect();
            if let Some(kept) = automaton.action(conflict.state, conflict.term) {
                viable.push(*kept);
            }
            viable.sort_by_key(|action| action_key(*action));
            viable.dedup();
            sets.insert((conflict.state, conflict.term), viable);
        }
        Self { sets, productions }
    }

    /// The viable actions for `(state, look)`: the fork set at a recorded
    /// conflict, otherwise the table's single action (empty when the
    /// table has none — the version dies).
    fn viable(&self, automaton: &Automaton, state: StateId, look: Look) -> Vec<Action> {
        match self.sets.get(&(state, look)) {
            Some(set) => set.clone(),
            None => automaton
                .action(state, look)
                .map(|action| vec![*action])
                .unwrap_or_default(),
        }
    }
}

/// Deterministic action ordering: shifts first (the table's own bias),
/// then reductions by production id, accepts last.
fn action_key(action: Action) -> (u8, u32) {
    match action {
        Action::Shift(target) => (0, target.0),
        Action::Reduce(prod) => (1, prod),
        Action::Accept => (2, 0),
    }
}

/// Run the island executor over `tokens` (the automaton's [`Look`]
/// alphabet, pseudo-EOF excluded — the executor appends it itself, the
/// [`crate::driver`] contract).
///
/// Deterministic and total under any [`IslandCaps`]: the same
/// `(automaton, tokens, caps)` triple always yields the same
/// [`IslandRun`]. Level-order sweep: at each feed position the reduce
/// closure runs to fixpoint (forking at recorded conflicts), then every
/// surviving version shifts; versions converge by merge, the cap prunes,
/// and the first accept in execution order wins.
#[must_use]
pub fn parse(automaton: &Automaton, tokens: &[Look], caps: &IslandCaps) -> IslandRun {
    let fork_sets = ForkSets::of(automaton);
    let mut gss = Gss { nodes: Vec::new() };
    let start = gss.spawn(GssNode {
        state: automaton.start,
        cost: 0,
        parents: Vec::new(),
        last_op: None,
        ops: Vec::new(),
    });
    let mut run = IslandRun {
        accepted: false,
        consumed: 0,
        ops: Vec::new(),
        forks: 0,
        merges: 0,
        versions_spawned: 1,
        pruned: 0,
        rejection: None,
    };
    let mut steps: u64 = 0;
    // Deaths: the furthest position a version died at and the union of
    // its expected sets (the rejection report's `at`/`expected`).
    let mut furthest = 0_usize;
    let mut furthest_expected: BTreeSet<Look> = BTreeSet::new();
    // Nodes whose closure round already ran: a merge that links a new
    // parent into a settled node re-queues it (the enriched graph needs
    // its reductions re-fanned).
    let mut settled: BTreeSet<u32> = BTreeSet::new();
    let mut live: Vec<u32> = vec![start];

    for position in 0..=tokens.len() {
        let look = tokens.get(position).copied().unwrap_or(Look::EOF);
        // This level's merge index: state → node (live states are unique
        // — the previous shift phase merged by state).
        let mut closure_index: BTreeMap<StateId, u32> = live
            .iter()
            .map(|id| (gss.nodes[*id as usize].state, *id))
            .collect();
        // Reduce closure to fixpoint: reductions (and the accept) on the
        // current lookahead, forking at recorded conflicts. Shifts defer
        // to the shift phase.
        let mut shifts: Vec<(u32, StateId)> = Vec::new();
        let mut queue: VecDeque<u32> = live.iter().copied().collect();
        let mut queued: BTreeSet<u32> = queue.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            queued.remove(&id);
            settled.insert(id);
            let (state, ops) = {
                let node = &gss.nodes[id as usize];
                (node.state, node.ops.clone())
            };
            let viable = fork_sets.viable(automaton, state, look);
            if viable.is_empty() {
                // The version dies here: record its expected set.
                if position > furthest {
                    furthest = position;
                    furthest_expected.clear();
                }
                if position == furthest {
                    furthest_expected.extend(automaton.expected(state));
                }
                continue;
            }
            if viable.len() > 1 {
                run.forks += 1;
            }
            for action in &viable {
                match *action {
                    Action::Shift(target) => shifts.push((id, target)),
                    Action::Reduce(prod) => {
                        steps += 1;
                        if steps > caps.step_budget {
                            run.consumed = position;
                            run.rejection = Some(IslandRejection {
                                at: position,
                                reason: RejectionReason::StepBudget,
                                expected: furthest_expected.iter().copied().collect(),
                            });
                            return run;
                        }
                        let popped = fork_sets.productions[prod as usize].symbols.len();
                        let lhs = fork_sets.productions[prod as usize].lhs;
                        for (ancestor, ancestor_state, ancestor_cost, ancestor_ops) in
                            reduce_ancestors(&gss, id, popped)
                        {
                            let Some(target) = automaton.goto(ancestor_state, lhs) else {
                                continue;
                            };
                            // The derivation: the ancestor's path, the
                            // popped segment, then the reduce. When the
                            // reducing node's own path runs through the
                            // ancestor, the segment is that path's suffix
                            // beyond it (it carries the fork branches the
                            // chain walk would miss); otherwise the
                            // first-parent walk stands in.
                            let segment = reduce_segment(&gss, id, ancestor, popped);
                            let mut derived = ancestor_ops;
                            derived.extend(segment);
                            derived.push(IslandOp::Reduce(prod));
                            let arrival = GssNode {
                                state: target,
                                cost: ancestor_cost + 1,
                                parents: vec![ancestor],
                                last_op: Some(IslandOp::Reduce(prod)),
                                ops: derived,
                            };
                            join_or_spawn(
                                &mut gss,
                                &mut closure_index,
                                &mut run,
                                arrival,
                                &mut queue,
                                &mut queued,
                                &mut settled,
                            );
                        }
                    }
                    Action::Accept => {
                        // The accept rides the pseudo-EOF: only the
                        // end-of-feed level accepts (defensive otherwise).
                        if position != tokens.len() {
                            continue;
                        }
                        run.accepted = true;
                        run.consumed = tokens.len();
                        run.ops = ops;
                        return run;
                    }
                }
            }
        }
        // Shift phase: every surviving version's shift lands at the next
        // level, merged by `(state, position)`.
        let mut next_index: BTreeMap<StateId, u32> = BTreeMap::new();
        let mut next: Vec<u32> = Vec::new();
        shifts.sort_by_key(|(id, target)| (*id, target.0));
        for (id, target) in shifts {
            steps += 1;
            if steps > caps.step_budget {
                run.consumed = position;
                run.rejection = Some(IslandRejection {
                    at: position,
                    reason: RejectionReason::StepBudget,
                    expected: furthest_expected.iter().copied().collect(),
                });
                return run;
            }
            let (cost, ops) = {
                let node = &gss.nodes[id as usize];
                (node.cost, node.ops.clone())
            };
            let mut derived = ops;
            derived.push(IslandOp::Shift);
            let arrival = GssNode {
                state: target,
                cost: cost + 1,
                parents: vec![id],
                last_op: Some(IslandOp::Shift),
                ops: derived,
            };
            if let Some(existing) = next_index.get(&target).copied() {
                run.merges += 1;
                gss.link(existing, id);
                let node = &mut gss.nodes[existing as usize];
                if arrival.cost < node.cost {
                    node.cost = arrival.cost;
                    node.ops = arrival.ops;
                }
            } else {
                let spawned = gss.spawn(arrival);
                run.versions_spawned += 1;
                next_index.insert(target, spawned);
                next.push(spawned);
            }
        }
        // Fork/merge cap: the next level keeps at most `max_versions`
        // live versions — the cheapest survive, ties by the earliest
        // spawn; the costliest (latest) are dropped.
        if next.len() > caps.max_versions {
            let mut ranked: Vec<(u64, u32)> = next
                .iter()
                .map(|id| (gss.nodes[*id as usize].cost, *id))
                .collect();
            ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
            let keep: BTreeSet<u32> = ranked[..caps.max_versions]
                .iter()
                .map(|(_, id)| *id)
                .collect();
            let dropped = next.len() - keep.len();
            run.pruned += u64::try_from(dropped).expect("prune count fits u64");
            next.retain(|id| keep.contains(id));
        }
        if next.is_empty() {
            stop_no_action(&mut run, furthest, &furthest_expected, tokens.len());
            return run;
        }
        live = next;
    }
    stop_no_action(&mut run, furthest, &furthest_expected, tokens.len());
    run
}

/// The no-accept stop: every version ran out of viable actions (or the
/// sweep ended without an accept) — the furthest death position and the
/// union of its expected sets become the rejection report.
fn stop_no_action(
    run: &mut IslandRun,
    furthest: usize,
    expected: &BTreeSet<Look>,
    tokens_len: usize,
) {
    let at = furthest.min(tokens_len);
    run.consumed = at;
    run.rejection = Some(IslandRejection {
        at,
        reason: RejectionReason::NoAction,
        expected: expected.iter().copied().collect(),
    });
}

/// The ancestors `popped` levels up from `id`, one candidate per parent
/// chain (the GSS fan-out), each as `(arena id, state, cost, derivation)`.
/// Depth-zero reductions (ε) reduce over the node itself.
fn reduce_ancestors(gss: &Gss, id: u32, popped: usize) -> Vec<(u32, StateId, u64, Vec<IslandOp>)> {
    if popped == 0 {
        let node = &gss.nodes[id as usize];
        return vec![(id, node.state, node.cost, node.ops.clone())];
    }
    let mut level: BTreeSet<u32> = BTreeSet::from([id]);
    for _ in 0..popped {
        let mut next_level: BTreeSet<u32> = BTreeSet::new();
        for current in &level {
            for parent in &gss.nodes[*current as usize].parents {
                next_level.insert(*parent);
            }
        }
        if next_level.is_empty() {
            // A malformed table reducing past the start state: no
            // ancestors, nothing to goto from — the version dies.
            return Vec::new();
        }
        level = next_level;
    }
    level
        .into_iter()
        .map(|ancestor| {
            let node = &gss.nodes[ancestor as usize];
            (ancestor, node.state, node.cost, node.ops.clone())
        })
        .collect()
}

/// The popped segment of the derivation: the ops between the ancestor
/// and the reducing node. When the reducing node's surviving path starts
/// with the ancestor's path, the segment is that path's suffix beyond the
/// ancestor (exact — it carries the fork branches the chain walk misses);
/// otherwise the first-parent walk stands in (the fan-out case).
fn reduce_segment(gss: &Gss, id: u32, ancestor: u32, popped: usize) -> Vec<IslandOp> {
    let node_ops = &gss.nodes[id as usize].ops;
    let ancestor_ops = &gss.nodes[ancestor as usize].ops;
    if node_ops.len() >= ancestor_ops.len() && node_ops[..ancestor_ops.len()] == ancestor_ops[..] {
        return node_ops[ancestor_ops.len()..].to_vec();
    }
    popped_segment(gss, id, popped)
}

/// The ops of the chain from the reducing node up `popped` levels
/// (first-parent walk — deterministic), in path order: the popped frames'
/// creating ops. Empty for ε reductions (nothing is popped).
fn popped_segment(gss: &Gss, id: u32, popped: usize) -> Vec<IslandOp> {
    let mut segment: Vec<IslandOp> = Vec::with_capacity(popped);
    let mut current = id;
    for _ in 0..popped {
        let node = &gss.nodes[current as usize];
        let Some(parent) = node.parents.first().copied() else {
            break;
        };
        if let Some(op) = node.last_op {
            segment.push(op);
        }
        current = parent;
    }
    segment.reverse();
    segment
}

/// Join an arriving closure node into the level: merge on equal
/// `(state, position)` (the cheaper derivation survives, equal costs keep
/// the first-discovered node; both parents link — the shared tail), spawn
/// otherwise, and queue fresh nodes for the closure fixpoint. A merge
/// that links a genuinely new parent into a settled node re-queues it:
/// the enriched graph needs its reductions re-fanned over the new paths.
fn join_or_spawn(
    gss: &mut Gss,
    index: &mut BTreeMap<StateId, u32>,
    run: &mut IslandRun,
    arrival: GssNode,
    queue: &mut VecDeque<u32>,
    queued: &mut BTreeSet<u32>,
    settled: &mut BTreeSet<u32>,
) {
    if let Some(existing) = index.get(&arrival.state).copied() {
        run.merges += 1;
        let parent = arrival.parents[0];
        let grew = gss.link(existing, parent);
        let node = &mut gss.nodes[existing as usize];
        if arrival.cost < node.cost {
            node.cost = arrival.cost;
            node.ops = arrival.ops;
        }
        // The arriving version's continuation rides the merged node;
        // a NEW parent edge means new reduce paths — re-explore.
        if grew && settled.contains(&existing) && !queued.contains(&existing) {
            queue.push_back(existing);
            queued.insert(existing);
            settled.remove(&existing);
        }
    } else {
        let spawned = gss.spawn(arrival);
        run.versions_spawned += 1;
        index.insert(gss.nodes[spawned as usize].state, spawned);
        if queued.insert(spawned) {
            queue.push_back(spawned);
        }
    }
}

#[cfg(test)]
mod tests {
    use gir_ir::{GrammarBuilder, TokenId};

    use super::{IslandCaps, IslandOp, parse};
    use crate::automaton::Automaton;
    use crate::lower::Look;

    /// The classic ambiguous nested-pair grammar `s → a s a | ε`.
    fn nested() -> (Automaton, TokenId) {
        let mut b = GrammarBuilder::new("nested");
        let a = b.token_lit("a", "a").expect("token");
        let s = b.declare_rule("s").expect("rule");
        b.set_body(
            s,
            GrammarBuilder::alt(vec![
                GrammarBuilder::seq(vec![
                    GrammarBuilder::token_ref(a),
                    GrammarBuilder::rule_ref(s),
                    GrammarBuilder::token_ref(a),
                ]),
                GrammarBuilder::seq(vec![]),
            ]),
        );
        b.set_allow_ambiguous(s, true);
        let grammar = b.finish().expect("valid grammar");
        (Automaton::build(&grammar), a)
    }

    #[test]
    fn the_winner_derivation_replays_the_shift_reduce_sequence() {
        let (automaton, a) = nested();
        let feed = [Look::token(a), Look::token(a)];
        let run = parse(&automaton, &feed, &IslandCaps::default());
        assert!(run.accepted);
        // The accepting derivation: shift the first `a`, reduce the ε
        // alternative over it, shift the closing `a`, reduce the outer
        // `a s a`.
        let shifts = run
            .ops
            .iter()
            .filter(|op| matches!(op, IslandOp::Shift))
            .count();
        let reduces = run
            .ops
            .iter()
            .filter(|op| matches!(op, IslandOp::Reduce(_)))
            .count();
        assert_eq!(shifts, 2, "two tokens shifted");
        assert_eq!(reduces, 2, "the ε reduce and the outer `a s a` reduce");
    }
}

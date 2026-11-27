//! The table-driven LR(1) driver (roadmap §7.4 "table-driven LR (O1
//! tier)"): the runtime half of the shadow automaton. The automaton's
//! `action`/`goto` tables drive a deterministic stack machine over a
//! token stream; the driver is the table backend the cross-backend
//! oracle (`syntact test-all`, §7.4) differential-tests against the
//! descent engine — "our own backends keep each other honest".
//!
//! The machine is total: every input terminates with an accepted or a
//! rejected [`ParseOutcome`]. A rejected outcome carries the error
//! state, the offending feed index, and the state's expected terminal
//! set — the §7.4 expected-set extraction, consumed by the oracle's
//! diagnostics.

use crate::automaton::{Action, Automaton, StateId};
use crate::lower::Look;

/// Why and where the table machine stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    /// The state the machine sat in when the terminal had no action.
    pub state: u32,
    /// The feed index of the offending terminal (`tokens.len()` when the
    /// pseudo-EOF carried no action).
    pub at: usize,
    /// The error state's expected terminal set (sorted: tokens by id,
    /// EOF last — [`Automaton::expected`]).
    pub expected: Vec<Look>,
}

/// The result of one table-driven run: acceptance plus where the machine
/// stopped (`PartialEq`/`Eq` so the oracle can pin determinism).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutcome {
    /// The whole feed derived down to the pseudo-EOF accept action.
    pub accepted: bool,
    /// Tokens consumed when the run stopped (== `tokens.len()` on
    /// accept; the offending index on reject).
    pub consumed: usize,
    /// The rejection step (`None` on accept).
    pub error: Option<StepError>,
    /// Shift actions performed.
    pub shifts: u64,
    /// Reduce actions performed.
    pub reduces: u64,
}

/// Build the rejection outcome at `state`/`at` with the machine's
/// counters (module-level so `parse` reads as one loop).
fn rejected(
    automaton: &Automaton,
    state: StateId,
    at: usize,
    shifts: u64,
    reduces: u64,
) -> ParseOutcome {
    ParseOutcome {
        accepted: false,
        consumed: at,
        error: Some(StepError {
            state: state.0,
            at,
            expected: automaton.expected(state),
        }),
        shifts,
        reduces,
    }
}

/// Run the table-driven LR(1) machine over `tokens` (a scanner feed
/// mapped into the automaton's [`Look`] alphabet, pseudo-EOF excluded —
/// the machine appends it itself).
///
/// Deterministic and total: the same `(automaton, tokens)` pair always
/// yields the same outcome. Well-formed canonical LR(1) runs are
/// polynomial in the feed length; a deterministic step budget (feed
/// length × states + productions, documented below) turns the
/// ε-reduce/goto cycles a hand-built or ε-cyclic table can contain into
/// a rejection instead of a hang — the recovery planner's work-budget
/// posture (§7.5) applied to the runtime.
#[must_use]
pub fn parse(automaton: &Automaton, tokens: &[Look]) -> ParseOutcome {
    let feed_len = u64::try_from(tokens.len()).unwrap_or(u64::MAX);
    let states = u64::try_from(automaton.states.len()).unwrap_or(u64::MAX);
    let productions = u64::try_from(automaton.lowered.productions.len()).unwrap_or(u64::MAX);
    // The step budget: every well-formed run shifts at most once per
    // token and interleaves at most states × productions ε-reductions
    // between two shifts (each ε-reduce/goto pair that repeats would be
    // a cycle). A hand-built ε-cyclic table exhausts the budget and is
    // rejected at the budget point — deterministically, so the outcome
    // stays a pure function of the input.
    let budget = feed_len
        .saturating_mul(states.saturating_add(productions))
        .saturating_add(feed_len)
        .saturating_add(64);

    let mut stack: Vec<StateId> = vec![automaton.start];
    let mut cursor = 0_usize;
    let mut steps = 0_u64;
    let (mut shifts, mut reduces) = (0_u64, 0_u64);

    loop {
        if steps > budget {
            let state = stack.last().copied().unwrap_or(automaton.start);
            return rejected(automaton, state, cursor, shifts, reduces);
        }
        steps += 1;

        let Some(&state) = stack.last() else {
            // Unreachable through construction (the start state is never
            // popped: reduces keep the stack non-empty), but the machine
            // stays total over hand-built tables.
            return rejected(automaton, automaton.start, cursor, shifts, reduces);
        };
        let look = tokens.get(cursor).copied().unwrap_or(Look::EOF);
        let Some(action) = automaton.action(state, look).copied() else {
            return rejected(automaton, state, cursor, shifts, reduces);
        };
        match action {
            Action::Shift(target) => {
                stack.push(target);
                cursor += 1;
                shifts += 1;
            }
            Action::Reduce(prod) => {
                // Reduce actions only ever reference productions of the
                // lowering (the augmented production reduces to Accept).
                let Some(production) = automaton.lowered.productions.get(prod as usize) else {
                    return rejected(automaton, state, cursor, shifts, reduces);
                };
                let Some(top) = stack.len().checked_sub(production.symbols.len()) else {
                    // A malformed table reducing more states than exist.
                    return rejected(automaton, state, cursor, shifts, reduces);
                };
                if top == 0 {
                    return rejected(automaton, state, cursor, shifts, reduces);
                }
                stack.truncate(top);
                let Some(&frame) = stack.last() else {
                    return rejected(automaton, state, cursor, shifts, reduces);
                };
                let Some(target) = automaton.goto(frame, production.lhs) else {
                    return rejected(automaton, state, cursor, shifts, reduces);
                };
                stack.push(target);
                reduces += 1;
            }
            Action::Accept => {
                debug_assert_eq!(cursor, tokens.len(), "accept rides the pseudo-EOF");
                return ParseOutcome {
                    accepted: true,
                    consumed: cursor,
                    error: None,
                    shifts,
                    reduces,
                };
            }
        }
        if cursor > tokens.len() {
            // Defensive: a malformed table shifting on the pseudo-EOF.
            let state = stack.last().copied().unwrap_or(automaton.start);
            return rejected(automaton, state, cursor, shifts, reduces);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use gir_ir::{Grammar, GrammarBuilder, RuleId, TokenId};

    use crate::automaton::{Action, Automaton, State, StateId};
    use crate::driver::parse;
    use crate::lower::{Look, Lowered, NontermId, ProdKind, Production};

    /// A one-rule grammar with tokens `a`/`b` (`kind=lit`), plus a start
    /// rule body set by `set`; returns the grammar, `a`, and `b`.
    fn tiny_grammar(
        set: fn(&mut GrammarBuilder, TokenId, TokenId, RuleId),
    ) -> (Grammar, TokenId, TokenId) {
        let mut b = GrammarBuilder::new("tiny");
        let a = b.token_lit("a", "a").expect("t");
        let c = b.token_lit("b", "b").expect("t");
        let s = b.declare_rule("s").expect("r");
        set(&mut b, a, c, s);
        (b.finish().expect("valid"), a, c)
    }

    #[test]
    fn accepts_the_start_rule_derivation() {
        let (grammar, a, _c) = tiny_grammar(|b, a, _c, s| {
            b.set_body(s, GrammarBuilder::token_ref(a));
        });
        let automaton = Automaton::build(&grammar);
        let outcome = parse(&automaton, &[Look::token(a)]);
        assert!(outcome.accepted);
        assert_eq!(outcome.consumed, 1);
        assert!(outcome.error.is_none());
        assert_eq!(outcome.shifts, 1);
        assert_eq!(outcome.reduces, 1, "`s → a` reduces once before accept");
    }

    #[test]
    fn rejects_the_empty_feed_when_the_start_is_not_nullable() {
        let (grammar, a, _c) = tiny_grammar(|b, a, _c, s| {
            b.set_body(s, GrammarBuilder::token_ref(a));
        });
        let automaton = Automaton::build(&grammar);
        let outcome = parse(&automaton, &[]);
        assert!(!outcome.accepted);
        assert_eq!(outcome.consumed, 0);
        let error = outcome.error.expect("rejection carries the step error");
        assert_eq!(error.at, 0, "the pseudo-EOF position");
        assert_eq!(error.expected, vec![Look::token(a)]);
    }

    #[test]
    fn accepts_empty_when_the_start_is_nullable() {
        let (grammar, a, _c) = tiny_grammar(|b, a, _c, s| {
            b.set_body(s, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
        });
        let automaton = Automaton::build(&grammar);
        let outcome = parse(&automaton, &[]);
        assert!(outcome.accepted, "ε derivation of a nullable start");
        let outcome = parse(&automaton, &[Look::token(a)]);
        assert!(outcome.accepted);
    }

    #[test]
    fn reduce_goto_loop_parses_repeats() {
        // s → a* (the `rep` marker) — star iterations ride the
        // ε/shift/reduce cycle of the synthetic star nonterminal.
        let (grammar, a, c) = tiny_grammar(|b, a, _c, s| {
            b.set_body(s, GrammarBuilder::rep0(GrammarBuilder::token_ref(a)));
        });
        let automaton = Automaton::build(&grammar);
        let looks = |tokens: &[TokenId]| -> Vec<Look> {
            tokens.iter().map(|&token| Look::token(token)).collect()
        };
        for tokens in [&[a][..], &[a, a][..], &[a, a, a][..], &[][..]] {
            let outcome = parse(&automaton, &looks(tokens));
            assert!(outcome.accepted, "{tokens:?} parses");
            assert_eq!(outcome.consumed, tokens.len());
        }
        let outcome = parse(&automaton, &looks(&[a, c]));
        assert!(!outcome.accepted, "a trailing `b` rejects");
    }

    #[test]
    fn same_input_same_outcome() {
        let (grammar, a, c) = tiny_grammar(|b, a, c, s| {
            b.set_body(
                s,
                GrammarBuilder::seq(vec![
                    GrammarBuilder::token_ref(a),
                    GrammarBuilder::token_ref(c),
                ]),
            );
        });
        let automaton = Automaton::build(&grammar);
        let feed = [Look::token(a), Look::token(c)];
        let first = parse(&automaton, &feed);
        let second = parse(&automaton, &feed);
        assert_eq!(first, second);
    }

    #[test]
    fn error_carries_the_expected_terminal_set() {
        let (grammar, a, c) = tiny_grammar(|b, a, c, s| {
            b.set_body(
                s,
                GrammarBuilder::seq(vec![
                    GrammarBuilder::token_ref(a),
                    GrammarBuilder::token_ref(c),
                ]),
            );
        });
        let automaton = Automaton::build(&grammar);
        // After the `a` shift the next state expects `c` (and only `c`).
        let outcome = parse(&automaton, &[Look::token(a)]);
        assert!(!outcome.accepted);
        let error = outcome.error.expect("expected-set report");
        assert_eq!(error.at, 1);
        assert_eq!(error.expected, vec![Look::token(c)]);
    }

    #[test]
    fn unknown_terminals_reject_without_panicking() {
        let (grammar, _a, _c) = tiny_grammar(|b, a, _c, s| {
            b.set_body(s, GrammarBuilder::token_ref(a));
        });
        let automaton = Automaton::build(&grammar);
        // A terminal no production mentions (the oracle maps scanner-
        // unmatched `bad` tokens here): no table action, clean rejection.
        let stranger = Look(u32::from(u16::MAX));
        let outcome = parse(&automaton, &[stranger]);
        assert!(!outcome.accepted);
        assert_eq!(outcome.error.expect("rejects").at, 0);
    }

    #[test]
    fn malformed_table_epsilon_cycle_hits_the_step_budget() {
        // A hand-built automaton whose ε-reduce/goto pair loops forever
        // (state 0 reduces an ε-production and gotos back into itself).
        // The driver's deterministic step budget turns the cycle into a
        // rejection instead of hanging.
        let lowered = Lowered {
            productions: vec![Production {
                lhs: NontermId(0),
                symbols: Vec::new(),
                kind: ProdKind::Epsilon,
                origin: None,
            }],
            nonterms: 1,
            start: NontermId(0),
            predicates: BTreeMap::new(),
        };
        let mut actions = BTreeMap::new();
        actions.insert(Look(0), Action::Reduce(0));
        let mut gotos = BTreeMap::new();
        gotos.insert(NontermId(0), StateId(0));
        let automaton = Automaton {
            states: vec![State {
                kernel: BTreeSet::new(),
                actions,
                gotos,
            }],
            conflicts: Vec::new(),
            start: StateId(0),
            lowered,
        };
        let outcome = parse(&automaton, &[Look(0)]);
        assert!(!outcome.accepted, "the cycle is rejected, never spun on");
        assert!(outcome.error.is_some());
    }
}

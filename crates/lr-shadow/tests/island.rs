//! The GLR island executor gates (roadmap §7.4, ADR-007).
//!
//! Plan lines pinned (§7.4): *"GLR islands for regions the analyzer marks
//! ambiguous: local graph-structured stack … with shared tails on fork …
//! fork/merge caps borrowed from tree-sitter's version bounds —
//! MAX_VERSION_COUNT-style limits, deterministic merge on equal (state,
//! position, cost)"*; and §12.2's status note: *"the island executor — the
//! arena GSS with fork/merge caps — does not"*. The headline gate: the
//! shift-wins-resolved table dead-ends on the ambiguous derivation, and
//! the island executor — forking at the recorded conflict — accepts it.

use gir_ir::GrammarBuilder;
use lr_shadow::island::{IslandCaps, RejectionReason};
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, driver, island};

/// The ambiguous pair grammar — the same shape the CLI ambiguity gate
/// pins (`crates/cli/tests/ambiguity.rs`): two alternatives with an
/// identical FIRST, a recorded reduce/reduce conflict, and two reduce
/// derivations that converge on the same goto state (the merge case).
fn pair_grammar() -> (Automaton, gir_ir::TokenId) {
    let mut builder = GrammarBuilder::new("pair");
    let token_a = builder.token_lit("a", "a").expect("token");
    let rule_s = builder.declare_rule("s").expect("rule");
    let rule_p = builder.declare_rule("p").expect("rule");
    let rule_q = builder.declare_rule("q").expect("rule");
    builder.set_body(
        rule_s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::rule_ref(rule_p),
            GrammarBuilder::rule_ref(rule_q),
        ]),
    );
    builder.set_body(
        rule_p,
        GrammarBuilder::seq(vec![GrammarBuilder::token_ref(token_a)]),
    );
    builder.set_body(
        rule_q,
        GrammarBuilder::seq(vec![GrammarBuilder::token_ref(token_a)]),
    );
    builder.set_allow_ambiguous(rule_s, true);
    let grammar = builder.finish().expect("valid grammar");
    (Automaton::build(&grammar), token_a)
}

/// The classic ambiguous nested-pair grammar `s → a s a | ε`: on `a a` the
/// shift-wins-resolved table dead-ends (the inner shift consumes both
/// tokens, then no closing `a` remains), while the reduce fork accepts.
fn nested_grammar() -> (Automaton, gir_ir::TokenId) {
    let mut builder = GrammarBuilder::new("nested");
    let token_a = builder.token_lit("a", "a").expect("token");
    let rule_s = builder.declare_rule("s").expect("rule");
    builder.set_body(
        rule_s,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(token_a),
                GrammarBuilder::rule_ref(rule_s),
                GrammarBuilder::token_ref(token_a),
            ]),
            GrammarBuilder::seq(vec![]),
        ]),
    );
    builder.set_allow_ambiguous(rule_s, true);
    let grammar = builder.finish().expect("valid grammar");
    (Automaton::build(&grammar), token_a)
}

#[test]
fn the_island_accepts_what_the_shift_wins_table_dead_ends_on() {
    let (automaton, token_a) = nested_grammar();
    let feed = [Look::token(token_a), Look::token(token_a)];

    let table = driver::parse(&automaton, &feed);
    assert!(
        !table.accepted,
        "the shift-wins resolution dead-ends on the ambiguous derivation"
    );

    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    assert!(
        run.accepted,
        "the island executor forks at the recorded conflict and accepts"
    );
    assert_eq!(
        run.consumed,
        feed.len(),
        "the winning derivation eats the feed"
    );
    assert!(run.rejection.is_none());
    assert!(
        run.forks >= 1,
        "the GSS forked at the recorded shift/reduce conflict"
    );
}

#[test]
fn reduce_derivations_converging_on_one_goto_state_merge() {
    let (automaton, token_a) = pair_grammar();
    let feed = [Look::token(token_a)];

    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    assert!(run.accepted, "both reduce derivations accept");
    assert!(run.forks >= 1, "the reduce/reduce conflict forks the stack");
    assert!(
        run.merges >= 1,
        "the two derivations converge on the same (state, position) and merge"
    );
    assert!(
        run.versions_spawned < run.forks + run.merges + 4,
        "the second arrival joined the existing goto frame (five arena \
         nodes: start, shifted token, the two reduce forks, the merged \
         frame) instead of spawning a sixth"
    );
}

#[test]
fn same_input_same_run() {
    let (automaton, token_a) = nested_grammar();
    let feed = [Look::token(token_a), Look::token(token_a)];
    let first = island::parse(&automaton, &feed, &IslandCaps::default());
    let second = island::parse(&automaton, &feed, &IslandCaps::default());
    assert_eq!(first, second, "the executor is a deterministic function");
}

#[test]
fn unambiguous_grammars_run_one_linear_stack() {
    let mut builder = GrammarBuilder::new("plain");
    let token_a = builder.token_lit("a", "a").expect("token");
    let token_c = builder.token_lit("c", "c").expect("token");
    let rule_s = builder.declare_rule("s").expect("rule");
    builder.set_body(
        rule_s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(token_a),
            GrammarBuilder::token_ref(token_c),
        ]),
    );
    let automaton = Automaton::build(&builder.finish().expect("valid"));

    let feed = [Look::token(token_a), Look::token(token_c)];
    let run = island::parse(&automaton, &feed, &IslandCaps::default());
    assert!(run.accepted);
    assert_eq!(run.forks, 0, "no conflicts, no forks");
    assert_eq!(run.merges, 0, "one derivation, nothing to merge");

    let table = driver::parse(&automaton, &feed);
    assert_eq!(run.accepted, table.accepted, "backends agree on accept");
    assert_eq!(run.consumed, table.consumed);

    let stranger = Look(u32::MAX - 1);
    let run = island::parse(&automaton, &[stranger], &IslandCaps::default());
    assert!(!run.accepted, "a terminal no production mentions rejects");
    let rejection = run.rejection.expect("the rejection is reported");
    assert_eq!(rejection.at, 0);
    assert_eq!(rejection.reason, RejectionReason::NoAction);
}

#[test]
fn the_version_cap_bounds_live_versions_deterministically() {
    let (automaton, token_a) = nested_grammar();
    let feed = [Look::token(token_a), Look::token(token_a)];

    // The default cap leaves both fork branches alive; the reduce branch
    // is the one that accepts.
    let generous = island::parse(&automaton, &feed, &IslandCaps::default());
    assert!(generous.accepted);

    // A one-version cap prunes the fork deterministically: the surviving
    // (shift) branch dead-ends, and the executor reports the rejection
    // instead of exploring an unbounded graph.
    let tight = island::parse(
        &automaton,
        &feed,
        &IslandCaps {
            max_versions: 1,
            ..IslandCaps::default()
        },
    );
    assert!(!tight.accepted, "the accepting fork was pruned by the cap");
    assert!(
        tight.pruned >= 1,
        "the cap pruned at least one live version"
    );
    assert_eq!(
        tight.rejection.expect("reported").reason,
        RejectionReason::NoAction,
    );
}

#[test]
fn the_step_budget_turns_exhaustion_into_a_rejection() {
    let (automaton, token_a) = nested_grammar();
    let feed = [Look::token(token_a), Look::token(token_a)];
    let run = island::parse(
        &automaton,
        &feed,
        &IslandCaps {
            step_budget: 2,
            ..IslandCaps::default()
        },
    );
    assert!(!run.accepted, "the budget stops the search");
    let rejection = run.rejection.as_ref().expect("the budget stop is reported");
    assert_eq!(rejection.reason, RejectionReason::StepBudget);
    // Determinism: the same budget reproduces the same stop.
    let again = island::parse(
        &automaton,
        &feed,
        &IslandCaps {
            step_budget: 2,
            ..IslandCaps::default()
        },
    );
    assert_eq!(run, again);
}

#[test]
fn empty_feed_rejects_a_non_nullable_start_and_accepts_a_nullable_one() {
    let (nested, _token_a) = nested_grammar();
    let run = island::parse(&nested, &[], &IslandCaps::default());
    assert!(run.accepted, "`s → a s a | ε` derives ε");

    let (pair, _token_a) = pair_grammar();
    let run = island::parse(&pair, &[], &IslandCaps::default());
    assert!(!run.accepted, "`s → p | q` with non-ε branches rejects ε");
    assert_eq!(
        run.rejection.expect("reported").reason,
        RejectionReason::NoAction,
    );
}

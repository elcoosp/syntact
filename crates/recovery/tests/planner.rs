//! Repair-planner integration tests (issue 016 TDD): hand-built stack
//! fixtures (real shadow-automaton stacks driven over valid prefixes),
//! determinism, canonical ordering, budget honoring and sync discounts.

use gir_ir::{Grammar, GrammarBuilder, TokenId};
use lr_shadow::{Action, Automaton, Look, StateId};
use recovery::{CostModel, Planner, RepairOp, SyncHints};

/// JSON-shaped fixture (same shape as the lr-shadow tests).
fn json_grammar() -> Grammar {
    let mut b = GrammarBuilder::new("json");
    let lbrace = b.token_lit("lbrace", "{").expect("t");
    let rbrace = b.token_lit("rbrace", "}").expect("t");
    let left_bracket = b.token_lit("lbrack", "[").expect("t");
    let right_bracket = b.token_lit("rbrack", "]").expect("t");
    let colon = b.token_lit("colon", ":").expect("t");
    let comma = b.token_lit("comma", ",").expect("t");
    let string = b.token_re("string", "\"[^\"]*\"").expect("t");
    let number = b.token_re("number", "[0-9]+").expect("t");

    let value = b.declare_rule("value").expect("r");
    let object = b.declare_rule("object").expect("r");
    let array = b.declare_rule("array").expect("r");
    let pair = b.declare_rule("pair").expect("r");
    b.set_body(
        value,
        GrammarBuilder::alt(vec![
            GrammarBuilder::rule_ref(object),
            GrammarBuilder::rule_ref(array),
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(number),
        ]),
    );
    b.set_body(
        object,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(lbrace),
            GrammarBuilder::rule_ref(pair),
            GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(comma),
                GrammarBuilder::rule_ref(pair),
            ])),
            GrammarBuilder::token_ref(rbrace),
        ]),
    );
    b.set_body(
        array,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(left_bracket),
            GrammarBuilder::opt(GrammarBuilder::rule_ref(value)),
            GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(comma),
                GrammarBuilder::rule_ref(value),
            ])),
            GrammarBuilder::token_ref(right_bracket),
        ]),
    );
    b.set_body(
        pair,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(string),
            GrammarBuilder::token_ref(colon),
            GrammarBuilder::rule_ref(value),
        ]),
    );
    b.finish().expect("valid")
}

fn tok(grammar: &Grammar, name: &str) -> TokenId {
    grammar
        .tokens
        .iter()
        .find(|token| grammar.interner.lookup(token.name) == name)
        .expect("fixture token")
        .id
}

fn look(grammar: &Grammar, name: &str) -> Look {
    Look::token(tok(grammar, name))
}

/// Drive the LR tables over a token stream, returning the resulting
/// cactus-stack prefix (the hand-built fixture stack). Reductions do not
/// consume input: the same lookahead is re-examined until it shifts.
fn stack_after(auto: &Automaton, stream: &[Look]) -> Vec<StateId> {
    let mut stack = vec![auto.start];
    for &term in stream {
        loop {
            let state = *stack.last().expect("stack never empty");
            match auto.action(state, term).copied().expect("valid prefix") {
                Action::Shift(target) => {
                    stack.push(target);
                    break;
                }
                Action::Reduce(prod) => {
                    let production = &auto.lowered.productions[prod as usize];
                    stack.truncate(stack.len() - production.symbols.len());
                    let base = *stack.last().expect("stack never empty");
                    let target = auto
                        .goto(base, production.lhs)
                        .expect("goto exists for reachable lhs");
                    stack.push(target);
                }
                Action::Accept => panic!("prefix drove to accept"),
            }
        }
    }
    stack
}

// ---------------------------------------------------------------------------
// Suites
// ---------------------------------------------------------------------------

#[test]
fn wrong_closer_is_delete_then_insert() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // Stack after `{"a": 1` — inside the object, expecting `,` or `}`.
    let stack = stack_after(
        &auto,
        &[
            look(&grammar, "lbrace"),
            look(&grammar, "string"),
            look(&grammar, "colon"),
            look(&grammar, "number"),
        ],
    );
    // The user typed `]` instead of closing the object. Repair: delete
    // the stray `]`, insert the expected `}` — then the window is empty
    // and the stack accepts at EOF.
    let planner = Planner::new(&auto);
    let plan = planner
        .plan(&stack, &[look(&grammar, "rbrack")], 512)
        .expect("repairable");
    // Both orders cost 4; the canonical tie-break picks the
    // lexicographically smallest op SEQUENCE — `Insert` < `Delete`.
    assert_eq!(
        plan.ops,
        vec![
            RepairOp::Insert(look(&grammar, "rbrace")),
            RepairOp::Delete(look(&grammar, "rbrack")),
        ],
        "canonical tied-cost order: Insert < Delete"
    );
    assert_eq!(plan.cost, 4, "delete 2 + insert 2 under the default model");
}

#[test]
fn stray_token_inside_object_is_deleted() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // Stack after `{` — expecting the first pair's string key.
    let stack = stack_after(&auto, &[look(&grammar, "lbrace")]);
    // A stray number where the key belongs, followed by the intended rest.
    let planner = Planner::new(&auto);
    let plan = planner
        .plan(
            &stack,
            &[
                look(&grammar, "number"),
                look(&grammar, "string"),
                look(&grammar, "colon"),
                look(&grammar, "number"),
                look(&grammar, "rbrace"),
            ],
            8192,
        )
        .expect("repairable");
    assert_eq!(plan.ops, vec![RepairOp::Delete(look(&grammar, "number"))]);
    assert_eq!(plan.cost, 2);
}

#[test]
fn missing_closer_at_eof_is_one_insert() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // Stack after `[1, 2` — inside the array, input ended.
    let stack = stack_after(
        &auto,
        &[
            look(&grammar, "lbrack"),
            look(&grammar, "number"),
            look(&grammar, "comma"),
            look(&grammar, "number"),
        ],
    );
    let planner = Planner::new(&auto);
    let plan = planner.plan(&stack, &[], 512).expect("repairable");
    assert_eq!(plan.ops, vec![RepairOp::Insert(look(&grammar, "rbrack"))]);
    assert_eq!(plan.cost, 2);
}

#[test]
fn planning_is_deterministic_and_canonical() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    let stack = stack_after(&auto, &[look(&grammar, "lbrace")]);
    let lookahead = [look(&grammar, "number")];
    let planner = Planner::new(&auto);
    let first = planner.plan(&stack, &lookahead, 512).expect("repairable");
    for _ in 0..5 {
        let again = planner.plan(&stack, &lookahead, 512).expect("repairable");
        assert_eq!(first, again, "same input, same plan");
    }
    // Canonical tie-break: with all costs tied, the lexicographically
    // smallest op sequence wins — stable across repeated runs.
    let tied = CostModel {
        insert: 1,
        delete: 1,
        sync_delete: 1,
        ..CostModel::default()
    };
    let a = Planner::new(&auto)
        .with_cost_model(tied)
        .plan(&stack, &lookahead, 512);
    let b = Planner::new(&auto)
        .with_cost_model(tied)
        .plan(&stack, &lookahead, 512);
    assert_eq!(a, b, "tied costs still canonical");
}

#[test]
fn work_budget_is_honored() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    let stack = stack_after(
        &auto,
        &[
            look(&grammar, "lbrace"),
            look(&grammar, "string"),
            look(&grammar, "colon"),
            look(&grammar, "number"),
        ],
    );
    let lookahead = [look(&grammar, "rbrack")];
    let planner = Planner::new(&auto);
    assert_eq!(planner.plan(&stack, &lookahead, 0), None, "zero budget");
    assert_eq!(planner.plan(&stack, &lookahead, 1), None, "one expansion");
    assert!(
        planner.plan(&stack, &lookahead, 64).is_some(),
        "small but sufficient budget"
    );
    // Budget growth never changes the plan, only availability.
    let plan512 = planner.plan(&stack, &lookahead, 512).expect("plan");
    let plan4096 = planner.plan(&stack, &lookahead, 4096).expect("plan");
    assert_eq!(plan512, plan4096);
}

#[test]
fn sync_hints_discount_boundary_deletes() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // Stack after `{` — a stray `,` where the key belongs; the rest of
    // the window parses cleanly after deleting it.
    let stack = stack_after(&auto, &[look(&grammar, "lbrace")]);
    let lookahead = [
        look(&grammar, "comma"),
        look(&grammar, "string"),
        look(&grammar, "colon"),
        look(&grammar, "number"),
        look(&grammar, "rbrace"),
    ];
    let plain = Planner::new(&auto)
        .plan(&stack, &lookahead, 512)
        .expect("repairable");
    assert_eq!(plain.ops, vec![RepairOp::Delete(look(&grammar, "comma"))]);
    assert_eq!(plain.cost, 2, "plain delete under the default model");

    let hinted = Planner::new(&auto)
        .with_sync_hints(SyncHints::from_tokens([look(&grammar, "comma")]))
        .plan(&stack, &lookahead, 512)
        .expect("repairable");
    assert_eq!(hinted.ops, plain.ops, "same ops — the hint only reprices");
    assert_eq!(hinted.cost, 1, "sync-hinted delete rides the discount");
}

#[test]
fn empty_window_on_completed_document_needs_no_repair() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // The `[]` document fully shifted: the stack reduces to accept at
    // EOF with zero repair operations.
    let stack = stack_after(&auto, &[look(&grammar, "lbrack"), look(&grammar, "rbrack")]);
    let planner = Planner::new(&auto);
    let plan = planner.plan(&stack, &[], 64).expect("accepts");
    assert_eq!(plan.ops, vec![], "no repair needed");
    assert_eq!(plan.cost, 0);
}

#[test]
fn unrepairable_within_tiny_budget_returns_none() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // A stack inside an object with a garbage window: the search needs
    // more expansions than the tiny budget allows — no complete repair
    // is reported.
    let stack = stack_after(&auto, &[look(&grammar, "lbrace")]);
    let planner = Planner::new(&auto);
    assert_eq!(
        planner.plan(&stack, &[look(&grammar, "rbrace"); 3], 2),
        None,
        "budget exhausted before any complete repair"
    );
}

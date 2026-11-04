# Recovery

`recovery` plans repairs for broken input *before* the parser
resyncs: a uniform-cost search over `Insert`/`Delete` operations
against the LR shadow automaton, bounded by a budget and a cost model,
with sync-token discounts so the search stops climbing at statement
boundaries.

The pieces:

- `Planner::new(&automaton)` — build over the shadow automaton; add
  `.with_cost_model(...)` and `.with_sync_hints(...)` as needed.
- `plan(stack, lookahead, budget)` — the cheapest canonical repair for
  the current parser stack and lookahead window, or `None` when the
  budget is exhausted.
- Canonical order — tied-cost repairs sort `Insert` before `Delete`,
  so the same damage always plans identically.

The recipe builds the JSON grammar, drives the automaton over `{`,
then plans a repair for a stray number where the pair's key belongs:

```rust
// Recipe: plan a repair for broken input with the recovery planner.
//
// The planner is a uniform-cost search over `Insert`/`Delete` repairs
// against the LR shadow automaton's parser stack. This recipe builds
// the JSON grammar, drives the automaton over `{`, then asks for the
// cheapest repair of a stray number where the pair's key belongs.

use gir_ir::{Grammar, GrammarBuilder, TokenId};
use lr_shadow::{Action, Automaton, Look, StateId};
use recovery::{Planner, RepairOp};

/// The JSON-shaped grammar (rules mirror the pack's grammar.kdl).
fn json_grammar() -> Grammar {
    let mut b = GrammarBuilder::new("json");
    let brace_open = b.token_lit("lbrace", "{").expect("token");
    let brace_close = b.token_lit("rbrace", "}").expect("token");
    let bracket_open = b.token_lit("lbrack", "[").expect("token");
    let bracket_close = b.token_lit("rbrack", "]").expect("token");
    let colon = b.token_lit("colon", ":").expect("token");
    let comma = b.token_lit("comma", ",").expect("token");
    let string = b.token_re("string", "\"[^\"]*\"").expect("token");
    let number = b.token_re("number", "[0-9]+").expect("token");

    let value = b.declare_rule("value").expect("rule");
    let object = b.declare_rule("object").expect("rule");
    let array = b.declare_rule("array").expect("rule");
    let pair = b.declare_rule("pair").expect("rule");
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
            GrammarBuilder::token_ref(brace_open),
            GrammarBuilder::rule_ref(pair),
            GrammarBuilder::rep0(GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(comma),
                GrammarBuilder::rule_ref(pair),
            ])),
            GrammarBuilder::token_ref(brace_close),
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
    b.set_body(
        array,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(bracket_open),
            GrammarBuilder::rep0(GrammarBuilder::rule_ref(value)),
            GrammarBuilder::token_ref(bracket_close),
        ]),
    );
    b.finish().expect("valid grammar")
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

/// Drive the automaton over a valid prefix, returning the parser stack.
/// Reductions never consume input: the same lookahead is re-examined
/// until it shifts.
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

#[test]
fn stray_token_plans_a_single_delete() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);

    // Stack after `{` — the parser expects the pair's string key next.
    let stack = stack_after(&auto, &[look(&grammar, "lbrace")]);

    // Input: a stray number where the key belongs, then the intended
    // rest of the object. The cheapest repair deletes the stray token.
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
    assert_eq!(plan.cost, 2, "one delete under the default cost model");
}
```


In the packs, the planned repairs ride the trace: `syntact trace json
broken.json` prints, per recovery region, the repair ops and the resync
point — the same plan the parser applies while wrapping damage in
`ERROR_KIND` nodes.

That is the whole toolkit: one event stream, lossless trees, incremental
reuse, queries, and planned recovery. The [cookbook
recipes](https://github.com/elcoosp/syntact) live in
`docs/cookbook/recipes/` — each one is compiled and executed by the
workspace test suite, so the docs cannot rot.

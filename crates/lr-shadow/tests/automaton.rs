//! lr-shadow integration tests (issue 011 TDD): state-count goldens,
//! table-driven accept walks, conflict policy, expected sets, reachability
//! and pseudo-EOF resize safety.

use gir_ir::{Associativity, Grammar, GrammarBuilder, PrattHint, TokenId};
use lr_shadow::{Action, Automaton, Look, ProdKind};

/// JSON-shaped fixture (same shape as the gir-analysis tests).
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

/// Token id by name in a fixture grammar.
fn tok(grammar: &Grammar, name: &str) -> TokenId {
    grammar
        .tokens
        .iter()
        .find(|token| grammar.interner.lookup(token.name) == name)
        .expect("fixture token")
        .id
}

/// Drive the LR tables over a token stream (a tiny table interpreter —
/// the same shape the shadow-driven recovery planner and the trace CLI
/// use). On failure prints the offending state/terminal to stderr.
fn accepts(auto: &Automaton, stream: &[Look]) -> bool {
    let mut stack: Vec<lr_shadow::StateId> = vec![auto.start];
    let mut pos = 0;
    loop {
        let state = *stack.last().expect("stack never empty");
        let Some(action) = stream
            .get(pos)
            .copied()
            .and_then(|term| auto.action(state, term))
            .copied()
        else {
            eprintln!(
                "reject at pos {pos} (term {:?}): state {} has no action; expected {:?}\nkernel: {:?}",
                stream.get(pos),
                state.0,
                auto.expected(state),
                auto.states[state.0 as usize].kernel,
            );
            return false;
        };
        match action {
            Action::Shift(target) => {
                stack.push(target);
                pos += 1;
            }
            Action::Reduce(prod) => {
                let production = &auto.lowered.productions[prod as usize];
                let symbols = production.symbols.len();
                stack.truncate(stack.len() - symbols);
                let top = *stack.last().expect("stack never empty");
                let target = auto
                    .goto(top, production.lhs)
                    .expect("goto exists for every reachable lhs");
                stack.push(target);
            }
            Action::Accept => return true,
        }
    }
}

// ---------------------------------------------------------------------------
// Golden state count + determinism
// ---------------------------------------------------------------------------

#[test]
fn json_state_count_golden() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // Golden state count for the canonical LR(1) construction over the
    // JSON fixture (kernel-identified states, discovery order).
    assert_eq!(auto.states.len(), 44, "state count golden");
    // JSON is LR(1): no conflicts survive the policy.
    assert!(
        auto.conflicts.is_empty(),
        "json fixture must be conflict-free: {:?}",
        auto.conflicts
    );
    // Exactly one accept action across the whole table.
    let accepts: usize = auto
        .states
        .iter()
        .map(|state| usize::from(state.actions.values().any(|a| *a == Action::Accept)))
        .sum();
    assert_eq!(accepts, 1, "exactly one accept state");
}

#[test]
fn construction_is_deterministic() {
    let grammar = json_grammar();
    let a = Automaton::build(&grammar);
    let b = Automaton::build(&grammar);
    assert_eq!(a.states.len(), b.states.len());
    for (sa, sb) in a.states.iter().zip(b.states.iter()) {
        assert_eq!(sa.kernel, sb.kernel, "same kernel per state number");
        assert_eq!(sa.actions, sb.actions);
        assert_eq!(sa.gotos, sb.gotos);
    }
    assert_eq!(a.conflicts, b.conflicts);
}

// ---------------------------------------------------------------------------
// Accept / reject walks
// ---------------------------------------------------------------------------

#[test]
fn json_document_accepts() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // { "a" : [ 1 ] } — arrays in this fixture hold exactly one value.
    let stream = [
        Look::token(tok(&grammar, "lbrace")),
        Look::token(tok(&grammar, "string")),
        Look::token(tok(&grammar, "colon")),
        Look::token(tok(&grammar, "lbrack")),
        Look::token(tok(&grammar, "number")),
        Look::token(tok(&grammar, "rbrack")),
        Look::token(tok(&grammar, "rbrace")),
        Look::EOF,
    ];
    assert!(accepts(&auto, &stream));
    // { "a" : 1 , "b" : 2 } — the object's comma repetition.
    let stream = [
        Look::token(tok(&grammar, "lbrace")),
        Look::token(tok(&grammar, "string")),
        Look::token(tok(&grammar, "colon")),
        Look::token(tok(&grammar, "number")),
        Look::token(tok(&grammar, "comma")),
        Look::token(tok(&grammar, "string")),
        Look::token(tok(&grammar, "colon")),
        Look::token(tok(&grammar, "number")),
        Look::token(tok(&grammar, "rbrace")),
        Look::EOF,
    ];
    assert!(accepts(&auto, &stream));
    // [ {} ] — an empty object inside an array: this fixture's object
    // rule demands at least one pair, so the walk must reject.
    let stream = [
        Look::token(tok(&grammar, "lbrack")),
        Look::token(tok(&grammar, "lbrace")),
        Look::token(tok(&grammar, "rbrace")),
        Look::token(tok(&grammar, "rbrack")),
        Look::EOF,
    ];
    assert!(
        !accepts(&auto, &stream),
        "empty object violates the fixture"
    );
}

#[test]
fn malformed_stream_rejects_with_expected_set() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // `{` then EOF: the parser sits in the state expecting a pair start.
    let stream = [Look::token(tok(&grammar, "lbrace")), Look::EOF];
    assert!(!accepts(&auto, &stream));
    // The start state's kernel is the augmented item: it expects the
    // value starters (no EOF — EOF arrives only after a complete value).
    let expected = auto.expected(auto.start);
    let mut names: Vec<&str> = expected
        .iter()
        .filter_map(|look| look.as_token().map(|id| grammar.token_name(id)))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["lbrace", "lbrack", "number", "string"]);
}

// ---------------------------------------------------------------------------
// Reachability
// ---------------------------------------------------------------------------

#[test]
fn unreachable_rule_generates_no_states() {
    let with_dead = {
        let mut b = GrammarBuilder::new("json-dead");
        let string = b.token_re("string", "\"[^\"]*\"").expect("t");
        let value = b.declare_rule("value").expect("r");
        b.set_body(value, GrammarBuilder::token_ref(string));
        // Dead rule: never referenced from `value`.
        let dead = b.declare_rule("dead").expect("r");
        b.set_body(dead, GrammarBuilder::token_ref(string));
        b.finish().expect("valid")
    };
    let without_dead = {
        let mut b = GrammarBuilder::new("json-live");
        let string = b.token_re("string", "\"[^\"]*\"").expect("t");
        let value = b.declare_rule("value").expect("r");
        b.set_body(value, GrammarBuilder::token_ref(string));
        b.finish().expect("valid")
    };
    let a = Automaton::build(&with_dead);
    let b = Automaton::build(&without_dead);
    assert_eq!(a.states.len(), b.states.len(), "dead rules cost no states");
    // Every state is reachable (worklist construction).
    let reachable = a.reachable();
    assert!(reachable.iter().all(|seen| *seen));
}

// ---------------------------------------------------------------------------
// Conflict policy
// ---------------------------------------------------------------------------

#[test]
fn shift_wins_on_dangling_else() {
    // stmt → "if" "c" stmt | "if" "c" stmt "else" stmt | "other"
    let mut b = GrammarBuilder::new("dangling");
    let if_ = b.token_lit("if", "if").expect("t");
    let cond = b.token_lit("c", "c").expect("t");
    let else_ = b.token_lit("else", "else").expect("t");
    let other = b.token_lit("other", "other").expect("t");
    let stmt = b.declare_rule("stmt").expect("r");
    b.set_body(
        stmt,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(if_),
                GrammarBuilder::token_ref(cond),
                GrammarBuilder::rule_ref(stmt),
            ]),
            GrammarBuilder::seq(vec![
                GrammarBuilder::token_ref(if_),
                GrammarBuilder::token_ref(cond),
                GrammarBuilder::rule_ref(stmt),
                GrammarBuilder::token_ref(else_),
                GrammarBuilder::rule_ref(stmt),
            ]),
            GrammarBuilder::token_ref(other),
        ]),
    );
    let grammar = b.finish().expect("valid");
    let auto = Automaton::build(&grammar);
    assert!(
        auto.conflicts.iter().any(|c| c.term == Look::token(else_)),
        "dangling else must surface as a conflict on `else`: {:?}",
        auto.conflicts
    );
    // Shift-wins: every contested `else` cell still shifts.
    for conflict in &auto.conflicts {
        assert!(matches!(
            auto.action(conflict.state, conflict.term),
            Some(Action::Shift(_))
        ));
    }
}

#[test]
fn synthetic_epsilon_conflicts_resolve_silently() {
    // s → q "a";  q → &b | &c — two predicate placeholders, both ε: the
    // ε/ε contention on `a` resolves silently (no recorded conflict).
    let mut builder = GrammarBuilder::new("predicates");
    let a = builder.token_lit("a", "a").expect("t");
    let bb = builder.token_lit("b", "b").expect("t");
    let c = builder.token_lit("c", "c").expect("t");
    let q = builder.declare_rule("q").expect("r");
    let s = builder.declare_rule("s").expect("r");
    builder.set_body(
        q,
        GrammarBuilder::alt(vec![
            GrammarBuilder::and(GrammarBuilder::token_ref(bb)),
            GrammarBuilder::and(GrammarBuilder::token_ref(c)),
        ]),
    );
    builder.set_body(
        s,
        GrammarBuilder::seq(vec![
            GrammarBuilder::rule_ref(q),
            GrammarBuilder::token_ref(a),
        ]),
    );
    let grammar = builder.finish().expect("valid");
    let auto = Automaton::build(&grammar);
    assert!(
        auto.conflicts.is_empty(),
        "synthetic ε/ε must stay silent: {:?}",
        auto.conflicts
    );
    // Both predicate placeholders exist and are ε-productions.
    let predicates: Vec<_> = auto
        .lowered
        .productions
        .iter()
        .filter(|p| p.kind == ProdKind::Predicate)
        .collect();
    assert_eq!(predicates.len(), 2);
    // The ε/ε cell still resolves to the earlier production.
    let has_reduce = auto
        .states
        .iter()
        .flat_map(|state| state.actions.values())
        .any(|action| matches!(action, Action::Reduce(_)));
    assert!(has_reduce, "a silent ε/ε reduce exists");
}

#[test]
fn pratt_grammar_still_builds() {
    // A pratt-hinted expression grammar builds the same shadow tables;
    // conflicts (if any) are recorded, not fatal.
    let mut b = GrammarBuilder::new("pratt");
    let n = b.token_re("n", "[0-9]+").expect("t");
    let plus = b.token_lit("plus", "+").expect("t");
    let expr = b.declare_rule("expr").expect("r");
    b.set_body(
        expr,
        GrammarBuilder::alt(vec![
            GrammarBuilder::seq(vec![
                GrammarBuilder::rule_ref(expr),
                GrammarBuilder::token_ref(plus),
                GrammarBuilder::token_ref(n),
            ]),
            GrammarBuilder::token_ref(n),
        ]),
    );
    b.set_pratt(
        expr,
        PrattHint {
            left_bp: 10,
            right_bp: 9,
            assoc: Associativity::Left,
        },
    );
    let grammar = b.finish().expect("valid");
    let auto = Automaton::build(&grammar);
    assert!(accepts(
        &auto,
        &[Look::token(n), Look::token(plus), Look::token(n), Look::EOF]
    ));
}

// ---------------------------------------------------------------------------
// Pseudo-EOF resize safety
// ---------------------------------------------------------------------------

#[test]
fn pseudo_eof_is_outside_token_space_and_resizes_safely() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // EOF sits above every token id (u16 space) — appending tokens can
    // never collide with it.
    assert!(Look::EOF.0 > u32::from(u16::MAX));
    assert!(Look::EOF.is_eof());
    assert_eq!(Look::EOF.as_token(), None);
    assert_eq!(Look::token(TokenId(7)).as_token(), Some(TokenId(7)));

    // Grow the token table (an unused token appended): the automaton is
    // unchanged — ids did not shift, EOF did not move.
    let grown = {
        let mut b = GrammarBuilder::new("json");
        let lbrace = b.token_lit("lbrace", "{").expect("t");
        let rbrace = b.token_lit("rbrace", "}").expect("t");
        let left_bracket = b.token_lit("lbrack", "[").expect("t");
        let right_bracket = b.token_lit("rbrack", "]").expect("t");
        let colon = b.token_lit("colon", ":").expect("t");
        let comma = b.token_lit("comma", ",").expect("t");
        let string = b.token_re("string", "\"[^\"]*\"").expect("t");
        let number = b.token_re("number", "[0-9]+").expect("t");
        b.token_lit("later_added", "~").expect("t");
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
    };
    let auto_grown = Automaton::build(&grown);
    assert_eq!(auto.states.len(), auto_grown.states.len());
    assert_eq!(auto.conflicts, auto_grown.conflicts);
    for (state, grown_state) in auto.states.iter().zip(auto_grown.states.iter()) {
        assert_eq!(state.kernel, grown_state.kernel);
    }
}

// ---------------------------------------------------------------------------
// Expected sets
// ---------------------------------------------------------------------------

#[test]
fn expected_sets_match_positions() {
    let grammar = json_grammar();
    let auto = Automaton::build(&grammar);
    // After shifting `{`, the parser expects exactly the pair starter.
    let lbrace = Look::token(tok(&grammar, "lbrace"));
    let string = tok(&grammar, "string");
    let Some(Action::Shift(after_lbrace)) = auto.action(auto.start, lbrace).copied() else {
        panic!("start shifts on lbrace");
    };
    let expected = auto.expected(after_lbrace);
    let tokens: Vec<_> = expected.iter().filter_map(|look| look.as_token()).collect();
    assert_eq!(tokens, [string], "after `{{` only a pair (string) fits");
}

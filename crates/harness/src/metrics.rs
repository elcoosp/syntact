//! Repair-quality metrics (issue 026): mutate valid documents, drive the
//! shadow automaton to the failure point, ask the recovery planner for a
//! repair, and measure — repair rate, op-distance, determinism re-run.
//! The calibration gate (≥ 95 % repair rate, 100 % determinism) is
//! asserted as a test with the published numbers in its output.

use gir_ir::{Grammar, GrammarBuilder, TokenId};
use lr_shadow::{Action, Automaton, Look, StateId};
use recovery::{Planner, RepairPlan};

/// The lookahead window handed to the planner (tokens past the failure).
pub const WINDOW: usize = 16;

/// Aggregate calibration statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MutationStats {
    /// Mutations generated.
    pub mutations: usize,
    /// Mutations that still parsed cleanly (excluded from the rate).
    pub no_error: usize,
    /// Broken mutations where the planner produced a plan.
    pub planned: usize,
    /// Broken mutations where the planner reported none (budget/none).
    pub unplanned: usize,
    /// Planned mutations whose re-run produced the identical plan.
    pub deterministic: usize,
    /// Broken mutations repaired with at most two operations (the
    /// hand-computed bound for single-token mutations).
    pub ops_within_two: usize,
    /// Total operations across plans.
    pub total_ops: usize,
}

impl MutationStats {
    /// The repair rate over broken mutations (planned / broken).
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // metric counts stay far below 2^52
    pub fn repair_rate(&self) -> f64 {
        let broken = self.planned + self.unplanned;
        if broken == 0 {
            1.0
        } else {
            self.planned as f64 / broken as f64
        }
    }

    /// The determinism rate over planned mutations.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // metric counts stay far below 2^52
    pub fn determinism_rate(&self) -> f64 {
        if self.planned == 0 {
            1.0
        } else {
            self.deterministic as f64 / self.planned as f64
        }
    }

    /// Mean operations per plan.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // metric counts stay far below 2^52
    pub fn mean_ops(&self) -> f64 {
        if self.planned == 0 {
            0.0
        } else {
            self.total_ops as f64 / self.planned as f64
        }
    }
}

/// The JSON fixture grammar (same shape as the lr-shadow tests) — the
/// calibration corpus's language.
#[must_use]
pub fn json_fixture() -> Grammar {
    let mut b = GrammarBuilder::new("json-calibration");
    let lbrace = b.token_lit("lbrace", "{").expect("t");
    let rbrace = b.token_lit("rbrace", "}").expect("t");
    let left_bracket = b.token_lit("lbrack", "[").expect("t");
    let right_bracket = b.token_lit("rbrack", "]").expect("t");
    let colon = b.token_lit("colon", ":").expect("t");
    let comma = b.token_lit("comma", ",").expect("t");
    let string = b.token_re("string", "\"[^\"]*\"").expect("t");
    let number = b.token_re("number", "[0-9]+").expect("t");
    let kw_true = b.token_lit("true", "true").expect("t");
    let kw_false = b.token_lit("false", "false").expect("t");
    let kw_null = b.token_lit("null", "null").expect("t");

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
            GrammarBuilder::token_ref(kw_true),
            GrammarBuilder::token_ref(kw_false),
            GrammarBuilder::token_ref(kw_null),
        ]),
    );
    b.set_body(
        object,
        GrammarBuilder::seq(vec![
            GrammarBuilder::token_ref(lbrace),
            GrammarBuilder::opt(GrammarBuilder::rule_ref(pair)),
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

/// One lexed fixture token: the `Look` plus the source range (for
/// splicing mutations).
#[derive(Debug, Clone)]
pub struct FixtureToken {
    /// The shadow-automaton lookahead.
    pub look: Look,
    /// Source byte range.
    pub range: std::ops::Range<usize>,
}

/// Lex a JSON-ish source into fixture tokens (hand lexer — the pack's
/// scanner takes over once lexgen wiring lands in the packs).
#[must_use]
pub fn lex_fixture(grammar: &Grammar, source: &str) -> Vec<FixtureToken> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0_usize;
    let look = |name: &str| Look::token(tok(grammar, name));
    while pos < bytes.len() {
        let start = pos;
        let name = match bytes[pos] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                pos += 1;
                continue;
            }
            b'{' => {
                pos += 1;
                "lbrace"
            }
            b'}' => {
                pos += 1;
                "rbrace"
            }
            b'[' => {
                pos += 1;
                "lbrack"
            }
            b']' => {
                pos += 1;
                "rbrack"
            }
            b':' => {
                pos += 1;
                "colon"
            }
            b',' => {
                pos += 1;
                "comma"
            }
            b'"' => {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                }
                pos = (pos + 1).min(bytes.len());
                "string"
            }
            b'0'..=b'9' => {
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                "number"
            }
            _ => {
                for (text, name) in [("true", "true"), ("false", "false"), ("null", "null")] {
                    if source[pos..].starts_with(text) {
                        pos += text.len();
                        out.push(FixtureToken {
                            look: look(name),
                            range: start..pos,
                        });
                        break;
                    }
                }
                pos += 1; // garbage char: skip (not a fixture token)
                continue;
            }
        };
        out.push(FixtureToken {
            look: look(name),
            range: start..pos,
        });
    }
    out
}

/// The drive outcome: `(stack, lookahead window)` either way — `Ok`
/// when accepted, `Err` at the failure point.
pub type DriveOutcome = Result<(Vec<StateId>, Vec<Look>), (Vec<StateId>, Vec<Look>)>;

/// Drive the automaton over the token stream; on failure return the
/// stack at the error point plus the remaining lookahead window.
/// `Ok` when the stream is accepted.
pub fn drive_to_failure(auto: &Automaton, tokens: &[FixtureToken]) -> DriveOutcome {
    let mut stack = vec![auto.start];
    for (idx, token) in tokens.iter().enumerate() {
        let la = token.look;
        loop {
            let state = *stack.last().expect("stack never empty");
            match auto.action(state, la).copied() {
                Some(Action::Shift(target)) => {
                    stack.push(target);
                    break;
                }
                Some(Action::Reduce(prod)) => {
                    let production = &auto.lowered.productions[prod as usize];
                    stack.truncate(stack.len() - production.symbols.len());
                    let base = *stack.last().expect("stack never empty");
                    let target = auto
                        .goto(base, production.lhs)
                        .expect("goto exists for reachable lhs");
                    stack.push(target);
                }
                Some(Action::Accept) => {
                    return Ok((stack, Vec::new()));
                }
                None => {
                    let window: Vec<Look> = tokens[idx..(idx + WINDOW).min(tokens.len())]
                        .iter()
                        .map(|t| t.look)
                        .collect();
                    return Err((stack, window));
                }
            }
        }
    }
    // All tokens consumed: run the EOF lookahead to completion — the
    // automaton may still owe reductions (e.g. closing the start rule)
    // before it accepts, and a plain `Accept`-only check misjudged
    // clean streams as failures.
    loop {
        let state = *stack.last().expect("stack never empty");
        match auto.action(state, Look::EOF).copied() {
            Some(Action::Reduce(prod)) => {
                let production = &auto.lowered.productions[prod as usize];
                stack.truncate(stack.len() - production.symbols.len());
                let base = *stack.last().expect("stack never empty");
                let target = auto
                    .goto(base, production.lhs)
                    .expect("goto exists for reachable lhs");
                stack.push(target);
            }
            Some(Action::Accept) => return Ok((stack, Vec::new())),
            // EOF never shifts; no action means a genuine failure.
            Some(Action::Shift(_)) | None => return Err((stack, Vec::new())),
        }
    }
}

/// Calibrate over `sources`: mutate (delete / replace) every token,
/// plan a repair for every broken mutation, aggregate the metrics.
#[must_use]
pub fn calibrate(sources: &[&str], budget: u32) -> MutationStats {
    let grammar = json_fixture();
    let auto = Automaton::build(&grammar);
    let planner = Planner::new(&auto);
    let mut stats = MutationStats::default();

    for source in sources {
        let tokens = lex_fixture(&grammar, source);
        // Mutation: delete token i, or replace it with a fixed wrong
        // token (the `,` punctuator — deterministic, seed-free).
        for i in 0..tokens.len() {
            for mutation in [Mutation::Delete, Mutation::ReplaceWithComma] {
                let mutated = mutate(source, &tokens, i, mutation);
                let Some(mutated) = mutated else {
                    continue;
                };
                stats.mutations += 1;
                let mtokens = lex_fixture(&grammar, &mutated);
                let Err((stack, window)) = drive_to_failure(&auto, &mtokens) else {
                    stats.no_error += 1;
                    continue;
                };
                let Some(plan) = planner.plan(&stack, &window, budget) else {
                    stats.unplanned += 1;
                    continue;
                };
                stats.planned += 1;
                stats.total_ops += plan.ops.len();
                if plan.ops.len() <= 2 {
                    stats.ops_within_two += 1;
                }
                // Determinism re-run: identical plan, byte for byte.
                if planner.plan(&stack, &window, budget) == Some(plan.clone())
                    && Some(&plan) == planner.plan(&stack, &window, budget).as_ref()
                {
                    stats.deterministic += 1;
                }
            }
        }
    }
    stats
}

/// One single-token mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// Remove token `i`'s bytes.
    Delete,
    /// Replace token `i`'s bytes with `,` (a plausible-but-wrong token).
    ReplaceWithComma,
}

fn mutate(source: &str, tokens: &[FixtureToken], i: usize, mutation: Mutation) -> Option<String> {
    let token = &tokens[i];
    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..token.range.start]);
    match mutation {
        Mutation::Delete => {}
        Mutation::ReplaceWithComma => {
            if &source[token.range.clone()] == "," {
                return None; // no-op replacement
            }
            out.push(',');
        }
    }
    out.push_str(&source[token.range.end..]);
    Some(out)
}

/// A hand-computed calibration fixture: the source, the expected repair
/// ops (as fixture token names), and the expected cost.
pub struct ExpectedRepair {
    /// The mutated source.
    pub source: &'static str,
    /// Expected ops as `(kind, token name)` pairs.
    pub ops: &'static [(&'static str, &'static str)],
}

/// Verify one hand-computed fixture against the planner.
#[must_use]
pub fn check_expected(auto: &Automaton, expected: &ExpectedRepair) -> Option<RepairPlan> {
    let grammar = json_fixture();
    let tokens = lex_fixture(&grammar, expected.source);
    let Err((stack, window)) = drive_to_failure(auto, &tokens) else {
        return None;
    };
    let planner = Planner::new(auto);
    planner.plan(&stack, &window, 512)
}

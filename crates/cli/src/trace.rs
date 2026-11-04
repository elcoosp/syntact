//! `syntact trace` (issue 029): per-region recovery events over a
//! document — the parse is driven by the LR shadow automaton; at every
//! failure the cactus-stack planner proposes a repair, which is applied
//! virtually and the drive resumes. Output is a pure function of the
//! input (byte-identical across runs), in human or `--json` form.
//!
//! Exit codes: [`OK`](crate::OK) when traced (any number of regions),
//! [`FAIL`](crate::FAIL) when a region has no repair within the budget,
//! [`USAGE`](crate::USAGE) on unreadable input or an unsupported pack.

use std::fmt::Write as _;

use gir_ir::Grammar;
use lr_shadow::{Action, Automaton, Look, StateId};
use recovery::{Planner, RepairOp};

/// Safety cap on the lookahead window handed to the planner (the goal
/// semantics need the document tail inside the window; the planner's
/// budget still bounds the work).
const WINDOW: usize = 256;
/// Work budget per region (deterministic expansions, never time).
const BUDGET: u32 = 4096;
/// Region cap — guarantees termination on pathological documents.
const MAX_REGIONS: usize = 64;

/// One recovery region: where the parse broke, the repair, and where it
/// resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Byte offset of the offending token (or EOF).
    pub offset: u32,
    /// The repair operations in canonical order.
    pub ops: Vec<RepairOp>,
    /// Byte offset where the parse resumed (after the repair).
    pub resync: u32,
}

/// The full trace of one document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trace {
    /// Regions in encounter order (deterministic).
    pub regions: Vec<Region>,
    /// The number of bytes scanned.
    pub bytes: u32,
    /// Whether every region found a repair.
    pub complete: bool,
}

/// Trace `source` through the JSON fixture pipeline.
#[must_use]
pub fn trace_json(source: &str) -> Trace {
    let grammar = harness::metrics::json_fixture();
    let auto = Automaton::build(&grammar);
    trace_with(&grammar, &auto, source)
}

/// The worker: drive / fail / plan / apply / resume loop.
fn trace_with(grammar: &Grammar, auto: &Automaton, source: &str) -> Trace {
    let planner = Planner::new(auto);
    let tokens = harness::metrics::lex_fixture(grammar, source);
    trace_loop(&planner, auto, grammar, &tokens, source)
}

/// The drive / fail / plan / apply / resume loop (split out to keep the
/// outer function small).
fn trace_loop(
    planner: &Planner<'_>,
    auto: &Automaton,
    grammar: &Grammar,
    tokens: &[harness::metrics::FixtureToken],
    source: &str,
) -> Trace {
    let _ = grammar;
    let bytes = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let mut trace = Trace {
        bytes,
        ..Trace::default()
    };
    let mut stack: Vec<StateId> = vec![auto.start];
    let mut idx = 0_usize;

    loop {
        if trace.regions.len() >= MAX_REGIONS {
            trace.complete = false;
            return trace;
        }
        // Drive from the current position.
        let mut failure: Option<(usize, Vec<Look>)> = None;
        'drive: for (i, token) in tokens[idx..].iter().enumerate() {
            let i = idx + i;
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
                        // The document parsed to accept: stop early.
                        trace.complete = true;
                        return trace;
                    }
                    None => {
                        let window: Vec<Look> = tokens[i..(i + WINDOW).min(tokens.len())]
                            .iter()
                            .map(|t| t.look)
                            .collect();
                        failure = Some((i, window));
                        break 'drive;
                    }
                }
            }
        }
        let Some((error_idx, window)) = failure else {
            // All tokens consumed. EOF decides (reduce chain to accept),
            // and an unaccepted tail is itself a region: the planner
            // closes the document with inserts.
            if eof_accepts(auto, &stack) {
                trace.complete = true;
                return trace;
            }
            if let Some(plan) = planner.plan(&stack, &[], BUDGET) {
                trace.regions.push(Region {
                    offset: bytes,
                    ops: plan.ops,
                    resync: bytes,
                });
                trace.complete = true;
            } else {
                trace.complete = false;
            }
            return trace;
        };
        // Region: plan the repair, apply it, record it.
        let Some(region) =
            plan_region(planner, auto, &mut stack, error_idx, &window, tokens, bytes)
        else {
            trace.complete = false;
            let offset = byte_at(tokens, error_idx, bytes);
            trace.regions.push(Region {
                offset,
                ops: Vec::new(),
                resync: offset,
            });
            return trace;
        };
        idx = region.resync_token;
        trace.regions.push(Region {
            offset: region.offset,
            ops: region.ops,
            resync: region.resync,
        });
        let resume_idx = idx;
        if idx >= tokens.len() {
            // Window consumed by deletes: EOF decides on the repaired stack.
            trace.complete = eof_accepts(auto, &stack);
            return trace;
        }
        // Progress guard: the resume point must not regress.
        if resume_idx <= error_idx.saturating_sub(1) {
            trace.complete = false;
            return trace;
        }
    }
}

/// Whether the stack reduces to accept at the pseudo-EOF (bounded chain —
/// the action on EOF is a reduce until the augmented accept fires).
fn eof_accepts(auto: &Automaton, stack: &[StateId]) -> bool {
    let mut stack = stack.to_vec();
    let mut steps = 0_usize;
    loop {
        let Some(top) = stack.last().copied() else {
            return false;
        };
        match auto.action(top, Look::EOF).copied() {
            Some(Action::Accept) => return true,
            Some(Action::Reduce(prod)) => {
                let production = &auto.lowered.productions[prod as usize];
                if stack.len() < production.symbols.len() + 1 {
                    return false;
                }
                stack.truncate(stack.len() - production.symbols.len());
                let base = *stack.last().expect("stack never empty");
                let Some(target) = auto.goto(base, production.lhs) else {
                    return false;
                };
                stack.push(target);
            }
            _ => return false,
        }
        steps += 1;
        if steps > stack.len() * 2 + 64 {
            return false;
        }
    }
}

/// The byte offset of token `idx`, or `fallback` at/after EOF.
fn byte_at(tokens: &[harness::metrics::FixtureToken], idx: usize, fallback: u32) -> u32 {
    tokens.get(idx).map_or(fallback, |t| {
        u32::try_from(t.range.start).unwrap_or(fallback)
    })
}

/// One planned region: the recorded offsets plus where the drive resumes
/// (the token index rides along for the loop).
struct PlannedRegion {
    offset: u32,
    ops: Vec<RepairOp>,
    resync: u32,
    resync_token: usize,
}

/// Plan the repair at `error_idx` and apply it to `stack` (inserts
/// reshape the stack, deletes skip input tokens).
fn plan_region(
    planner: &Planner<'_>,
    auto: &Automaton,
    stack: &mut Vec<StateId>,
    error_idx: usize,
    window: &[Look],
    tokens: &[harness::metrics::FixtureToken],
    bytes: u32,
) -> Option<PlannedRegion> {
    let plan = planner.plan(stack, window, BUDGET)?;
    let mut deletes = 0_usize;
    for op in &plan.ops {
        match *op {
            RepairOp::Insert(look) => advance_stack(auto, stack, look),
            RepairOp::Delete(_) => deletes += 1,
        }
    }
    let resume_idx = error_idx + deletes;
    Some(PlannedRegion {
        offset: byte_at(tokens, error_idx, bytes),
        ops: plan.ops,
        resync: byte_at(tokens, resume_idx, bytes),
        resync_token: resume_idx,
    })
}

/// Simulate shifting `look` (reduce chain + shift) — the same semantics
/// the planner uses internally.
fn advance_stack(auto: &Automaton, stack: &mut Vec<StateId>, look: Look) {
    loop {
        let state = *stack.last().expect("stack never empty");
        match auto.action(state, look).copied() {
            Some(Action::Shift(target)) => {
                stack.push(target);
                return;
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
            _ => return, // cannot shift: leave the stack (region already recorded)
        }
    }
}

/// The token name for a `Look` (fixpoint: `tok42` when out of table).
fn look_name(grammar: &Grammar, look: Look) -> String {
    look.as_token()
        .and_then(|id| grammar.tokens.get(usize::from(id.0)))
        .map_or_else(
            || format!("tok{}", look.0),
            |token| grammar.interner.lookup(token.name).to_owned(),
        )
}

/// Render the trace in the human-readable form (one line per region).
#[must_use]
pub fn render_text(grammar: &Grammar, trace: &Trace) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "trace: {} byte(s), {} region(s)",
        trace.bytes,
        trace.regions.len()
    );
    for (idx, region) in trace.regions.iter().enumerate() {
        let ops: Vec<String> = region
            .ops
            .iter()
            .map(|op| match *op {
                RepairOp::Insert(look) => format!("insert {}", look_name(grammar, look)),
                RepairOp::Delete(look) => format!("delete {}", look_name(grammar, look)),
            })
            .collect();
        let _ = writeln!(
            out,
            "region {} @ {}: repair [{}] → resync @ {}",
            idx + 1,
            region.offset,
            if ops.is_empty() {
                "none".to_owned()
            } else {
                ops.join(", ")
            },
            region.resync
        );
    }
    let _ = writeln!(
        out,
        "verdict: {}",
        if trace.complete {
            "repaired"
        } else {
            "unrepaired"
        }
    );
    out
}

/// Render the trace as JSON for tooling (byte-identical across runs).
#[must_use]
pub fn render_json(grammar: &Grammar, trace: &Trace) -> String {
    let mut out = String::new();
    let _ = write!(out, "{{\"bytes\":{},\"regions\":[", trace.bytes);
    for (idx, region) in trace.regions.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        let _ = write!(out, "{{\"offset\":{},\"ops\":[", region.offset);
        for (oidx, op) in region.ops.iter().enumerate() {
            if oidx > 0 {
                out.push(',');
            }
            match *op {
                RepairOp::Insert(look) => {
                    let _ = write!(out, "[\"insert\",\"{}\"]", look_name(grammar, look));
                }
                RepairOp::Delete(look) => {
                    let _ = write!(out, "[\"delete\",\"{}\"]", look_name(grammar, look));
                }
            }
        }
        let _ = write!(out, "],\"resync\":{}}}", region.resync);
    }
    let _ = write!(
        out,
        "],\"complete\":{}}}",
        if trace.complete { "true" } else { "false" }
    );
    out
}

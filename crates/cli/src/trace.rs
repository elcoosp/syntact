//! `syntact trace` — per-region recovery events over a real parse
//! (issue 029).
//!
//! Two real engines cooperate, no stubs:
//!
//! - **Pack reference parsers** ([`packs_json::reference`] /
//!   [`packs_toml::reference`]): their [`Event::Error`] side-table
//!   entries give every recovery region — the byte offset of the
//!   offending token, the diagnostic message, and the resync point
//!   where the parser resumed (the sync token right after the error
//!   region).
//! - **Shadow automaton + CPCT+ planner** (JSON — the flagship recovery
//!   oracle from issue 026): the fixture grammar's LR(1) automaton is
//!   driven over the source in a single incremental pass; at every
//!   failure the planner proposes an `insert`/`delete` repair script,
//!   whose ops are applied lazily — interleaved with the free shifts of
//!   the surviving input — and the drive continues. That yields
//!   per-region repair ops plus the exact resync offset (the first real
//!   token shifted once the repair is spent), and enriches the region
//!   kind with the pack's diagnostic when both engines mark the same
//!   offset. Pack diagnostics at or after the first repair region are
//!   attributed to it (the repair's continuation covers the cascade);
//!   diagnostics before any repair region surface as native
//!   skip-to-resync regions.
//!
//! [`trace`] is a pure function from `(pack, source)` to a
//! [`TraceOutput`]; rendering and I/O live in [`render_json`] /
//! [`render_text`] and the command layer, so repeated runs are
//! byte-for-byte deterministic.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::ops::Range;

use gir_ir::Grammar;
use harness::metrics::{self, FixtureToken, WINDOW};
use lr_shadow::{Action, Automaton, Look};
use packs_json::reference as json_ref;
use packs_toml::reference as toml_ref;
use recovery::{Planner, RepairOp};
use runtime_core::events::Event;

/// The planner work budget. Calibration (issue 026) showed 512 exhausts
/// the uniform-cost search on multi-error inputs; 4096 plans 100 % of
/// the gate corpus.
const BUDGET: u32 = 4096;

/// One planned repair operation, printable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceOp {
    /// `"insert"` or `"delete"`.
    pub op: &'static str,
    /// The fixture token name the op applies to (`"colon"`, `"rbrace"`,
    /// `"EOF"`).
    pub token: String,
}

/// One recovery region: where recovery kicked in, why, what repaired
/// it, and where parsing resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceRegion {
    /// Byte offset of the offending token (source length at EOF).
    pub offset: usize,
    /// Diagnostic kind: the pack's message when both engines mark the
    /// same offset, else `unexpected <token>`.
    pub kind: String,
    /// Planned repair ops (empty when only a resync happened).
    pub ops: Vec<TraceOp>,
    /// Byte offset where parsing resumed after the repair: the first
    /// real token shifted once the ops were spent (source length when
    /// the parse ended first).
    pub resync: usize,
}

/// The full trace of one document through one pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceOutput {
    /// The pack that ran.
    pub pack: String,
    /// Source length in bytes.
    pub source_len: usize,
    /// True when no recovery region was hit.
    pub ok: bool,
    /// Regions in ascending offset order (deterministic).
    pub regions: Vec<TraceRegion>,
}

/// Trace one document through `pack` (`"json"` or `"toml"`); pure.
///
/// # Errors
/// Unknown pack name.
#[must_use = "the trace output is the whole point"]
pub fn trace(pack: &str, source: &str) -> Result<TraceOutput, String> {
    match pack {
        "json" => Ok(trace_json(source)),
        "toml" => Ok(trace_toml(source)),
        other => Err(format!("unknown pack `{other}` (available: json, toml)")),
    }
}

/// One recovery region reported by a pack's reference parser.
struct PackDiag {
    /// Byte offset of the offending token (source length at EOF).
    offset: usize,
    /// Side-table diagnostic index.
    diag: u32,
    /// Byte offset where parsing resumed (the sync token after the
    /// error region; source length when the failure was at EOF).
    resume: usize,
    /// End of the diag's own error-token range: the resume fill only
    /// fires on tokens starting at or after this (the error token
    /// itself must not count as its own resync).
    after: u32,
}

/// The JSON trace: pack diagnostics + the shadow-automaton repair loop.
fn trace_json(source: &str) -> TraceOutput {
    let source_len = source.len();
    let (events, ranges, _errors) = json_ref::parse_events(source);
    let diags = pack_diags(&events, &ranges, source_len);

    let (mut regions, matched) = automaton_regions(source, source_len, &diags);
    // Pack diagnostics no repair region explains (lexer gaps, garbage
    // the fixture lexer skips, anything before the first repair) still
    // get reported — as native skip-to-resync regions with no ops.
    for (pos, diag) in diags.iter().enumerate() {
        if matched.contains(&pos) {
            continue;
        }
        let explained = regions.iter().any(|region| region.offset <= diag.offset);
        if !explained {
            regions.push(TraceRegion {
                offset: diag.offset,
                kind: json_ref::error_message(diag.diag).to_owned(),
                ops: Vec::new(),
                resync: diag.resume,
            });
        }
    }
    regions.sort_by(|a, b| a.offset.cmp(&b.offset).then_with(|| a.kind.cmp(&b.kind)));
    TraceOutput {
        pack: "json".to_owned(),
        source_len,
        ok: regions.is_empty(),
        regions,
    }
}

/// The TOML trace: the reference parser's native recovery regions (no
/// automaton fixture yet — ops stay empty, resync is the parser's own
/// skip-to-sync resume point).
fn trace_toml(source: &str) -> TraceOutput {
    let source_len = source.len();
    let (events, ranges, _errors) = toml_ref::parse_events(source);
    let mut regions: Vec<TraceRegion> = pack_diags(&events, &ranges, source_len)
        .into_iter()
        .map(|diag| TraceRegion {
            offset: diag.offset,
            kind: toml_ref::error_message(diag.diag).to_owned(),
            ops: Vec::new(),
            resync: diag.resume,
        })
        .collect();
    regions.sort_by(|a, b| a.offset.cmp(&b.offset).then_with(|| a.kind.cmp(&b.kind)));
    TraceOutput {
        pack: "toml".to_owned(),
        source_len,
        ok: regions.is_empty(),
        regions,
    }
}

/// Walk a pack event stream and collect every [`Event::Error`] with the
/// offending token that follows it (the side-table `ranges` are
/// parallel to the token events in DFS order). Consecutive errors each
/// attach to their own following token (nothing is dropped); a diag's
/// resume is the first token starting at or after its error token's
/// end. Errors at EOF carry no offending token: offset and resume are
/// the source length.
fn pack_diags(events: &[Event], ranges: &[(u16, Range<u32>)], source_len: usize) -> Vec<PackDiag> {
    let mut diags: Vec<PackDiag> = Vec::new();
    let mut token_idx = 0_usize;
    let mut pending: Vec<u32> = Vec::new();
    // Diag indices still awaiting their resume fill.
    let mut awaiting: Vec<usize> = Vec::new();
    for event in events {
        match *event {
            Event::Error { diag } => pending.push(diag),
            Event::Token { .. } => {
                let Some((_, range)) = ranges.get(token_idx) else {
                    break; // malformed side table: stop collecting
                };
                let start = range.start as usize;
                token_idx += 1;
                for diag in pending.drain(..) {
                    diags.push(PackDiag {
                        offset: start,
                        diag,
                        resume: source_len,
                        after: range.end,
                    });
                    awaiting.push(diags.len() - 1);
                }
                // The first token that starts at or after an error
                // token's end is where parsing resumed; the error
                // token itself (same start) keeps waiting.
                awaiting.retain(|&i| {
                    if start == diags[i].offset {
                        return true;
                    }
                    if start >= diags[i].after as usize {
                        diags[i].resume = start;
                        return false;
                    }
                    true
                });
            }
            Event::Start { .. } | Event::Finish => {}
        }
    }
    // Pending errors with no offending token: the failure was at EOF.
    for diag in pending {
        diags.push(PackDiag {
            offset: source_len,
            diag,
            resume: source_len,
            after: u32::try_from(source_len).unwrap_or(u32::MAX),
        });
    }
    diags
}

/// The JSON shadow-automaton repair loop, as a single incremental drive:
/// shift/reduce until stuck; when stuck, apply the next queued repair
/// op (inserts splice a zero-width synthetic token, deletes drop the
/// offending token); with the queue empty, ask the planner for a repair
/// script — that is one reported region — and lazily apply its ops
/// interleaved with the free shifts of the surviving input. Returns the
/// regions (in failure order) plus the pack-diag positions whose offset
/// enriched a region's kind.
///
/// Synthetic tokens are recognizable by their empty byte range (the
/// fixture lexer never emits one).
fn automaton_regions(
    source: &str,
    source_len: usize,
    diags: &[PackDiag],
) -> (Vec<TraceRegion>, BTreeSet<usize>) {
    let grammar = metrics::json_fixture();
    let auto = Automaton::build(&grammar);
    let planner = Planner::new(&auto);
    let names = token_names(&grammar);
    let mut tokens = metrics::lex_fixture(&grammar, source);
    let max_iterations = tokens.len() * 16 + 256;
    let mut iterations = 0_usize;

    let mut regions: Vec<TraceRegion> = Vec::new();
    let mut matched = BTreeSet::new();
    let mut stack = vec![auto.start];
    let mut idx = 0_usize;
    let mut queue: VecDeque<RepairOp> = VecDeque::new();
    // The region still awaiting its resync offset (filled by the first
    // real token shifted with an empty op queue).
    let mut pending_region: Option<usize> = None;
    // The index a repair script was last planned at; a second stuck at
    // the same index means the script did not line up — force instead
    // of planning the identical script forever.
    let mut planned_at: Option<usize> = None;

    loop {
        iterations += 1;
        if iterations > max_iterations {
            if let Some(ri) = pending_region {
                regions[ri].resync = source_len;
            }
            break;
        }
        let la = tokens.get(idx).map_or(Look::EOF, |token| token.look);
        let state = *stack.last().expect("stack never empty");
        match auto.action(state, la).copied() {
            Some(Action::Shift(target)) => {
                stack.push(target);
                if idx < tokens.len() && tokens[idx].range.start != tokens[idx].range.end {
                    // A real token shifted with the repair spent: the
                    // pending region's parse has resumed here.
                    if queue.is_empty() {
                        if let Some(ri) = pending_region {
                            regions[ri].resync = tokens[idx].range.start;
                            pending_region = None;
                        }
                    }
                }
                idx += 1;
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
                if let Some(ri) = pending_region {
                    regions[ri].resync = source_len;
                }
                break;
            }
            None => {
                // Stuck: spend the next queued op, or report a region.
                if let Some(op) = queue.pop_front() {
                    match op {
                        RepairOp::Insert(look) => {
                            let at = tokens
                                .get(idx)
                                .map_or(source_len, |token| token.range.start);
                            tokens.insert(
                                idx,
                                FixtureToken {
                                    look,
                                    range: at..at,
                                },
                            );
                        }
                        RepairOp::Delete(look) => {
                            if tokens.get(idx).is_some_and(|token| token.look == look) {
                                tokens.remove(idx);
                            } else {
                                queue.clear(); // misaligned script: force next
                            }
                        }
                    }
                    continue;
                }
                let offset = tokens
                    .get(idx)
                    .map_or(source_len, |token| token.range.start);
                // The previous region's parse resumed exactly here.
                if let Some(ri) = pending_region {
                    regions[ri].resync = offset;
                }
                let window: Vec<Look> = tokens[idx..(idx + WINDOW).min(tokens.len())]
                    .iter()
                    .map(|token| token.look)
                    .collect();
                let kind = resolve_kind(diags, &mut matched, offset, &window, &names);
                let plan = if planned_at == Some(idx) {
                    None // same failure twice: the script did not line up
                } else {
                    planner.plan(&stack, &window, BUDGET)
                };
                if let Some(plan) = plan {
                    planned_at = Some(idx);
                    regions.push(TraceRegion {
                        offset,
                        kind,
                        ops: render_ops(&plan.ops, &names),
                        resync: source_len,
                    });
                    pending_region = Some(regions.len() - 1);
                    queue.extend(plan.ops);
                } else {
                    // No plan (budget, repeat, or EOF without one):
                    // force the drive forward by deleting the
                    // offending token — the honest "skip to next
                    // sync" fallback.
                    planned_at = None;
                    if idx < tokens.len() {
                        tokens.remove(idx);
                        regions.push(TraceRegion {
                            offset,
                            kind,
                            ops: Vec::new(),
                            resync: source_len,
                        });
                        pending_region = Some(regions.len() - 1);
                    } else {
                        regions.push(TraceRegion {
                            offset,
                            kind,
                            ops: Vec::new(),
                            resync: source_len,
                        });
                        break; // EOF with nothing left to skip
                    }
                }
            }
        }
    }
    (regions, matched)
}

/// Resolve the region kind: the pack's message when a diagnostic covers
/// this offset — exactly, or inside its `[offset, resume)` error span
/// (the pack's error token often swallows leading whitespace) — and has
/// not been consumed yet; else the unexpected-look fallback.
fn resolve_kind(
    diags: &[PackDiag],
    matched: &mut BTreeSet<usize>,
    offset: usize,
    window: &[Look],
    names: &[(u32, String)],
) -> String {
    let covers =
        |diag: &PackDiag| diag.offset == offset || (diag.offset < offset && offset < diag.resume);
    match diags.iter().position(covers) {
        Some(pos) if !matched.contains(&pos) => {
            matched.insert(pos);
            json_ref::error_message(diags[pos].diag).to_owned()
        }
        _ => match window.first() {
            Some(look) => format!("unexpected {}", look_name(*look, names)),
            None => "unexpected EOF".to_owned(),
        },
    }
}

/// Fixture token names keyed by [`Look`] id (for op + kind printing).
fn token_names(grammar: &Grammar) -> Vec<(u32, String)> {
    grammar
        .tokens
        .iter()
        .map(|token| {
            (
                u32::from(token.id.0),
                grammar.interner.lookup(token.name).to_owned(),
            )
        })
        .collect()
}

/// Render one [`Look`] for display: `EOF` sentinel or the fixture name.
fn look_name(look: Look, names: &[(u32, String)]) -> String {
    if look == Look::EOF {
        return "EOF".to_owned();
    }
    names
        .iter()
        .find(|(id, _)| *id == look.0)
        .map_or_else(|| format!("token#{}", look.0), |(_, name)| name.clone())
}

/// Render the plan's ops as printable trace ops.
fn render_ops(ops: &[RepairOp], names: &[(u32, String)]) -> Vec<TraceOp> {
    ops.iter()
        .map(|op| match op {
            RepairOp::Insert(look) => TraceOp {
                op: "insert",
                token: look_name(*look, names),
            },
            RepairOp::Delete(look) => TraceOp {
                op: "delete",
                token: look_name(*look, names),
            },
        })
        .collect()
}

/// Render the trace as deterministic pretty JSON (fixed field order, no
/// map iteration — byte-identical across runs).
#[must_use]
pub fn render_json(output: &TraceOutput) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"pack\": {},", json_str(&output.pack));
    let _ = writeln!(out, "  \"source_len\": {},", output.source_len);
    let _ = writeln!(out, "  \"ok\": {},", output.ok);
    if output.regions.is_empty() {
        let _ = writeln!(out, "  \"regions\": []");
    } else {
        let _ = writeln!(out, "  \"regions\": [");
        for (index, region) in output.regions.iter().enumerate() {
            let comma = if index + 1 < output.regions.len() {
                ","
            } else {
                ""
            };
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      \"offset\": {},", region.offset);
            let _ = writeln!(out, "      \"kind\": {},", json_str(&region.kind));
            if region.ops.is_empty() {
                let _ = writeln!(out, "      \"ops\": [],");
            } else {
                let _ = writeln!(out, "      \"ops\": [");
                for op in &region.ops {
                    let _ = writeln!(
                        out,
                        "        {{\"op\": \"{}\", \"token\": {}}}",
                        op.op,
                        json_str(&op.token)
                    );
                }
                let _ = writeln!(out, "      ],");
            }
            let _ = writeln!(out, "      \"resync\": {}", region.resync);
            let _ = writeln!(out, "    }}{comma}");
        }
        let _ = writeln!(out, "  ]");
    }
    let _ = writeln!(out, "}}");
    out
}

/// Render the trace as a human-readable listing.
#[must_use]
pub fn render_text(output: &TraceOutput) -> String {
    let mut out = String::new();
    if output.regions.is_empty() {
        let _ = writeln!(
            out,
            "pack {}: {} bytes, no recovery regions — ok",
            output.pack, output.source_len
        );
        return out;
    }
    let _ = writeln!(
        out,
        "pack {}: {} bytes, {} recovery region(s)",
        output.pack,
        output.source_len,
        output.regions.len()
    );
    for (index, region) in output.regions.iter().enumerate() {
        let _ = writeln!(
            out,
            "  region {index}: offset {} kind {}",
            region.offset, region.kind
        );
        if region.ops.is_empty() {
            let _ = writeln!(out, "    repair: (none — resync only)");
        } else {
            for op in &region.ops {
                let _ = writeln!(out, "    repair: {} {}", op.op, op.token);
            }
        }
        let _ = writeln!(out, "    resync: {}", region.resync);
    }
    out
}

/// Quote a string as a JSON string literal with minimal escaping.
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

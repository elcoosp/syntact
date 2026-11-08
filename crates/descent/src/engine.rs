//! The descent engine (issue 023): interprets [`Desc`]riptors over a
//! token feed, emitting the event protocol with the reference contract —
//! clean token ranges, gap-covering trivia records, error nodes with
//! guaranteed progress.

use std::ops::Range;

use runtime_core::ERROR_KIND;
use runtime_core::events::Event;

use crate::desc::{Desc, Pat};

/// The index into the diagnostic side table meaning "syntax error" (the
/// only message the v0 engine emits).
pub const DIAG_SYNTAX: u32 = 0;

/// One mode-hook crossing recorded from the pinned rule hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeCrossing {
    /// The pinned mode id.
    pub mode: u16,
    /// `true` on rule entry (mode entered), `false` on exit.
    pub enter: bool,
    /// The token-feed index at the crossing.
    pub at: usize,
}

/// The output of one engine run — the reference-parser contract.
#[derive(Debug, Clone, Default)]
pub struct Run {
    /// Raw events (no implicit root; wrap before [`crate::green_tree`]).
    pub events: Vec<Event>,
    /// Clean token ranges parallel to token events (DFS order).
    pub tokens: Vec<(u16, Range<u32>)>,
    /// Trivia records covering every gap (`(range, trailing)`).
    pub trivia: Vec<(Range<u32>, bool)>,
    /// Number of syntax errors (each has an `Event::Error`).
    pub errors: usize,
    /// Mode-hook crossings in encounter order.
    pub modes: Vec<ModeCrossing>,
}

/// Interpreter state.
struct Engine<'d, 'f> {
    desc: &'d Desc,
    /// The token feed: `(kind, clean range)`, non-overlapping, ascending.
    feed: &'f [(u16, Range<u32>)],
    source: &'f str,
    /// Cursor into `feed`.
    pos: usize,
    /// End of the last emitted span (trivia fills the gaps).
    last_end: u32,
    run: Run,
}

/// Interpret `desc` over `feed`, producing the full run (events, clean
/// token ranges, trivia records, error count, mode crossings).
///
/// The feed must be non-overlapping ascending clean ranges (a lexgen
/// scanner over non-trivia tokens, or a hand lexer with the same
/// discipline — see `packs_json::reference`).
#[must_use]
pub fn parse_events(desc: &Desc, feed: &[(u16, Range<u32>)], source: &str) -> Run {
    let mut engine = Engine {
        desc,
        feed,
        source,
        pos: 0,
        last_end: 0,
        run: Run::default(),
    };
    let _ = engine.pat(&desc.start, &[]);
    engine.fill_trivia();
    engine.run
}

impl Engine<'_, '_> {
    /// Snapshot for backtracking (`Alt`/`Opt`/`Seq` rollback).
    fn mark(&self) -> (usize, usize, usize, u32) {
        (
            self.pos,
            self.run.events.len(),
            self.run.tokens.len(),
            self.last_end,
        )
    }

    fn reset(&mut self, mark: (usize, usize, usize, u32)) {
        self.pos = mark.0;
        self.run.events.truncate(mark.1);
        self.run.tokens.truncate(mark.2);
        self.last_end = mark.3;
    }

    /// Emit the pending trivia gap `[last_end, next_start)` (if any).
    fn fill_trivia(&mut self) {
        let start = self.last_end;
        let end = self.feed.get(self.pos).map_or(
            u32::try_from(self.source.len()).unwrap_or(u32::MAX),
            |(_, r)| r.start,
        );
        if end > start {
            let text = &self.source[start as usize..end as usize];
            let trailing = !text.contains('\n');
            self.run.trivia.push((start..end, trailing));
        }
    }

    /// Consume feed token `self.pos` as a Token event.
    fn bump(&mut self) -> bool {
        let Some((kind, range)) = self.feed.get(self.pos) else {
            return false;
        };
        self.fill_trivia();
        self.run.events.push(Event::Token {
            kind: *kind,
            n_raw: 1,
        });
        self.run.tokens.push((*kind, range.clone()));
        self.last_end = range.end;
        self.pos += 1;
        true
    }

    /// Recovery (issue 023 v0; calibrated in issue 026): emit an error +
    /// ERROR node wrapping the offending token — skipped when the token
    /// is in `sync` (an enclosing closer) or at EOF. Consuming the token
    /// (when allowed) keeps every error making forward progress.
    fn recover(&mut self, sync: &[u16], depth_budget: &mut u32) {
        self.run.errors += 1;
        self.run.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.run.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        let offending = self.feed.get(self.pos).map(|(kind, _)| *kind);
        match offending {
            Some(kind) if !sync.contains(&kind) => {
                // Consume: progress guarantee for iteration contexts.
                self.bump();
            }
            _ => {
                // EOF or sync-protected: the error node stays empty; the
                // enclosing Rep breaks on the zero-progress iteration.
            }
        }
        self.run.events.push(Event::Finish);
        *depth_budget = depth_budget.saturating_sub(1);
    }

    /// Recovery step for [`Pat::Recover`] (document-level skip): emit an
    /// error + ERROR node around the offending token and consume it.
    /// Silent no-op at EOF — there is nothing left to recover, and the
    /// enclosing `Rep` breaks on the zero-progress iteration instead of
    /// manufacturing an empty error.
    fn recover_step(&mut self) {
        if self.feed.get(self.pos).is_none() {
            return;
        }
        self.run.errors += 1;
        self.run.events.push(Event::Error { diag: DIAG_SYNTAX });
        self.run.events.push(Event::Start {
            kind: ERROR_KIND,
            forward_parent: None,
        });
        self.bump();
        self.run.events.push(Event::Finish);
    }

    /// Interpret a pattern; `true` on match (committed), `false` on
    /// mismatch (rolled back). `sync` governs recovery consumption.
    ///
    /// Commit discipline (the losslessness guarantee): `Seq` keeps its
    /// committed prefix on failure (consumed tokens are never dropped at
    /// a rule boundary); `Alt`/`Opt`/`Rep` roll back failed attempts —
    /// alternatives whose first item can partially match must therefore
    /// be prefix-disjoint (generated grammars dispatch with [`Pat::Peek`]).
    fn pat(&mut self, pat: &Pat, sync: &[u16]) -> bool {
        match pat {
            Pat::Tok(kind) => {
                if self.feed.get(self.pos).is_some_and(|(k, _)| k == kind) {
                    self.bump()
                } else {
                    false
                }
            }
            Pat::Seq(items) => {
                let all_matched = items.iter().all(|item| self.pat(item, sync));
                // Keep the committed prefix even on failure: the rule
                // boundary turns it into the recovery region.
                all_matched
            }
            Pat::Alt(alternatives) => {
                for alternative in alternatives {
                    let mark = self.mark();
                    if self.pat(alternative, sync) {
                        return true;
                    }
                    self.reset(mark);
                }
                false
            }
            Pat::Opt(inner) => {
                let mark = self.mark();
                if !self.pat(inner, sync) {
                    self.reset(mark);
                }
                true
            }
            Pat::Rep(inner) => {
                loop {
                    let mark = self.mark();
                    if !self.pat(inner, sync) {
                        self.reset(mark);
                        break;
                    }
                    // Zero-progress iteration (e.g. a recovery that could
                    // not consume): break instead of spinning.
                    if self.pos == mark.0 {
                        break;
                    }
                }
                true
            }
            Pat::Node(kind, inner) => {
                let mark = self.mark();
                self.run.events.push(Event::Start {
                    kind: *kind,
                    forward_parent: None,
                });
                if self.pat(inner, sync) {
                    self.run.events.push(Event::Finish);
                    true
                } else {
                    self.reset(mark);
                    false
                }
            }
            Pat::Rule(id) => self.rule(*id),
            Pat::Expr(id) => {
                let table = self.desc.expr(*id).clone();
                self.pratt_climb(&table, 0)
            }
            Pat::Peek { tok, pat } => {
                if self.feed.get(self.pos).is_some_and(|(k, _)| k == tok) {
                    self.pat(pat, sync)
                } else {
                    false
                }
            }
            Pat::PeekSet { toks, pat } => {
                if self
                    .feed
                    .get(self.pos)
                    .is_some_and(|(k, _)| toks.contains(k))
                {
                    self.pat(pat, sync)
                } else {
                    false
                }
            }
            Pat::Recover => {
                self.recover_step();
                true
            }
        }
    }

    /// Interpret rule `id`: open its node (+ mode hook), match its body,
    /// recover inside the node on failure — the rule always closes and
    /// the committed token prefix is kept (losslessness).
    fn rule(&mut self, id: u16) -> bool {
        let rule = self.desc.rule(id);
        let (kind, body, sync, hook) = (
            rule.node_kind,
            rule.body.clone(),
            rule.sync.clone(),
            rule.mode_hook,
        );
        if let Some(mode) = hook {
            self.run.modes.push(ModeCrossing {
                mode,
                enter: true,
                at: self.pos,
            });
        }
        self.run.events.push(Event::Start {
            kind,
            forward_parent: None,
        });
        let start_pos = self.pos;
        let start_events = self.run.events.len();
        let mut budget: u32 = 8;
        if !self.pat(&body, &sync) {
            // Recover only when the rule got somewhere: consumed a token
            // or emitted content beyond its own Start. A zero-progress
            // failure (e.g. the trailing Rep iteration at EOF) closes the
            // empty node silently — nothing to recover from.
            if self.pos != start_pos || self.run.events.len() > start_events + 1 {
                self.recover(&sync, &mut budget);
            }
        }
        self.run.events.push(Event::Finish);
        if let Some(mode) = hook {
            self.run.modes.push(ModeCrossing {
                mode,
                enter: false,
                at: self.pos,
            });
        }
        true
    }

    /// Pratt precedence climbing (issue 023): `min_bp` is the minimum
    /// binding power to keep climbing; returns `false` when no atom
    /// matched (rolled back — the caller falls through alternatives).
    fn pratt_climb(&mut self, table: &crate::desc::PrattDesc, min_bp: u8) -> bool {
        let mark = self.mark();
        // The precede point: every op node applied at this level inserts
        // its Start HERE, nesting outward around the current subtree
        // (left-assoc chains become right-nested event Starts with
        // correctly interleaved Finishes).
        let sub_start = self.run.events.len();

        if let Some((open, close)) = table.group {
            if self.feed.get(self.pos).is_some_and(|(k, _)| *k == open) {
                self.bump();
                if !self.pratt_climb(table, 0) {
                    self.reset(mark);
                    return false;
                }
                if !self.pat(&Pat::Tok(close), &[]) {
                    // Unterminated group: error region inside the group.
                    let mut budget: u32 = 4;
                    self.recover(&[], &mut budget);
                }
            } else if !self.climb_primary(table, sub_start) {
                self.reset(mark);
                return false;
            }
        } else if !self.climb_primary(table, sub_start) {
            self.reset(mark);
            return false;
        }

        // Climb: infix binaries and postfix unaries while binding power
        // holds. Each application inserts its node Start at `sub_start`
        // (wrapping the current subtree), bumps the operator, parses the
        // rhs (infix) and closes its own node.
        while let Some((kind, _)) = self.feed.get(self.pos) {
            let kind = *kind;
            if let Some(infix) = table
                .infix
                .iter()
                .find(|i| i.token == kind && i.lbp >= min_bp)
                .copied()
            {
                self.run.events.insert(
                    sub_start,
                    Event::Start {
                        kind: infix.node,
                        forward_parent: None,
                    },
                );
                self.bump();
                if !self.pratt_climb(table, infix.rbp) {
                    // Dangling operator: error region inside the node.
                    let mut budget: u32 = 4;
                    self.recover(&[], &mut budget);
                }
                self.run.events.push(Event::Finish);
            } else if let Some(postfix) = table
                .postfix
                .iter()
                .find(|p| p.token == kind && p.bp >= min_bp)
                .copied()
            {
                self.run.events.insert(
                    sub_start,
                    Event::Start {
                        kind: postfix.node,
                        forward_parent: None,
                    },
                );
                self.bump();
                self.run.events.push(Event::Finish);
            } else {
                break;
            }
        }
        true
    }

    /// The climb primary: dispatch between a prefix mix-in and the bare
    /// atom. A prefix node closes as soon as its operand climb returns —
    /// same-level operators then wrap it via the caller's precede insert
    /// (so `-a + b` is ADD[NEG[a], +, b], never `-a` swallowing `+ b`).
    fn climb_primary(&mut self, table: &crate::desc::PrattDesc, _sub_start: usize) -> bool {
        if let Some(unary) = table
            .prefix
            .iter()
            .find(|u| self.feed.get(self.pos).is_some_and(|(k, _)| *k == u.token))
            .copied()
        {
            self.run.events.push(Event::Start {
                kind: unary.node,
                forward_parent: None,
            });
            self.bump();
            if !self.pratt_climb(table, unary.bp) {
                return false;
            }
            self.run.events.push(Event::Finish);
            true
        } else {
            self.pat(&table.atom, &[])
        }
    }
}

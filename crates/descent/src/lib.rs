//! # descent — descriptor-driven descent + Pratt backend (issue 023, §7.4)
//!
//! A hand-written descent **engine** whose control flow is data: grammars
//! are [`desc::Desc`]riptors (sequences, alternatives, repetition, node
//! wrapping) interpreted over a token feed, emitting the runtime-core
//! 8-byte event protocol. Expressions use the **Pratt** backend
//! (`pratt::PrattDesc`): binding powers + associativity encoded as
//! `(left, right)` pairs, prefix/postfix unary mix-ins.
//!
//! ## Contract (mirrors the packs-json reference, issue 019)
//!
//! - **Events**: [`Event::Start`]/[`Event::Finish`] bracket nodes,
//!   [`Event::Token`] leaves (`n_raw == 1`). Whitespace is *never* a token
//!   event (ADR-008) — it flows through trivia records.
//! - **Ranges**: clean `(kind, range)` pairs parallel to token events,
//!   DFS order; trivia records cover every gap so ranges + trivia
//!   partition `[0, len)`.
//! - **Errors**: [`Event::Error`] + an [`ERROR_KIND`] node wrapping the
//!   offending token (omitted at EOF). Recovery consumes the offending
//!   token unless it is one of the rule's sync kinds; every error
//!   guarantees forward progress.
//! - **Shape**: no implicit root — callers wrap (see [`green_tree`]).
//!
//! ## Mode monomorphization hooks
//!
//! Every [`desc::RuleDesc`] pins an optional `mode_hook: Option<u16>` —
//! the lexer mode (issue 022 [`lexgen::ModeAction`]) the rule's region is
//! scanned in. The engine records hook crossings in [`Run::modes`]; the
//! codegen monomorphizes per pinned mode (one specialization per mode
//! instead of runtime dispatch at hot spots).

pub mod desc;
pub mod engine;

use std::ops::Range;
use std::sync::Arc;

use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

pub use desc::{Desc, InfixDesc, Pat, PrattDesc, RuleDesc, UnaryDesc};
pub use engine::{ModeCrossing, Run, parse_events};

/// Wrap a run's events in one implicit root and drive them through the
/// full tree integration (`process` → `drive_sink` → `TreeBuilder`).
/// Mirrors `packs_json::reference::parse` — no implicit root in the raw
/// stream, exactly one root node in the tree.
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// syntax errors are recovered, not errors.
pub fn green_tree(
    run: &Run,
    source: &str,
    root_kind: u16,
    trivia_kind: u16,
) -> Result<Arc<GreenNode>, String> {
    let mut wrapped = Vec::with_capacity(run.events.len() + 2);
    wrapped.push(Event::Start {
        kind: root_kind,
        forward_parent: None,
    });
    wrapped.extend(run.events.iter().copied());
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped)?;
    let input = Source::from_str_unchecked(source);
    // NodeCache is !Sync by design (single-threaded parser core) — the
    // Arc satisfies the builder's shared-ownership signature.
    #[allow(clippy::arc_with_non_send_sync)]
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &input);
    let records: Vec<TriviaRecord> = run
        .trivia
        .iter()
        .map(|(range, trailing)| TriviaRecord {
            kind: trivia_kind,
            range: range.clone(),
            trailing: *trailing,
        })
        .collect();
    builder.attach_trivia_records(&records);
    drive_sink(
        &output,
        &run.tokens
            .iter()
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>(),
        &mut builder,
    )?;
    builder.finish()
}

/// Validate a run's stream invariants the way consumers will: balanced
/// enter/exit and single-root shape after wrapping, plus full trivia
/// coverage of `[0, len)`.
#[must_use]
pub fn stream_is_balanced(run: &Run, source: &str, root_kind: u16) -> bool {
    let mut wrapped = Vec::with_capacity(run.events.len() + 2);
    wrapped.push(Event::Start {
        kind: root_kind,
        forward_parent: None,
    });
    wrapped.extend(run.events.iter().copied());
    wrapped.push(Event::Finish);
    if events::process(&wrapped).is_err() {
        return false;
    }
    // Trivia + token ranges must partition [0, len): merge and check gaps.
    let mut spans: Vec<Range<u32>> = run
        .tokens
        .iter()
        .map(|(_, r)| r.clone())
        .chain(run.trivia.iter().map(|(r, _)| r.clone()))
        .collect();
    spans.sort_by_key(|r| r.start);
    let mut cursor = 0_u32;
    for span in spans {
        if span.start != cursor {
            return false;
        }
        cursor = cursor.max(span.end);
    }
    cursor == u32::try_from(source.len()).unwrap_or(u32::MAX)
}
